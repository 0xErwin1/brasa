//! Native builtin implementations: every `brasa_bytecode::BUILTINS`
//! entry the code generator can emit, with the messages and failure
//! classes `docs/spec/05-stdlib.md` fixes. Method-style entries dispatch on the receiver's
//! runtime kind; higher-order entries (`map`, `filter`, `each`,
//! `sortBy`) call back into user code through the VM's bounded
//! reentrant loop.

use std::cmp::Ordering;
use std::rc::Rc;

use brasa_runtime::proc_env::{
    env_lookup, merged_env, non_zero_exit_message, run_all, run_command, shell_argv, valid_env_name,
};
use brasa_runtime::table::{OrderedMap, OrderedSet};
use brasa_runtime::{cli_glue, fs_glue, http_glue, io_glue, json_glue, num_glue, time_glue};

use crate::value::{
    ArgsValue, NativeErrorValue, OutputValue, ResponseValue, Value, WalkValue, value_cmp, value_eq,
};
use crate::vm::{ASSERTION_FAILED, INTEGER_OVERFLOW, Signal, Step, Vm, VmResult};

/// The canonical qualified name of the native `string` parse error
/// (mirrors `brasa_resolver::STRING_PARSE_ERROR`).
const STRING_PARSE_ERROR: &str = "string.ParseError";

/// The canonical qualified name of the native `string` regex error
/// (mirrors `brasa_resolver::STRING_REGEX_ERROR`).
const STRING_REGEX_ERROR: &str = "string.RegexError";

/// The canonical qualified name of the native `proc` non-zero-exit
/// error (mirrors `brasa_resolver::PROC_NON_ZERO_EXIT`).
const PROC_NON_ZERO_EXIT: &str = "proc.NonZeroExit";

/// The canonical qualified name of the native `proc` spawn error
/// (mirrors `brasa_resolver::PROC_SPAWN_ERROR`).
const PROC_SPAWN_ERROR: &str = "proc.SpawnError";

/// The canonical qualified name of the native `http` request error
/// (`docs/spec/05-stdlib.md`, BRS-113): a request that never produced a
/// response.
const HTTP_REQUEST_ERROR: &str = "http.RequestError";

/// The canonical qualified name of the native `cli` usage error
/// (`docs/spec/05-stdlib.md`, BRS-112): a command line the declaration
/// does not accept.
const CLI_USAGE_ERROR: &str = "cli.UsageError";

