//! The native builtin registry: the stable `name -> BuiltinId` mapping
//! shared by the code generator (BRS-27) and the VM (BRS-28).
//!
//! spec: 07 — Diseño del bytecode (calls) keeps the builtin registry a
//! stdlib concern — bytecode only carries the opaque [`BuiltinId`] — but
//! the two consumers must agree on the mapping, so the table lives here
//! in the shared vocabulary crate. Ids are positions in [`BUILTINS`];
//! appending is the only compatible way to extend the table (M4 adds
//! the remaining stdlib modules).
//!
//! Calling convention for [`crate::Op::CallBuiltin`]: `argc` counts
//! every pushed operand, the receiver included when the builtin takes
//! one ([`BuiltinDef::has_receiver`]), so the stack effect is always
//! "pop `argc`, push one result".
//!
//! Two internal entries exist for code the generator can prove faulty
//! at compile time but must fail at runtime, mirroring the
//! behavior fixed by spec: 05 — Stdlib de scripting:
//!
//! - `<fatal>`: raises an uncatchable fatal error with the message
//!   string argument (e.g. a member call on a module that has not
//!   landed yet).
//! - `<assert-failed>`: raises `panics.AssertionFailed` with the detail
//!   string argument (match fall-through after guards, a `for` pattern
//!   that did not match the element).

use crate::BuiltinId;

/// One registry entry.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct BuiltinDef {
    /// The surface name: a prelude function (`puts`), a method name
    /// (`push`), a dotted module member (`math.sqrt`), or an internal
    /// `<...>` marker no surface name can collide with.
    pub name: &'static str,
    /// Whether the first pushed operand is the receiver (method-style
    /// builtins) rather than an ordinary argument.
    pub has_receiver: bool,
}

const fn free(name: &'static str) -> BuiltinDef {
    BuiltinDef {
        name,
        has_receiver: false,
    }
}

const fn method(name: &'static str) -> BuiltinDef {
    BuiltinDef {
        name,
        has_receiver: true,
    }
}