impl Vm<'_> {
    /// Receiver-less builtins: the prelude printers, `std::math`
    /// members, and the internal failure raisers.
    pub(crate) fn free_builtin(&mut self, name: &str, args: Vec<Value>) -> VmResult {
        match name {
            "puts" | "print" => {
                let [value] = args.as_slice() else {
                    return Err(Signal::Fatal(
                        "brasa: `puts`/`print` take exactly 1 argument".to_string(),
                    ));
                };
                let text = self.display(value)?;
                let result = if name == "puts" {
                    writeln!(self.out, "{text}")
                } else {
                    write!(self.out, "{text}")
                };
                match result {
                    Ok(()) => Ok(Value::Unit),
                    // A closed read end (`brasa ... | head`) is not a
                    // program failure: exit silently like Unix tools.
                    Err(err) if err.kind() == std::io::ErrorKind::BrokenPipe => {
                        Err(Signal::BrokenPipe)
                    }
                    Err(err) => Err(Signal::Fatal(format!(
                        "brasa: failed to write output: {err}"
                    ))),
                }
            }
            "<fatal>" => match args.into_iter().next() {
                Some(Value::Str(message)) => Err(Signal::Fatal(message.to_string())),
                _ => unreachable!("<fatal> always receives a message string"),
            },
            "<assert-failed>" => match args.into_iter().next() {
                Some(Value::Str(detail)) => Err(self.panic(ASSERTION_FAILED, detail.to_string())),
                _ => unreachable!("<assert-failed> always receives a detail string"),
            },
            _ => {
                if let Some(member) = name.strip_prefix("math.") {
                    self.math_call(member, args)
                } else if let Some(member) = name.strip_prefix("cli.") {
                    self.cli_call(member, args)
                } else if let Some(member) = name.strip_prefix("http.") {
                    self.http_call(member, args)
                } else if let Some(member) = name.strip_prefix("proc.") {
                    self.proc_call(member, args)
                } else if let Some(member) = name.strip_prefix("env.") {
                    self.env_call(member, args)
                } else if let Some(member) = name.strip_prefix("fs.") {
                    self.fs_call(member, args)
                } else if let Some(member) = name.strip_prefix("json.") {
                    self.json_call(member, args)
                } else if let Some(member) = name.strip_prefix("io.") {
                    self.io_call(member, args)
                } else if let Some(member) = name.strip_prefix("time.") {
                    self.time_call(member, args)
                } else if let Some(member) = name.strip_prefix("rand.") {
                    self.rand_call(member, args)
                } else {
                    unreachable!("unknown free builtin `{name}`")
                }
            }
        }
    }

    /// The `std::proc` runners, ported from the walker's `proc_call`
    /// (BRS-32, `docs/spec/05-stdlib.md`): `run`/`tryRun` take an argv
    /// vector or a whitespace-split string, `shell` runs via
    /// `/bin/sh -c`; every runner accepts an optional trailing stdin
    /// string. `run`/`shell` throw `proc.NonZeroExit` on a non-zero
    /// exit; every runner throws `proc.SpawnError` when the child
    /// cannot start.
    fn proc_call(&mut self, name: &str, args: Vec<Value>) -> VmResult {
        if name == "tryRunAll" {
            return self.proc_try_run_all(args);
        }

        if !matches!(name, "run" | "tryRun" | "shell") {
            return Err(Signal::Fatal(format!(
                "brasa: unknown member `{name}` on module `proc`"
            )));
        }

        let invalid = || Signal::Fatal(format!("brasa: invalid argument(s) to `proc.{name}`"));
        let (cmd, stdin) = match args.as_slice() {
            [cmd] => (cmd, None),
            [cmd, Value::Str(text)] => (cmd, Some(text.clone())),
            _ => return Err(invalid()),
        };

        let (argv, shown) = match (name, cmd) {
            ("shell", Value::Str(line)) => (shell_argv(line), line.to_string()),
            ("shell", _) => return Err(invalid()),
            (_, Value::Str(line)) => {
                let argv: Vec<String> = line.split_whitespace().map(str::to_string).collect();
                let shown = argv.join(" ");
                (argv, shown)
            }
            (_, Value::Vector(items)) => {
                let items = self.heap.vector(*items).borrow().clone();
                let mut argv = Vec::with_capacity(items.len());
                for item in &items {
                    match item {
                        Value::Str(s) => argv.push(s.to_string()),
                        _ => return Err(invalid()),
                    }
                }
                let shown = argv.join(" ");
                (argv, shown)
            }
            _ => return Err(invalid()),
        };

        let output = run_command(&argv, stdin.as_deref(), &self.env_overlay)
            .map_err(|message| native_error(PROC_SPAWN_ERROR, message))?;

        if name != "tryRun" && output.code != 0 {
            let message = non_zero_exit_message(&shown, &output);
            return Err(native_error(PROC_NON_ZERO_EXIT, message));
        }

        Ok(Value::ProcOutput(Rc::new(OutputValue {
            stdout: Rc::from(output.stdout),
            stderr: Rc::from(output.stderr),
            code: output.code,
        })))
    }

    /// The `std::cli` members (`docs/spec/05-stdlib.md`, BRS-112):
    /// `parse(args, spec)` and `help(program, spec)`.
    ///
    /// A malformed DECLARATION is fatal rather than a `cli.UsageError`:
    /// it is the script author's bug, and reporting it as a usage error
    /// would tell the person running the script to fix a command line
    /// that was fine.
    fn cli_call(&mut self, name: &str, args: Vec<Value>) -> VmResult {
        let invalid = || Signal::Fatal(format!("brasa: invalid argument(s) to `cli.{name}`"));

        match (name, args.as_slice()) {
            ("parse", [Value::Vector(argv), Value::Vector(spec)]) => {
                let argv = self.string_vector(*argv).ok_or_else(invalid)?;
                let params = self.params(*spec)?;

                let parsed = cli_glue::parse(&params, &argv)
                    .map_err(|err| native_error(CLI_USAGE_ERROR, err.message))?;

                Ok(Value::CliArgs(Rc::new(ArgsValue {
                    flags: parsed.flags,
                    options: parsed.options,
                    rest: parsed.rest,
                })))
            }
            ("help", [Value::Str(program), Value::Vector(spec)]) => {
                let params = self.params(*spec)?;
                Ok(Value::Str(Rc::from(cli_glue::help(program, &params))))
            }
            ("parse" | "help", _) => Err(invalid()),
            _ => Err(Signal::Fatal(format!(
                "brasa: unknown member `{name}` on module `cli`"
            ))),
        }
    }

    /// A `Vector<string>` argument as plain strings, or `None` when it
    /// holds anything else.
    fn string_vector(&self, vector: crate::heap::GcRef) -> Option<Vec<String>> {
        let items = self.heap.vector(vector).borrow().clone();

        items
            .iter()
            .map(|item| match item {
                Value::Str(s) => Some(s.to_string()),
                _ => None,
            })
            .collect()
    }

    /// The declared parameters behind a `cli` spec argument.
    fn params(&self, spec: crate::heap::GcRef) -> Result<Vec<cli_glue::Param>, Signal> {
        let rows = self.heap.vector(spec).borrow().clone();

        let mut params = Vec::with_capacity(rows.len());
        for row in &rows {
            let Value::Vector(row) = row else {
                return Err(Signal::Fatal(
                    "brasa: a `cli` spec is a Vector of Vector<string>".to_string(),
                ));
            };
            let row = self.string_vector(*row).ok_or_else(|| {
                Signal::Fatal("brasa: a `cli` spec is a Vector of Vector<string>".to_string())
            })?;

            params.push(
                cli_glue::param(&row)
                    .map_err(|message| Signal::Fatal(format!("brasa: {message}")))?,
            );
        }

        Ok(params)
    }

    /// The `std::http` members (`docs/spec/05-stdlib.md`, BRS-113):
    /// `get(url, timeoutMs?)` and `post(url, body, timeoutMs?)`.
    ///
    /// A non-2xx status is an ANSWER and comes back in the `Response`;
    /// only a request that never produced one — DNS, connection, TLS,
    /// timeout — throws `http.RequestError`. That is the same split
    /// `std::proc` draws between a non-zero exit and a `SpawnError`.
    ///
    /// Nothing in the TLS stack initializes before the first call
    /// reaches here, which is what keeps cold start unmoved for the
    /// scripts that never make a request.
    fn http_call(&mut self, name: &str, args: Vec<Value>) -> VmResult {
        let invalid = || Signal::Fatal(format!("brasa: invalid argument(s) to `http.{name}`"));

        let (url, body, timeout) = match (name, args.as_slice()) {
            ("get", [Value::Str(url)]) => (url.to_string(), None, None),
            ("get", [Value::Str(url), Value::Int(ms)]) => (url.to_string(), None, Some(*ms)),
            ("post", [Value::Str(url), Value::Str(body)]) => {
                (url.to_string(), Some(body.to_string()), None)
            }
            ("post", [Value::Str(url), Value::Str(body), Value::Int(ms)]) => {
                (url.to_string(), Some(body.to_string()), Some(*ms))
            }
            ("get" | "post", _) => return Err(invalid()),
            _ => {
                return Err(Signal::Fatal(format!(
                    "brasa: unknown member `{name}` on module `http`"
                )));
            }
        };

        let headers = std::collections::HashMap::new();
        let result = match &body {
            None => http_glue::get(&url, &headers, timeout),
            Some(body) => http_glue::post(&url, body, &headers, timeout),
        };
        let response = result.map_err(|message| native_error(HTTP_REQUEST_ERROR, message))?;

        Ok(Value::HttpResponse(Rc::new(ResponseValue {
            status: response.status,
            body: Rc::from(response.body),
            headers: response.headers,
        })))
    }

    /// The `Args` record's members (BRS-112): `flag(name)`,
    /// `option(name)`, and `rest`.
    ///
    /// Both lookups are total: an undeclared flag is `false` and a
    /// missing option is `None`, answered with `??`. A script asking
    /// about a name it did not declare gets the same answer as one the
    /// user did not pass, which is the only reading that does not need
    /// a second error channel.
    fn args_builtin(&mut self, parsed: &ArgsValue, name: &str, args: &[Value]) -> VmResult {
        match (name, args) {
            ("flag", [Value::Str(wanted)]) => Ok(Value::Bool(
                parsed.flags.iter().any(|flag| flag.as_str() == &**wanted),
            )),
            ("option", [Value::Str(wanted)]) => {
                let found = parsed
                    .options
                    .iter()
                    .find(|(name, _)| name.as_str() == &**wanted)
                    .map(|(_, value)| Value::Str(Rc::from(value.as_str())));

                Ok(Value::Option(found.map(Rc::new)))
            }
            ("rest", []) => {
                let items = parsed
                    .rest
                    .iter()
                    .map(|item| Value::Str(Rc::from(item.as_str())))
                    .collect();

                Ok(self.heap.alloc_vector(items))
            }
            _ => Err(builtin_error(name)),
        }
    }

    /// `proc.tryRunAll(commands, limit?)`: every command run with a
    /// concurrency cap, results in input order.
    ///
    /// Tolerant like `tryRun` and for the same reason, one step
    /// stronger: a batch that aborts on the first non-zero exit has
    /// already paid for the work it then throws away, and the codes are
    /// exactly the data the caller asked for. A child that cannot START
    /// is still `proc.SpawnError` — that is an environment failure, not
    /// a result, and mapping it to some invented exit code would hide
    /// it.
    fn proc_try_run_all(&mut self, args: Vec<Value>) -> VmResult {
        let invalid =
            || Signal::Fatal("brasa: invalid argument(s) to `proc.tryRunAll`".to_string());

        let (commands, limit) = match args.as_slice() {
            [Value::Vector(commands)] => (*commands, None),
            [Value::Vector(commands), Value::Int(limit)] => (*commands, Some(*limit)),
            _ => return Err(invalid()),
        };

        let rows = self.heap.vector(commands).borrow().clone();
        let mut argvs = Vec::with_capacity(rows.len());
        for row in &rows {
            let Value::Vector(items) = row else {
                return Err(invalid());
            };
            let items = self.heap.vector(*items).borrow().clone();

            let mut argv = Vec::with_capacity(items.len());
            for item in &items {
                match item {
                    Value::Str(s) => argv.push(s.to_string()),
                    _ => return Err(invalid()),
                }
            }
            argvs.push(argv);
        }

        // A non-positive cap is clamped by `run_all` rather than
        // rejected: `0` reads as "no limit" to anyone who knows
        // `xargs -P0`, and the honest answer to that is the machine's
        // parallelism, not an unbounded fan-out.
        let limit = limit.map(|n| usize::try_from(n).unwrap_or(1));
        let results = run_all(&argvs, &self.env_overlay, limit);

        let mut outputs = Vec::with_capacity(results.len());
        for result in results {
            let output = result.map_err(|message| native_error(PROC_SPAWN_ERROR, message))?;
            outputs.push(Value::ProcOutput(Rc::new(OutputValue {
                stdout: Rc::from(output.stdout),
                stderr: Rc::from(output.stderr),
                code: output.code,
            })));
        }

        Ok(self.heap.alloc_vector(outputs))
    }

    /// The `std::env` members, ported from the walker's `env_call`
    /// (BRS-32, `docs/spec/05-stdlib.md`): the process environment
    /// merged with the `env.set` overlay.
    fn env_call(&mut self, name: &str, args: Vec<Value>) -> VmResult {
        match (name, args.as_slice()) {
            // A chosen exit is not an error: it unwinds past every
            // handler and the CLI prints nothing
            // (`docs/spec/05-stdlib.md`).
            ("exit", [Value::Int(code)]) => {
                let code = *code;
                if !(0..=255).contains(&code) {
                    return Err(self.panic(
                        ASSERTION_FAILED,
                        format!("`env.exit` takes a status of 0 to 255, got {code}"),
                    ));
                }
                Err(Signal::Exit(code as i32))
            }
            ("get", [Value::Str(key)]) => {
                let value = self
                    .env_overlay
                    .get(key.as_ref())
                    .cloned()
                    .or_else(|| env_lookup(key));
                Ok(match value {
                    Some(value) => Value::some(Value::str(value)),
                    None => Value::NONE,
                })
            }
            ("set", [Value::Str(key), Value::Str(value)]) => {
                if !valid_env_name(key) {
                    return Err(Signal::Fatal(format!(
                        "brasa: invalid environment variable name {:?} in `env.set`",
                        key.as_ref()
                    )));
                }
                self.env_overlay.insert(key.to_string(), value.to_string());
                Ok(Value::Unit)
            }
            ("vars", []) => {
                let entries = merged_env(&self.env_overlay)
                    .into_iter()
                    .map(|(key, value)| (Value::str(key), Value::str(value)))
                    .collect();
                Ok(self
                    .heap
                    .alloc_map(OrderedMap::from_distinct_entries(entries)))
            }
            ("args", []) => {
                let args = self.script_args.iter().map(Value::str).collect();
                Ok(self.heap.alloc_vector(args))
            }
            ("cwd", []) => fs_str(fs_glue::cwd()),
            ("cd", [Value::Str(path)]) => fs_unit(fs_glue::cd(path)),
            ("get" | "set" | "vars" | "args" | "cwd" | "cd", _) => Err(Signal::Fatal(format!(
                "brasa: invalid argument(s) to `env.{name}`"
            ))),
            _ => Err(Signal::Fatal(format!(
                "brasa: unknown member `{name}` on module `env`"
            ))),
        }
    }

    /// The `std::fs` members plus path helpers, ported from the
    /// walker's `fs_call` (BRS-33, `docs/spec/05-stdlib.md`); all OS
    /// behavior lives in the shared `brasa_runtime::fs_glue`, only value
    /// construction happens here.
    fn fs_call(&mut self, name: &str, args: Vec<Value>) -> VmResult {
        match (name, args.as_slice()) {
            ("read", [Value::Str(path)]) => fs_str(fs_glue::read(path)),
            ("write", [Value::Str(path), Value::Str(contents)]) => {
                fs_unit(fs_glue::write(path, contents))
            }
            ("append", [Value::Str(path), Value::Str(contents)]) => {
                fs_unit(fs_glue::append(path, contents))
            }
            ("exists?", [Value::Str(path)]) => Ok(Value::Bool(fs_glue::exists(path))),
            ("isFile?", [Value::Str(path)]) => Ok(Value::Bool(fs_glue::is_file(path))),
            ("isDir?", [Value::Str(path)]) => Ok(Value::Bool(fs_glue::is_dir(path))),
            // The one predicate that must NOT follow the link: it
            // answers about the path, not about its target.
            ("isSymlink?", [Value::Str(path)]) => Ok(Value::Bool(fs_glue::is_symlink(path))),
            ("ls", [Value::Str(path)]) => self.fs_strings(fs_glue::ls(path)),
            ("glob", [Value::Str(pattern)]) => self.fs_strings(fs_glue::glob(pattern)),
            ("walk", [Value::Str(path)]) => self.fs_strings(fs_glue::walk(path, &[])),
            ("walk", [Value::Str(path), Value::Vector(prune)]) => {
                let names = self.prune_names(*prune, "walk")?;
                self.fs_strings(fs_glue::walk(path, &names))
            }
            ("tryWalk", [Value::Str(path)]) => self.fs_walk(fs_glue::try_walk(path, &[])),
            ("tryWalk", [Value::Str(path), Value::Vector(prune)]) => {
                let names = self.prune_names(*prune, "tryWalk")?;
                self.fs_walk(fs_glue::try_walk(path, &names))
            }
            ("mkdir", [Value::Str(path)]) => fs_unit(fs_glue::mkdir(path)),
            ("mkdirAll", [Value::Str(path)]) => fs_unit(fs_glue::mkdir_all(path)),
            ("rm", [Value::Str(path)]) => fs_unit(fs_glue::rm(path)),
            ("rmAll", [Value::Str(path)]) => fs_unit(fs_glue::rm_all(path)),
            ("cp", [Value::Str(from), Value::Str(to)]) => fs_unit(fs_glue::cp(from, to)),
            ("mv", [Value::Str(from), Value::Str(to)]) => fs_unit(fs_glue::mv(from, to)),
            ("join", [Value::Str(base), Value::Str(part)]) => {
                Ok(Value::str(fs_glue::join(base, part)))
            }
            ("base", [Value::Str(path)]) => Ok(Value::str(fs_glue::base(path))),
            ("dir", [Value::Str(path)]) => Ok(Value::str(fs_glue::dir(path))),
            ("ext", [Value::Str(path)]) => Ok(Value::str(fs_glue::ext(path))),
            ("abs", [Value::Str(path)]) => fs_str(fs_glue::abs(path)),
            ("resolve", [Value::Str(path)]) => fs_str(fs_glue::resolve(path)),
            (
                "read" | "write" | "append" | "exists?" | "isFile?" | "isDir?" | "ls" | "glob"
                | "walk" | "tryWalk" | "mkdir" | "mkdirAll" | "rm" | "rmAll" | "cp" | "mv" | "join"
                | "base" | "dir" | "ext" | "abs",
                _,
            ) => Err(Signal::Fatal(format!(
                "brasa: invalid argument(s) to `fs.{name}`"
            ))),
            _ => Err(Signal::Fatal(format!(
                "brasa: unknown member `{name}` on module `fs`"
            ))),
        }
    }

    fn fs_strings(&mut self, result: fs_glue::FsResult<Vec<String>>) -> VmResult {
        let items = result.map_err(fs_signal)?;
        Ok(self
            .heap
            .alloc_vector(items.into_iter().map(Value::str).collect()))
    }

    /// The directory names a `walk`/`tryWalk` prune argument carries.
    fn prune_names(&self, prune: crate::heap::GcRef, member: &str) -> VmResult<Vec<String>> {
        let items = self.heap.vector(prune).borrow().clone();

        let mut names = Vec::with_capacity(items.len());
        for item in &items {
            match item {
                Value::Str(name) => names.push(name.to_string()),
                _ => return Err(builtin_error(member)),
            }
        }

        Ok(names)
    }

    /// Builds the `Walk` record (BRS-66) from what the traversal
    /// reached and what it could not read.
    fn fs_walk(&mut self, result: fs_glue::FsResult<(Vec<String>, Vec<String>)>) -> VmResult {
        let (paths, unreadable) = result.map_err(fs_signal)?;

        let paths = self
            .heap
            .alloc_vector(paths.into_iter().map(Value::str).collect());

        // The first vector is reachable from nothing until the record
        // exists, so it is rooted across the second allocation. Nothing
        // between them can collect today — allocation is not a
        // safepoint — but saying so here is cheaper than a reader
        // having to re-derive it (BRS-62).
        let rooted = [paths.clone()];
        self.with_rooted(&rooted, |this| {
            let unreadable = this
                .heap
                .alloc_vector(unreadable.into_iter().map(Value::str).collect());

            Ok(Value::Walk(Rc::new(WalkValue { paths, unreadable })))
        })
    }

    /// The `std::json` members, ported from the walker's `json_call`
    /// (BRS-34, `docs/spec/05-stdlib.md`); all JSON behavior lives in
    /// the shared `brasa_runtime::json_glue`, only value construction
    /// happens here.
    fn json_call(&mut self, name: &str, args: Vec<Value>) -> VmResult {
        match (name, args.as_slice()) {
            ("parse", [Value::Str(text)]) => match json_glue::parse(text) {
                Ok(tree) => Ok(Value::Json(tree)),
                Err(err) => Err(native_error(err.name, err.message)),
            },
            ("stringify", [Value::Json(tree)]) => Ok(Value::str(json_glue::stringify(tree))),
            ("parse" | "stringify", _) => Err(Signal::Fatal(format!(
                "brasa: invalid argument(s) to `json.{name}`"
            ))),
            _ => Err(Signal::Fatal(format!(
                "brasa: unknown member `{name}` on module `json`"
            ))),
        }
    }

    /// The `std::io` members, ported from the walker's `io_call`
    /// (BRS-34, `docs/spec/05-stdlib.md`): `puts`/`print` mirror the
    /// prelude printers, `eprint` writes to the run's error stream, and
    /// the readers consume the run's input stream through the shared
    /// `brasa_runtime::io_glue`.
    fn io_call(&mut self, name: &str, args: Vec<Value>) -> VmResult {
        match (name, args.as_slice()) {
            ("puts" | "print" | "eprint", [value]) => {
                let value = value.clone();
                let text = self.display(&value)?;
                self.write_io(name, &text)
            }
            ("readLine", []) => Ok(match io_glue::read_line(self.input) {
                Some(line) => Value::some(Value::str(line)),
                None => Value::NONE,
            }),
            ("readAll", []) => Ok(Value::str(io_glue::read_all(self.input))),
            ("puts" | "print" | "eprint" | "readLine" | "readAll", _) => Err(Signal::Fatal(
                format!("brasa: invalid argument(s) to `io.{name}`"),
            )),
            _ => Err(Signal::Fatal(format!(
                "brasa: unknown member `{name}` on module `io`"
            ))),
        }
    }

    /// One printer write: `puts` appends a newline, `eprint` targets
    /// stderr. A closed read end is a silent exit on every stream,
    /// like the prelude printers.
    fn write_io(&mut self, name: &str, text: &str) -> VmResult {
        let result = match name {
            "puts" => writeln!(self.out, "{text}"),
            "print" => write!(self.out, "{text}"),
            _ => write!(self.err, "{text}"),
        };

        match result {
            Ok(()) => Ok(Value::Unit),
            Err(err) if err.kind() == std::io::ErrorKind::BrokenPipe => Err(Signal::BrokenPipe),
            Err(err) => Err(Signal::Fatal(format!(
                "brasa: failed to write output: {err}"
            ))),
        }
    }

    /// The `Json` accessors (BRS-34, `docs/spec/05-stdlib.md`), pure
    /// over the shared tree; `None` means the name is not a `Json`
    /// builtin (the caller reports it).
    fn json_builtin(
        &mut self,
        tree: &json_glue::JsonValue,
        name: &str,
        args: &[Value],
    ) -> Option<Value> {
        if !args.is_empty() {
            return None;
        }

        let some_or_none = |value: Option<Value>| value.map(Value::some).unwrap_or(Value::NONE);

        Some(match name {
            "asString" => some_or_none(json_glue::as_string(tree).map(Value::str)),
            "asInt" => some_or_none(json_glue::as_int(tree).map(Value::Int)),
            "asFloat" => some_or_none(json_glue::as_float(tree).map(Value::Float)),
            "asBool" => some_or_none(json_glue::as_bool(tree).map(Value::Bool)),
            "asArray" => some_or_none(json_glue::as_array(tree).map(|items| {
                self.heap
                    .alloc_vector(items.into_iter().map(Value::Json).collect())
            })),
            "asObject" => some_or_none(json_glue::as_object(tree).map(|members| {
                let entries = members
                    .into_iter()
                    .map(|(key, member)| (Value::str(key), Value::Json(member)))
                    .collect();
                self.heap
                    .alloc_map(OrderedMap::from_distinct_entries(entries))
            })),
            "null?" => Value::Bool(json_glue::is_null(tree)),
            _ => return None,
        })
    }

    /// The `Json` accessors on an `Option<Json>` receiver: `Some`
    /// unwraps and delegates, `None` propagates — except `null?`,
    /// which is `false` (an absent member is not an explicit JSON
    /// `null`).
    fn json_option_builtin(
        &mut self,
        inner: Option<&Value>,
        name: &str,
        args: &[Value],
    ) -> Option<Value> {
        match inner {
            Some(Value::Json(tree)) => {
                let tree = tree.clone();
                self.json_builtin(&tree, name, args)
            }
            None if args.is_empty() => match name {
                "null?" => Some(Value::Bool(false)),
                "asString" | "asInt" | "asFloat" | "asBool" | "asArray" | "asObject" => {
                    Some(Value::NONE)
                }
                _ => None,
            },
            _ => None,
        }
    }

    /// The `std::math` members (closed in BRS-35): f64 semantics
    /// throughout; `abs`, `min`, and `max` are polymorphic over ints
    /// and floats. The constants `pi`/`e` arrive here through the
    /// module field-read path with zero arguments.
    fn math_call(&mut self, name: &str, args: Vec<Value>) -> VmResult {
        match (name, args.as_slice()) {
            ("pi", []) => Ok(Value::Float(std::f64::consts::PI)),
            ("e", []) => Ok(Value::Float(std::f64::consts::E)),
            ("sqrt", [Value::Float(v)]) => Ok(Value::Float(v.sqrt())),
            ("floor", [Value::Float(v)]) => Ok(Value::Float(v.floor())),
            ("ceil", [Value::Float(v)]) => Ok(Value::Float(v.ceil())),
            ("round", [Value::Float(v)]) => Ok(Value::Float(v.round())),
            ("pow", [Value::Float(a), Value::Float(b)]) => Ok(Value::Float(a.powf(*b))),
            ("abs", [Value::Float(v)]) => Ok(Value::Float(v.abs())),
            ("abs", [Value::Int(v)]) => v
                .checked_abs()
                .map(Value::Int)
                .ok_or_else(|| self.panic(INTEGER_OVERFLOW, "integer overflow in `math.abs`")),
            ("min", [Value::Int(a), Value::Int(b)]) => Ok(Value::Int((*a).min(*b))),
            ("max", [Value::Int(a), Value::Int(b)]) => Ok(Value::Int((*a).max(*b))),
            ("min", [Value::Float(a), Value::Float(b)]) => Ok(Value::Float(a.min(*b))),
            ("max", [Value::Float(a), Value::Float(b)]) => Ok(Value::Float(a.max(*b))),
            (
                "pi" | "e" | "sqrt" | "floor" | "ceil" | "round" | "pow" | "abs" | "min" | "max",
                _,
            ) => Err(Signal::Fatal(format!(
                "brasa: invalid argument(s) to `math.{name}`"
            ))),
            _ => Err(Signal::Fatal(format!(
                "brasa: unknown member `{name}` on module `math`"
            ))),
        }
    }

    /// The `std::time` members (BRS-35), ported from the walker's
    /// `time_call`; all clock and formatting behavior lives in the
    /// shared `brasa_runtime::time_glue`. A negative `sleep` duration
    /// panics with `panics.AssertionFailed`.
    fn time_call(&mut self, name: &str, args: Vec<Value>) -> VmResult {
        match (name, args.as_slice()) {
            ("now", []) => Ok(Value::Float(time_glue::now_seconds())),
            ("nowMillis", []) => Ok(Value::Int(time_glue::now_millis())),
            ("sleep", [Value::Int(ms)]) => {
                if *ms < 0 {
                    return Err(self.panic(
                        ASSERTION_FAILED,
                        format!("cannot sleep a negative duration ({ms} ms)"),
                    ));
                }
                time_glue::sleep_ms(*ms as u64);
                Ok(Value::Unit)
            }
            ("iso", [Value::Int(millis)]) => Ok(Value::str(time_glue::iso_utc(*millis))),
            ("now" | "nowMillis" | "sleep" | "iso", _) => Err(Signal::Fatal(format!(
                "brasa: invalid argument(s) to `time.{name}`"
            ))),
            _ => Err(Signal::Fatal(format!(
                "brasa: unknown member `{name}` on module `time`"
            ))),
        }
    }

    /// The `std::rand` members (BRS-35), ported from the walker's
    /// `rand_call` and backed by the same shared PRNG
    /// (`brasa_runtime::rand_glue`). Picking from an empty range or
    /// vector panics with `panics.AssertionFailed`; `shuffle` returns
    /// a NEW vector.
    fn rand_call(&mut self, name: &str, args: Vec<Value>) -> VmResult {
        match (name, args.as_slice()) {
            ("seed", [Value::Int(n)]) => {
                self.rng = brasa_runtime::rand_glue::Rng::seeded(*n as u64);
                Ok(Value::Unit)
            }
            ("int", [Value::Range { lo, hi, inclusive }]) => {
                match self.rng.int_in(*lo, *hi, *inclusive) {
                    Some(value) => Ok(Value::Int(value)),
                    None => Err(self.panic(ASSERTION_FAILED, "cannot pick from an empty range")),
                }
            }
            ("float", []) => Ok(Value::Float(self.rng.float())),
            ("choice", [Value::Vector(items)]) => {
                let items = self.heap.vector(*items).borrow();
                if items.is_empty() {
                    return Err(self.panic(ASSERTION_FAILED, "cannot pick from an empty vector"));
                }
                let index = self.rng.below(items.len() as u64) as usize;
                Ok(items[index].clone())
            }
            ("shuffle", [Value::Vector(items)]) => {
                let mut shuffled = self.heap.vector(*items).borrow().clone();
                self.rng.shuffle(&mut shuffled);
                Ok(self.heap.alloc_vector(shuffled))
            }
            ("seed" | "int" | "float" | "choice" | "shuffle", _) => Err(Signal::Fatal(format!(
                "brasa: invalid argument(s) to `rand.{name}`"
            ))),
            _ => Err(Signal::Fatal(format!(
                "brasa: unknown member `{name}` on module `rand`"
            ))),
        }
    }

    /// Method-style builtins, dispatched on the receiver's runtime
    /// kind.
    pub(crate) fn method_builtin(&mut self, name: &str, recv: Value, args: Vec<Value>) -> VmResult {
        // The universal derived `toString` applies to every type; a
        // struct's own method wins inside `display` via the shape.
        if name == "toString" && args.is_empty() && !matches!(recv, Value::Int(_) | Value::Float(_))
        {
            let text = self.display(&recv)?;
            return Ok(Value::str(text));
        }

        match &recv {
            Value::Int(v) => self.int_builtin(*v, name, &args),
            Value::Float(v) => self.float_builtin(*v, name, &args),
            Value::Str(s) => {
                let s = s.clone();
                self.string_builtin(&s, name, &args)
            }
            Value::Vector(_) => self.vector_builtin(&recv, name, args),
            Value::Map(_) => self.map_builtin(&recv, name, &args),
            Value::Set(_) => self.set_builtin(&recv, name, &args),
            Value::ProcOutput(output) => {
                let output = output.clone();
                proc_output_builtin(&output, name, &args)
            }
            Value::HttpResponse(response) => {
                let response = response.clone();
                response_builtin(&response, name, &args)
            }
            Value::CliArgs(parsed) => {
                let parsed = parsed.clone();
                self.args_builtin(&parsed, name, &args)
            }
            Value::Walk(walk) => {
                let walk = walk.clone();
                walk_builtin(&walk, name, &args)
            }
            Value::Json(tree) => {
                let tree = tree.clone();
                self.json_builtin(&tree, name, &args)
                    .ok_or_else(|| builtin_error(name))
            }
            // The `Json` accessors flatten through `Option<Json>`
            // (BRS-34, `docs/spec/05-stdlib.md`): `None` propagates,
            // except `null?`, which is `false` — absent is not `null`.
            Value::Option(inner) => {
                let inner = inner.clone();
                self.json_option_builtin(inner.as_deref(), name, &args)
                    .ok_or_else(|| builtin_error(name))
            }
            _ => Err(builtin_error(name)),
        }
    }

    fn int_builtin(&mut self, v: i64, name: &str, args: &[Value]) -> VmResult {
        match (name, args) {
            ("toFloat", []) => Ok(Value::Float(v as f64)),
            ("toFixed", [Value::Int(digits)]) => {
                let digits = *digits;
                self.check_fixed_digits(digits)?;
                Ok(Value::str(num_glue::int_to_fixed(v, digits)))
            }
            ("toString", []) => Ok(Value::str(v.to_string())),
            _ => Err(builtin_error(name)),
        }
    }

    /// `toFixed` asks for a decimal count a `f64` can back; anything
    /// else is a programmer error, so it panics rather than throwing —
    /// the same rule `time.sleep` and `rand.int` follow for arguments
    /// outside their domain.
    fn check_fixed_digits(&self, digits: i64) -> Result<(), Signal> {
        if num_glue::digits_in_range(digits) {
            return Ok(());
        }
        Err(self.panic(
            ASSERTION_FAILED,
            format!(
                "`toFixed` takes 0 to {} digits, got {digits}",
                num_glue::MAX_DIGITS
            ),
        ))
    }

    fn float_builtin(&mut self, v: f64, name: &str, args: &[Value]) -> VmResult {
        match (name, args) {
            ("toInt", []) => Ok(Value::Int(v as i64)),
            ("toFixed", [Value::Int(digits)]) => {
                let digits = *digits;
                self.check_fixed_digits(digits)?;
                Ok(Value::str(num_glue::float_to_fixed(v, digits)))
            }
            ("toString", []) => {
                let text = self.display(&Value::Float(v))?;
                Ok(Value::str(text))
            }
            _ => Err(builtin_error(name)),
        }
    }

    /// Compiles `pattern` through the per-run cache; an invalid pattern
    /// throws the native `string.RegexError`, message included.
    ///
    /// Handed back as an `Rc` rather than by value. `regex::Regex` is
    /// `Clone`, but a clone carries its own lazy-DFA cache, so every
    /// call was rebuilding the automaton it had just built — 37% of a
    /// real script's run time went into `init_cache` and
    /// `cache_next_state`. Sharing one instance is what makes the cache
    /// a cache.
    fn compile_regex(&mut self, pattern: &str) -> Result<Rc<regex::Regex>, Signal> {
        if let Some(re) = self.regex_cache.get(pattern) {
            return Ok(re.clone());
        }

        let re =
            Rc::new(regex::Regex::new(pattern).map_err(|_| {
                native_error(STRING_REGEX_ERROR, format!("invalid regex {pattern:?}"))
            })?);
        self.regex_cache.insert(pattern.to_string(), re.clone());
        Ok(re)
    }

    fn string_builtin(&mut self, s: &str, name: &str, args: &[Value]) -> VmResult {
        match (name, args) {
            ("len", []) => Ok(Value::Int(s.chars().count() as i64)),
            ("count", [Value::Str(needle)]) => {
                if needle.is_empty() {
                    return Ok(Value::Int(0));
                }
                Ok(Value::Int(s.matches(needle.as_ref()).count() as i64))
            }
            ("trim", []) => Ok(Value::str(s.trim())),
            ("trimStart", []) => Ok(Value::str(s.trim_start())),
            ("trimEnd", []) => Ok(Value::str(s.trim_end())),
            ("reverse", []) => Ok(Value::str(s.chars().rev().collect::<String>())),
            ("toUpper", []) => Ok(Value::str(s.to_uppercase())),
            ("toLower", []) => Ok(Value::str(s.to_lowercase())),
            ("contains?", [Value::Str(needle)]) => Ok(Value::Bool(s.contains(needle.as_ref()))),
            ("startsWith?", [Value::Str(prefix)]) => {
                Ok(Value::Bool(s.starts_with(prefix.as_ref())))
            }
            ("endsWith?", [Value::Str(suffix)]) => Ok(Value::Bool(s.ends_with(suffix.as_ref()))),
            ("split", [Value::Str(sep)]) => {
                let parts: Vec<Value> = if sep.is_empty() {
                    s.chars().map(|c| Value::str(c.to_string())).collect()
                } else {
                    s.split(sep.as_ref()).map(Value::str).collect()
                };
                Ok(self.heap.alloc_vector(parts))
            }
            ("lines", []) => Ok(self.heap.alloc_vector(s.lines().map(Value::str).collect())),
            ("chars", []) => Ok(self.heap.alloc_vector(s.chars().map(Value::Char).collect())),
            ("bytes", []) => Ok(self
                .heap
                .alloc_vector(s.bytes().map(|b| Value::Int(b as i64)).collect())),
            ("slice", [Value::Int(from), Value::Int(to)]) => {
                let len = s.chars().count() as i64;
                let from = (*from).clamp(0, len) as usize;
                let to = (*to).clamp(0, len) as usize;
                if from >= to {
                    return Ok(Value::str(""));
                }
                let text: String = s.chars().skip(from).take(to - from).collect();
                Ok(Value::str(text))
            }
            ("repeat", [Value::Int(n)]) => {
                if *n <= 0 {
                    return Ok(Value::str(""));
                }
                Ok(Value::str(s.repeat(*n as usize)))
            }
            ("padStart" | "padEnd", [Value::Int(width), Value::Str(pad)]) => {
                let len = s.chars().count();
                if *width <= len as i64 || pad.is_empty() {
                    return Ok(Value::str(s));
                }

                let missing = *width as usize - len;
                let filler: String = pad.chars().cycle().take(missing).collect();
                let text = if name == "padStart" {
                    format!("{filler}{s}")
                } else {
                    format!("{s}{filler}")
                };
                Ok(Value::str(text))
            }
            ("replace", [Value::Str(from), Value::Str(to)]) => {
                Ok(Value::str(s.replace(from.as_ref(), to.as_ref())))
            }
            ("match?", [Value::Str(pattern)]) => {
                let re = self.compile_regex(pattern)?;
                Ok(Value::Bool(re.is_match(s)))
            }
            ("captures", [Value::Str(pattern)]) => {
                let re = self.compile_regex(pattern)?;
                match re.captures(s) {
                    Some(caps) => {
                        let groups: Vec<Value> = caps
                            .iter()
                            .map(|group| Value::str(group.map_or("", |m| m.as_str())))
                            .collect();
                        Ok(Value::some(self.heap.alloc_vector(groups)))
                    }
                    None => Ok(Value::NONE),
                }
            }
            ("replaceRe", [Value::Str(pattern), Value::Str(with)]) => {
                let re = self.compile_regex(pattern)?;
                Ok(Value::str(re.replace_all(s, with.as_ref())))
            }
            ("scan", [Value::Str(pattern)]) => {
                let re = self.compile_regex(pattern)?;
                let matches = re.find_iter(s).map(|m| Value::str(m.as_str())).collect();
                Ok(self.heap.alloc_vector(matches))
            }
            ("find", [Value::Str(needle)]) => match s.find(needle.as_ref()) {
                Some(byte_index) => {
                    let char_index = s[..byte_index].chars().count() as i64;
                    Ok(Value::some(Value::Int(char_index)))
                }
                None => Ok(Value::NONE),
            },
            ("toInt", []) => s.parse::<i64>().map(Value::Int).map_err(|_| {
                native_error(STRING_PARSE_ERROR, format!("cannot parse {s:?} as int"))
            }),
            ("toFloat", []) => s.parse::<f64>().map(Value::Float).map_err(|_| {
                native_error(STRING_PARSE_ERROR, format!("cannot parse {s:?} as float"))
            }),
            _ => Err(builtin_error(name)),
        }
    }

    fn vector_builtin(&mut self, recv: &Value, name: &str, args: Vec<Value>) -> VmResult {
        let Value::Vector(items) = recv else {
            return Err(builtin_error(name));
        };
        let items = *items;

        match (name, args.as_slice()) {
            ("len", []) => Ok(Value::Int(self.heap.vector(items).borrow().len() as i64)),
            ("push", [value]) => {
                self.heap
                    .edit_vector(items, |items| items.push(value.clone()));
                Ok(Value::Unit)
            }
            ("pop", []) => Ok(match self.heap.edit_vector(items, Vec::pop) {
                Some(value) => Value::some(value),
                None => Value::NONE,
            }),
            ("first", []) => Ok(self
                .heap
                .vector(items)
                .borrow()
                .first()
                .map(|v| Value::some(v.clone()))
                .unwrap_or(Value::NONE)),
            ("last", []) => Ok(self
                .heap
                .vector(items)
                .borrow()
                .last()
                .map(|v| Value::some(v.clone()))
                .unwrap_or(Value::NONE)),
            ("reverse", []) => {
                let mut reversed = self.heap.vector(items).borrow().clone();
                reversed.reverse();
                Ok(self.heap.alloc_vector(reversed))
            }
            ("contains?", [value]) => Ok(Value::Bool(
                self.heap
                    .vector(items)
                    .borrow()
                    .iter()
                    .any(|v| value_eq(&self.heap, v, value)),
            )),
            ("join", [Value::Str(sep)]) => {
                let items = self.heap.vector(items).borrow().clone();
                let mut parts = Vec::with_capacity(items.len());
                for item in &items {
                    match item {
                        Value::Str(s) => parts.push(s.to_string()),
                        _ => {
                            return Err(Signal::Fatal(
                                "brasa: `join` requires a `Vector<string>`".to_string(),
                            ));
                        }
                    }
                }
                Ok(Value::str(parts.join(sep)))
            }
            ("map", [f]) => {
                let snapshot = self.heap.vector(items).borrow().clone();
                let f = f.clone();
                let mapped = self.collect_rooted(recv, snapshot, |this, item| {
                    this.call_callable(f.clone(), vec![item]).map(Some)
                })?;
                Ok(self.heap.alloc_vector(mapped))
            }
            ("filter", [f]) => {
                let snapshot = self.heap.vector(items).borrow().clone();
                let f = f.clone();
                let kept = self.collect_rooted(recv, snapshot, |this, item| {
                    match this.call_callable(f.clone(), vec![item.clone()])? {
                        Value::Bool(true) => Ok(Some(item)),
                        Value::Bool(false) => Ok(None),
                        _ => Err(Signal::Fatal(
                            "brasa: `filter` predicate must return a bool".to_string(),
                        )),
                    }
                })?;
                Ok(self.heap.alloc_vector(kept))
            }
            ("each", [f]) => {
                let snapshot = self.heap.vector(items).borrow().clone();
                let f = f.clone();
                self.collect_rooted(recv, snapshot, |this, item| {
                    this.call_callable(f.clone(), vec![item])?;
                    Ok(None)
                })?;
                Ok(Value::Unit)
            }
            ("sortBy", [f]) => {
                let snapshot = self.heap.vector(items).borrow().clone();
                self.sort_by(recv, snapshot, f.clone())
            }
            ("sort", []) => {
                let snapshot = self.heap.vector(items).borrow().clone();
                self.sort_natural(snapshot)
            }
            ("reduce", [init, f]) => {
                let snapshot = self.heap.vector(items).borrow().clone();
                let (init, f) = (init.clone(), f.clone());
                self.fold_rooted(recv, snapshot, init, |this, acc, item| {
                    this.call_callable(f.clone(), vec![acc, item])
                })
            }
            ("find", [f]) => {
                let snapshot = self.heap.vector(items).borrow().clone();
                let f = f.clone();
                let found = self.find_rooted(recv, snapshot, |this, item| {
                    match this.call_callable(f.clone(), vec![item.clone()])? {
                        Value::Bool(true) => Ok(Step::Stop(Value::some(item))),
                        Value::Bool(false) => Ok(Step::Continue),
                        _ => Err(Signal::Fatal(
                            "brasa: `find` predicate must return a bool".to_string(),
                        )),
                    }
                })?;
                Ok(found.unwrap_or(Value::NONE))
            }
            // `any?` short-circuits on the first `true`, `all?` on the
            // first `false`; the empty vector is `false`/`true`.
            ("any?" | "all?", [f]) => {
                let deciding = name == "any?";
                let snapshot = self.heap.vector(items).borrow().clone();
                let f = f.clone();
                let found = self.find_rooted(recv, snapshot, |this, item| {
                    match this.call_callable(f.clone(), vec![item])? {
                        Value::Bool(found) if found == deciding => {
                            Ok(Step::Stop(Value::Bool(deciding)))
                        }
                        Value::Bool(_) => Ok(Step::Continue),
                        _ => Err(Signal::Fatal(format!(
                            "brasa: `{name}` predicate must return a bool"
                        ))),
                    }
                })?;
                Ok(found.unwrap_or(Value::Bool(!deciding)))
            }
            // Pairs up to the shorter length; the leftovers of the
            // longer vector are dropped.
            ("zip", [Value::Vector(other)]) => {
                let left = self.heap.vector(items).borrow().clone();
                let right = self.heap.vector(*other).borrow().clone();
                let pairs = left
                    .into_iter()
                    .zip(right)
                    .map(|(a, b)| Value::Tuple(Rc::from(vec![a, b])))
                    .collect();
                Ok(self.heap.alloc_vector(pairs))
            }
            ("flatten", []) => {
                let snapshot = self.heap.vector(items).borrow().clone();
                let mut flat = Vec::new();
                for item in snapshot {
                    match item {
                        Value::Vector(inner) => {
                            flat.extend(self.heap.vector(inner).borrow().iter().cloned())
                        }
                        _ => {
                            return Err(Signal::Fatal(
                                "brasa: `flatten` requires a `Vector<Vector<...>>`".to_string(),
                            ));
                        }
                    }
                }
                Ok(self.heap.alloc_vector(flat))
            }
            // Structural equality, first occurrence kept, insertion
            // order preserved — the `Set` constructor's dedup rule.
            ("uniq", []) => {
                let snapshot = self.heap.vector(items).borrow().clone();
                let mut unique: Vec<Value> = Vec::new();
                for item in snapshot {
                    if !unique.iter().any(|seen| value_eq(&self.heap, seen, &item)) {
                        unique.push(item);
                    }
                }
                Ok(self.heap.alloc_vector(unique))
            }
            _ => Err(builtin_error(name)),
        }
    }

    /// `sort` in natural ascending order: the elements must satisfy the
    /// same orderable rule as `sortBy` keys, NaN panic included
    /// (BRS-35, `docs/spec/05-stdlib.md`).
    fn sort_natural(&mut self, items: Vec<Value>) -> VmResult {
        for item in &items {
            match item {
                Value::Float(v) if v.is_nan() => {
                    return Err(self.panic(
                        ASSERTION_FAILED,
                        "cannot sort a NaN element (floats with NaN do not order)",
                    ));
                }
                Value::Int(_) | Value::Float(_) | Value::Str(_) | Value::Char(_) => {}
                _ => {
                    return Err(Signal::Fatal(
                        "brasa: `sort` elements must be ints, floats, strings, or chars"
                            .to_string(),
                    ));
                }
            }
        }

        let mut sorted = items;
        sorted.sort_by(|a, b| value_cmp(a, b).unwrap_or(Ordering::Equal));
        Ok(self.heap.alloc_vector(sorted))
    }

    fn sort_by(&mut self, recv: &Value, items: Vec<Value>, f: Value) -> VmResult {
        let mut keyed = self.key_rooted(recv, items, |this, item| {
            let key = this.call_callable(f.clone(), vec![item.clone()])?;
            match &key {
                Value::Float(v) if v.is_nan() => Err(this.panic(
                    ASSERTION_FAILED,
                    "cannot sort by a NaN key (floats with NaN do not order)",
                )),
                Value::Int(_) | Value::Float(_) | Value::Str(_) | Value::Char(_) => Ok(key),
                _ => Err(Signal::Fatal(
                    "brasa: `sortBy` key must be an int, float, string, or char".to_string(),
                )),
            }
        })?;

        keyed.sort_by(|(a, _), (b, _)| value_cmp(a, b).unwrap_or(Ordering::Equal));
        Ok(self
            .heap
            .alloc_vector(keyed.into_iter().map(|(_, item)| item).collect()))
    }

    fn map_builtin(&mut self, recv: &Value, name: &str, args: &[Value]) -> VmResult {
        let Value::Map(entries) = recv else {
            return Err(builtin_error(name));
        };
        let entries = *entries;

        match (name, args) {
            ("len", []) => Ok(Value::Int(self.heap.map(entries).borrow().len() as i64)),
            ("keys", []) => {
                let keys = self
                    .heap
                    .map(entries)
                    .borrow()
                    .iter()
                    .map(|(k, _)| k.clone())
                    .collect();
                Ok(self.heap.alloc_vector(keys))
            }
            ("values", []) => {
                let values = self
                    .heap
                    .map(entries)
                    .borrow()
                    .iter()
                    .map(|(_, v)| v.clone())
                    .collect();
                Ok(self.heap.alloc_vector(values))
            }
            ("insert", [key, value]) => {
                self.heap.edit_map(entries, |entries| {
                    entries.insert(key.clone(), value.clone(), |a, b| {
                        value_eq(&self.heap, a, b)
                    });
                });
                Ok(Value::Unit)
            }
            ("remove", [key]) => Ok(self
                .heap
                .edit_map(entries, |entries| {
                    entries.remove(key, |a, b| value_eq(&self.heap, a, b))
                })
                .map(Value::some)
                .unwrap_or(Value::NONE)),
            ("has?", [key]) => Ok(Value::Bool(
                self.heap
                    .map(entries)
                    .borrow()
                    .contains_key(key, |a, b| value_eq(&self.heap, a, b)),
            )),
            ("get", [key]) => Ok(self
                .heap
                .map(entries)
                .borrow()
                .get(key, |a, b| value_eq(&self.heap, a, b))
                .map(|v| Value::some(v.clone()))
                .unwrap_or(Value::NONE)),
            ("entries", []) => {
                let pairs = self
                    .heap
                    .map(entries)
                    .borrow()
                    .iter()
                    .map(|(k, v)| Value::Tuple(Rc::from(vec![k.clone(), v.clone()])))
                    .collect();
                Ok(self.heap.alloc_vector(pairs))
            }
            // A NEW map: the receiver's entries, then the argument's,
            // with the argument winning on duplicate keys; neither
            // operand is modified.
            ("merge", [Value::Map(other)]) => {
                let mut merged = self.heap.map(entries).borrow().clone();
                let additions = self.heap.map(*other).borrow().entries().to_vec();
                for (key, value) in additions {
                    merged.insert(key, value, |a, b| value_eq(&self.heap, a, b));
                }
                Ok(self.heap.alloc_map(merged))
            }
            ("each", [f]) => {
                let snapshot = self.heap.map(entries).borrow().entries().to_vec();
                let f = f.clone();
                self.each_pair_rooted(recv, snapshot, |this, key, value| {
                    this.call_callable(f.clone(), vec![key, value])?;
                    Ok(())
                })?;
                Ok(Value::Unit)
            }
            _ => Err(builtin_error(name)),
        }
    }

    fn set_builtin(&mut self, recv: &Value, name: &str, args: &[Value]) -> VmResult {
        let Value::Set(items) = recv else {
            return Err(builtin_error(name));
        };
        let items = *items;

        match (name, args) {
            ("len", []) => Ok(Value::Int(self.heap.set(items).borrow().len() as i64)),
            ("add", [value]) => {
                self.heap.edit_set(items, |items| {
                    items.add(value.clone(), |a, b| value_eq(&self.heap, a, b));
                });
                Ok(Value::Unit)
            }
            ("remove", [value]) => Ok(Value::Bool(self.heap.edit_set(items, |items| {
                items.remove(value, |a, b| value_eq(&self.heap, a, b))
            }))),
            ("has?", [value]) => Ok(Value::Bool(
                self.heap
                    .set(items)
                    .borrow()
                    .contains(value, |a, b| value_eq(&self.heap, a, b)),
            )),
            // The algebra members return NEW sets in the receiver's
            // insertion order (`union` appends the argument's unseen
            // elements in its order); neither operand is modified.
            ("union", [Value::Set(other)]) => {
                let mut result = self.heap.set(items).borrow().clone();
                let additions = self.heap.set(*other).borrow().items().to_vec();
                for value in additions {
                    result.add(value, |a, b| value_eq(&self.heap, a, b));
                }
                Ok(self.heap.alloc_set(result))
            }
            ("intersect" | "diff", [Value::Set(other)]) => {
                let other = self.heap.set(*other).borrow();
                let keep_present = name == "intersect";
                let result: Vec<Value> = self
                    .heap
                    .set(items)
                    .borrow()
                    .iter()
                    .filter(|v| {
                        other.contains(v, |a, b| value_eq(&self.heap, a, b)) == keep_present
                    })
                    .cloned()
                    .collect();
                drop(other);
                Ok(self.heap.alloc_set(OrderedSet::from_distinct_items(result)))
            }
            _ => Err(builtin_error(name)),
        }
    }
}