/// The registry, in id order. Method names are shared across receiver
/// types (`len` is one id for string, Vector, Map, and Set); the VM
/// dispatches on the receiver's runtime kind.
pub const BUILTINS: &[BuiltinDef] = &[
    // Prelude functions (spec: 05 — Stdlib de scripting).
    free("puts"),
    free("print"),
    // `std::math` members (spec: 05 — Stdlib de scripting).
    free("math.sqrt"),
    free("math.floor"),
    free("math.ceil"),
    free("math.round"),
    free("math.pow"),
    free("math.abs"),
    free("math.min"),
    free("math.max"),
    // Internal failure raisers (module docs).
    free("<fatal>"),
    free("<assert-failed>"),
    // Universal derived toString, as a bound value (`v.toString` without
    // the call); the call form compiles to `Op::ToString` instead.
    method("toString"),
    // string methods (`brasa_typeck::builtins`).
    method("len"),
    method("count"),
    method("trim"),
    method("toUpper"),
    method("toLower"),
    method("contains?"),
    method("startsWith?"),
    method("endsWith?"),
    method("split"),
    method("lines"),
    method("chars"),
    method("slice"),
    method("repeat"),
    method("replace"),
    method("find"),
    method("toInt"),
    method("toFloat"),
    // Vector methods.
    method("push"),
    method("pop"),
    method("first"),
    method("last"),
    method("reverse"),
    method("join"),
    method("map"),
    method("filter"),
    method("each"),
    method("sortBy"),
    // Map methods.
    method("keys"),
    method("values"),
    method("insert"),
    method("remove"),
    method("get"),
    method("has?"),
    // Set methods.
    method("add"),
    // M4 string surface (BRS-31): appended, never reordered. `reverse`
    // already exists above (ids are shared across receiver kinds).
    method("bytes"),
    method("trimStart"),
    method("trimEnd"),
    method("padStart"),
    method("padEnd"),
    method("match?"),
    method("captures"),
    method("replaceRe"),
    method("scan"),
    // M4 `std::proc` + `std::env` (BRS-32): appended, never reordered.
    free("proc.run"),
    free("proc.tryRun"),
    free("proc.shell"),
    free("proc.tryRunAll"),
    free("http.get"),
    free("http.post"),
    // The `Response` record's members (BRS-113), like the `Output`
    // and `Walk` field accessors below.
    method("status"),
    method("body"),
    method("header"),
    free("cli.parse"),
    free("cli.help"),
    // The `Args` record's members (BRS-112).
    method("flag"),
    method("option"),
    method("rest"),
    free("env.get"),
    free("env.set"),
    free("env.vars"),
    free("env.args"),
    // The `Output` record's field reads, dispatched on the receiver's
    // runtime kind like every method-style builtin.
    method("stdout"),
    method("stderr"),
    method("code"),
    // M4 `std::fs` plus `env.cwd`/`env.cd` (BRS-33): appended, never
    // reordered.
    free("fs.read"),
    free("fs.write"),
    free("fs.append"),
    free("fs.exists?"),
    free("fs.isFile?"),
    free("fs.isDir?"),
    free("fs.ls"),
    free("fs.glob"),
    free("fs.walk"),
    free("fs.mkdir"),
    free("fs.mkdirAll"),
    free("fs.rm"),
    free("fs.rmAll"),
    free("fs.cp"),
    free("fs.mv"),
    free("fs.join"),
    free("fs.base"),
    free("fs.dir"),
    free("fs.ext"),
    free("fs.abs"),
    free("env.cwd"),
    free("env.cd"),
    // M4 `std::json` + `std::io` (BRS-34): appended, never reordered.
    free("json.parse"),
    free("json.stringify"),
    free("io.puts"),
    free("io.print"),
    free("io.eprint"),
    free("io.readLine"),
    free("io.readAll"),
    // The `Json` accessors, dispatched on the receiver's runtime kind
    // (`Json` or the flattening `Option<Json>`) like every
    // method-style builtin.
    method("asString"),
    method("asInt"),
    method("asFloat"),
    method("asBool"),
    method("asArray"),
    method("asObject"),
    method("null?"),
    // M4 collections plus `std::math`/`std::time`/`std::rand` closure
    // (BRS-35): appended, never reordered. `find`, `each`, `remove`,
    // `has?`, and `len` already exist above (ids are shared across
    // receiver kinds); the math constants are zero-argument free
    // builtins served by the module field-read path.
    method("reduce"),
    method("any?"),
    method("all?"),
    method("sort"),
    method("zip"),
    method("flatten"),
    method("uniq"),
    method("entries"),
    method("merge"),
    method("union"),
    method("intersect"),
    method("diff"),
    free("math.pi"),
    free("math.e"),
    free("time.now"),
    free("time.nowMillis"),
    free("time.sleep"),
    free("time.iso"),
    free("rand.seed"),
    free("rand.int"),
    free("rand.float"),
    free("rand.choice"),
    free("rand.shuffle"),
    method("toFixed"),
    free("env.exit"),
    free("fs.resolve"),
    free("fs.isSymlink?"),
    // The `Walk` record's field reads are receiver-only, exactly like
    // `Output`'s (BRS-66).
    method("paths"),
    method("unreadable"),
    free("fs.tryWalk"),
    // BRS-53: appended, never reordered. `slice` and `join` grew a
    // Vector arm rather than a new entry — ids are shared across
    // receiver kinds — so only the new string method lands here.
    method("removePrefix"),
    // Structured concurrency (BRS-133): appended, never reordered.
    // `concurrent` is a prelude-style free builtin like `print`;
    // `spawn` and `value` are the `Scope`/`Task` receiver methods.
    free("concurrent"),
    method("spawn"),
    method("value"),
    // `std::http` with request headers (BRS-129): appended, never
    // reordered.
    free("http.getWith"),
    free("http.postWith"),
    // The `NativeError` record a `catch` arm binds (BRS-135).
    method("message"),
    // The `std::json` write side (BRS-34): appended, never reordered.
    // `json.stringify` widened its parameter rather than gaining a
    // second entry — a signature change is not an id change.
    free("json.of"),
];

/// Looks up a builtin by its stable name.
pub fn builtin_id(name: &str) -> Option<BuiltinId> {
    BUILTINS
        .iter()
        .position(|def| def.name == name)
        .map(|ix| BuiltinId(ix as u16))
}

/// The entry behind an id, if the id is in range.
pub fn builtin_def(id: BuiltinId) -> Option<&'static BuiltinDef> {
    BUILTINS.get(id.0 as usize)
}

#[cfg(test)]
mod tests {
    use super::{BUILTINS, builtin_def, builtin_id};
    use crate::BuiltinId;

    #[test]
    fn names_are_unique_and_round_trip() {
        for (ix, def) in BUILTINS.iter().enumerate() {
            let id = builtin_id(def.name).expect("every name resolves");
            assert_eq!(id, BuiltinId(ix as u16), "duplicate name {}", def.name);
            assert_eq!(builtin_def(id), Some(def));
        }
    }

    /// The registry mints ids; `brasa_stdlib` declares what the members
    /// are. A declared member with no id could not be called at all, so
    /// the two halves must cover each other (BRS-96).
    #[test]
    fn every_declared_method_holds_a_receiver_id() {
        for (receiver, _, members) in brasa_stdlib::RECEIVERS {
            for decl in *members {
                let id = builtin_id(decl.name).unwrap_or_else(|| {
                    panic!("`{receiver}.{}` is declared but has no id", decl.name)
                });

                assert!(
                    builtin_def(id).is_some_and(|def| def.has_receiver),
                    "`{receiver}.{}` is a method, so its entry must take a receiver",
                    decl.name
                );
            }
        }
    }

    /// A name shared across receiver kinds holds ONE id, and each
    /// receiver declares its own row for it. That is what makes
    /// `remove` able to answer a bool on a `Set` and a value on a
    /// `Map`: the VM dispatches on the runtime kind, so the two
    /// declarations describe different receivers rather than
    /// contradicting each other.
    #[test]
    fn a_shared_method_name_resolves_to_one_id_for_every_receiver() {
        for name in ["len", "remove", "has?", "get", "each", "find", "reverse"] {
            let declaring: Vec<_> = brasa_stdlib::RECEIVERS
                .iter()
                .filter(|(_, _, members)| members.iter().any(|decl| decl.name == name))
                .map(|(receiver, _, _)| *receiver)
                .collect();

            if declaring.len() < 2 {
                continue;
            }

            let id = builtin_id(name).expect("a declared name has an id");
            assert_eq!(
                BUILTINS.iter().filter(|def| def.name == name).count(),
                1,
                "`{name}` is declared by {declaring:?} and must share one id"
            );
            assert!(builtin_def(id).is_some_and(|def| def.has_receiver));
        }
    }

    /// The same cover for the free modules, whose registry names are
    /// qualified (`fs.read`) because a bare member name is not unique
    /// across modules (`fs.read`, `io.readAll`).
    ///
    /// Both directions walk `FREE_MODULES` rather than naming a module,
    /// so converting the next one is covered without editing this file.
    #[test]
    fn every_declared_free_member_holds_a_free_id() {
        for (module, members) in brasa_stdlib::FREE_MODULES {
            for decl in *members {
                let qualified = format!("{}.{}", module, decl.name);
                let id = builtin_id(&qualified)
                    .unwrap_or_else(|| panic!("`{qualified}` is declared but has no id"));

                assert!(
                    builtin_def(id).is_some_and(|def| !def.has_receiver),
                    "`{qualified}` is a module member, so its entry must take no receiver"
                );
            }
        }
    }

    /// The same cover for the records, whose registry names are bare
    /// and receiver-taking like a method's — `status` and `paths` are
    /// reached through a receiver exactly as `push` is, and the VM
    /// dispatches on the receiver's runtime kind.
    ///
    /// Walks `RECORDS` for the same reason the free-module cover walks
    /// `FREE_MODULES`: a record added later is covered without editing
    /// this file.
    #[test]
    fn every_declared_record_member_holds_a_receiver_id() {
        for (record, members) in brasa_stdlib::RECORDS {
            for decl in *members {
                let id = builtin_id(decl.name).unwrap_or_else(|| {
                    panic!("`{record}.{}` is declared but has no id", decl.name)
                });

                assert!(
                    builtin_def(id).is_some_and(|def| def.has_receiver),
                    "`{record}.{}` is reached through a receiver, so its entry must take one",
                    decl.name
                );
            }
        }
    }

    /// The other direction: an id minted under a converted module's
    /// prefix that no row declares would be a member the checker cannot
    /// type and the VM cannot reach.
    #[test]
    fn every_free_registry_entry_is_declared() {
        for (module, _) in brasa_stdlib::FREE_MODULES {
            let prefix = format!("{module}.");

            for def in BUILTINS {
                let Some(member) = def.name.strip_prefix(&prefix) else {
                    continue;
                };

                assert!(
                    brasa_stdlib::free_member(module, member).is_some(),
                    "`{}` holds an id but no row declares it",
                    def.name
                );
            }
        }
    }

    #[test]
    fn unknown_names_do_not_resolve() {
        assert_eq!(builtin_id("definitelyNotABuiltin"), None);
        assert_eq!(builtin_def(BuiltinId(u16::MAX)), None);
    }
}