/// The `Output` record's field accessors (BRS-32,
/// `docs/spec/05-stdlib.md`): receiver-only builtins that yield the
/// field value, matching the walker's `proc_output_builtin`.
fn proc_output_builtin(output: &OutputValue, name: &str, args: &[Value]) -> VmResult {
    match (name, args) {
        ("stdout", []) => Ok(Value::Str(output.stdout.clone())),
        ("stderr", []) => Ok(Value::Str(output.stderr.clone())),
        ("code", []) => Ok(Value::Int(output.code)),
        _ => Err(builtin_error(name)),
    }
}

/// The `Response` record's members (BRS-113): two field accessors and
/// `header`, which is a method rather than a field.
///
/// The lookup is case-insensitive because HTTP header names are, and
/// total because a header that is absent is an ordinary answer — the
/// caller writes `?? fallback` rather than guarding.
fn response_builtin(response: &ResponseValue, name: &str, args: &[Value]) -> VmResult {
    match (name, args) {
        ("status", []) => Ok(Value::Int(response.status)),
        ("body", []) => Ok(Value::Str(response.body.clone())),
        ("header", [Value::Str(wanted)]) => {
            let wanted = wanted.to_lowercase();
            let found = response
                .headers
                .iter()
                .find(|(name, _)| *name == wanted)
                .map(|(_, value)| Value::Str(Rc::from(value.as_str())));

            Ok(Value::Option(found.map(Rc::new)))
        }
        _ => Err(builtin_error(name)),
    }
}

/// The `Walk` record's field accessors (BRS-66), the same shape as
/// `proc_output_builtin`.
fn walk_builtin(walk: &WalkValue, name: &str, args: &[Value]) -> VmResult {
    match (name, args) {
        ("paths", []) => Ok(walk.paths.clone()),
        ("unreadable", []) => Ok(walk.unreadable.clone()),
        _ => Err(builtin_error(name)),
    }
}

fn builtin_error(name: &str) -> Signal {
    Signal::Fatal(format!("brasa: unknown builtin method `{name}`"))
}

/// Raises a stdlib-native error: an ordinary error signal carrying a
/// [`Value::NativeError`], caught by naming its qualified name or by
/// `_` like any thrown value.
fn native_error(name: &'static str, message: String) -> Signal {
    Signal::Error(Value::NativeError(Rc::new(NativeErrorValue {
        name,
        message: Rc::from(message),
    })))
}

fn fs_signal(err: fs_glue::FsError) -> Signal {
    native_error(err.name, err.message)
}

fn fs_str(result: fs_glue::FsResult<String>) -> VmResult {
    result.map(Value::str).map_err(fs_signal)
}

fn fs_unit(result: fs_glue::FsResult<()>) -> VmResult {
    result.map(|()| Value::Unit).map_err(fs_signal)
}
