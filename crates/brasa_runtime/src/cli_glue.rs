//! Backend-agnostic argument parsing for `std::cli`
//! (spec: 05 — Stdlib de scripting, BRS-112).
//!
//! The shape is a declaration plus one parse, not a builder: Brasa has
//! no named arguments, so a builder would need an opaque accumulating
//! value and a member per declaration kind — five members and two value
//! kinds to express what a table of rows says directly.
//!
//! Two classes of mistake, kept apart on purpose:
//!
//! - A malformed DECLARATION is the script author's bug and is fatal.
//!   Reporting it as a usage error would tell the person running the
//!   script to fix their command line when the command line was fine.
//! - A malformed COMMAND LINE is [`ParseError`], which the language
//!   surfaces as the catchable `cli.UsageError`. The script decides its
//!   own exit status; a stdlib member must not call `exit`.

/// One declared parameter.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Param {
    pub kind: Kind,
    /// The long name, without dashes: `top` for `--top`.
    pub name: String,
    /// The one-letter short name, without its dash; empty for none.
    pub short: String,
    pub help: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    /// `--verbose`: present or absent, never takes a value.
    Flag,
    /// `--top 5` or `--top=5`.
    Option,
    /// A positional, declared only so `--help` can name it.
    Arg,
}

/// What one command line parsed to.
#[derive(Debug, Default)]
pub struct Parsed {
    pub flags: Vec<String>,
    pub options: Vec<(String, String)>,
    pub rest: Vec<String>,
}

/// A command line the declaration does not accept.
#[derive(Debug)]
pub struct ParseError {
    pub message: String,
}

/// Reads one declaration row. `Err` is the author's bug, not the user's.
pub fn param(row: &[String]) -> Result<Param, String> {
    let [kind, name, short, help] = row else {
        return Err(format!(
            "a `cli` parameter is [kind, name, short, help]; found {} field(s)",
            row.len()
        ));
    };

    let kind = match kind.as_str() {
        "flag" => Kind::Flag,
        "option" => Kind::Option,
        "arg" => Kind::Arg,
        other => {
            return Err(format!(
                "unknown `cli` parameter kind `{other}`: expected `flag`, `option`, or `arg`"
            ));
        }
    };

    if name.is_empty() {
        return Err("a `cli` parameter needs a name".to_string());
    }
    if short.chars().count() > 1 {
        return Err(format!(
            "the short name of `{name}` must be one character, found `{short}`"
        ));
    }

    Ok(Param {
        kind,
        name: name.clone(),
        short: short.clone(),
        help: help.clone(),
    })
}

fn find<'p>(params: &'p [Param], token: &str) -> Option<&'p Param> {
    let (wanted, is_short) = match token.strip_prefix("--") {
        Some(long) => (long, false),
        None => (token.strip_prefix('-').unwrap_or(token), true),
    };

    params.iter().find(|param| {
        param.kind != Kind::Arg
            && if is_short {
                !param.short.is_empty() && param.short == wanted
            } else {
                param.name == wanted
            }
    })
}

/// Parses `args` against `params`.
///
/// `--` ends option parsing: everything after it is positional, however
/// it is spelled. That is the only way to pass an argument that starts
/// with a dash, and every tool that omits it eventually grows it.
pub fn parse(params: &[Param], args: &[String]) -> Result<Parsed, ParseError> {
    let fail = |message: String| ParseError { message };

    let mut parsed = Parsed::default();
    let mut rest_only = false;
    let mut index = 0;

    while index < args.len() {
        let token = &args[index];
        index += 1;

        if rest_only {
            parsed.rest.push(token.clone());
            continue;
        }
        if token == "--" {
            rest_only = true;
            continue;
        }
        if token == "-" || !token.starts_with('-') {
            parsed.rest.push(token.clone());
            continue;
        }

        // `--name=value` is split before lookup so the two spellings of
        // an option reach the same branch.
        let (token, inline) = match token.split_once('=') {
            Some((name, value)) if token.starts_with("--") => (name.to_string(), Some(value)),
            _ => (token.clone(), None),
        };

        let Some(param) = find(params, &token) else {
            return Err(fail(format!("unknown option `{token}`")));
        };

        match param.kind {
            Kind::Flag => {
                if inline.is_some() {
                    return Err(fail(format!(
                        "`--{}` is a flag and takes no value",
                        param.name
                    )));
                }
                parsed.flags.push(param.name.clone());
            }
            Kind::Option => {
                let value = match inline {
                    Some(value) => value.to_string(),
                    None => {
                        let Some(value) = args.get(index) else {
                            return Err(fail(format!("`{token}` needs a value")));
                        };
                        index += 1;
                        value.clone()
                    }
                };
                parsed.options.push((param.name.clone(), value));
            }
            Kind::Arg => unreachable!("`find` never returns a positional"),
        }
    }

    Ok(parsed)
}

/// The usage text, generated from the declaration.
///
/// Generated rather than written by hand because a hand-written usage
/// string drifts from the parser the first time a parameter is added,
/// and a wrong usage message is worse than none.
pub fn help(program: &str, params: &[Param]) -> String {
    let positionals: Vec<&Param> = params.iter().filter(|p| p.kind == Kind::Arg).collect();
    let options: Vec<&Param> = params.iter().filter(|p| p.kind != Kind::Arg).collect();

    let mut usage = format!("usage: {program}");
    if !options.is_empty() {
        usage.push_str(" [options]");
    }
    for param in &positionals {
        usage.push_str(&format!(" <{}>", param.name));
    }
    usage.push('\n');

    let mut render = |group: &[&Param], title: &str, prefix: bool| {
        if group.is_empty() {
            return;
        }

        usage.push_str(&format!("\n{title}\n"));

        let labels: Vec<String> = group
            .iter()
            .map(|param| {
                if !prefix {
                    return param.name.clone();
                }
                match param.short.is_empty() {
                    true => format!("    --{}", param.name),
                    false => format!("-{}, --{}", param.short, param.name),
                }
            })
            .collect();

        let width = labels.iter().map(|label| label.chars().count()).max();
        let width = width.unwrap_or(0);

        for (label, param) in labels.iter().zip(group) {
            let padding = " ".repeat(width - label.chars().count());
            usage.push_str(&format!("  {label}{padding}  {}\n", param.help));
        }
    };

    render(&positionals, "arguments:", false);
    render(&options, "options:", true);

    usage
}

#[cfg(test)]
mod tests {
    use super::*;

    fn params() -> Vec<Param> {
        vec![
            param(&["option".into(), "top".into(), "t".into(), "how many".into()])
                .expect("declared"),
            param(&["flag".into(), "verbose".into(), "v".into(), "loud".into()]).expect("declared"),
            param(&[
                "arg".into(),
                "input".into(),
                String::new(),
                "the file".into(),
            ])
            .expect("declared"),
        ]
    }

    fn args(items: &[&str]) -> Vec<String> {
        items.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn the_two_spellings_of_an_option_agree() {
        let spaced = parse(&params(), &args(&["--top", "5"])).expect("parses");
        let inline = parse(&params(), &args(&["--top=5"])).expect("parses");

        assert_eq!(spaced.options, vec![("top".to_string(), "5".to_string())]);
        assert_eq!(spaced.options, inline.options);
    }

    #[test]
    fn a_short_name_reaches_the_same_parameter() {
        let parsed = parse(&params(), &args(&["-t", "3", "-v"])).expect("parses");

        assert_eq!(parsed.options, vec![("top".to_string(), "3".to_string())]);
        assert_eq!(parsed.flags, vec!["verbose".to_string()]);
    }

    /// `--` is the only way to pass an argument that starts with a
    /// dash, so everything after it is positional however it is spelled.
    #[test]
    fn a_double_dash_ends_option_parsing() {
        let parsed = parse(&params(), &args(&["--top", "1", "--", "--top", "-v"])).expect("parses");

        assert_eq!(parsed.options, vec![("top".to_string(), "1".to_string())]);
        assert!(parsed.flags.is_empty(), "`-v` after `--` is positional");
        assert_eq!(parsed.rest, args(&["--top", "-v"]));
    }

    /// A lone `-` is conventionally "stdin", so it is data rather than a
    /// malformed option.
    #[test]
    fn a_lone_dash_is_positional() {
        let parsed = parse(&params(), &args(&["-"])).expect("parses");

        assert_eq!(parsed.rest, args(&["-"]));
    }

    #[test]
    fn an_unknown_option_is_a_usage_error() {
        let err = parse(&params(), &args(&["--nope"])).expect_err("rejected");

        assert!(
            err.message.contains("unknown option `--nope`"),
            "{}",
            err.message
        );
    }

    #[test]
    fn an_option_with_no_value_left_is_a_usage_error() {
        let err = parse(&params(), &args(&["--top"])).expect_err("rejected");

        assert!(err.message.contains("needs a value"), "{}", err.message);
    }

    /// A flag never takes a value, so `--verbose=1` is a mistake rather
    /// than a silently ignored assignment.
    #[test]
    fn a_flag_given_a_value_is_a_usage_error() {
        let err = parse(&params(), &args(&["--verbose=1"])).expect_err("rejected");

        assert!(err.message.contains("takes no value"), "{}", err.message);
    }

    /// A positional is declared only so the usage text can name it; it
    /// is never matched as an option.
    #[test]
    fn a_positional_declaration_is_not_an_option() {
        let err = parse(&params(), &args(&["--input", "x"])).expect_err("rejected");

        assert!(err.message.contains("unknown option"), "{}", err.message);
    }

    #[test]
    fn a_malformed_declaration_is_the_authors_bug() {
        assert!(param(&["flag".into(), "x".into()]).is_err(), "wrong arity");
        assert!(
            param(&["nope".into(), "x".into(), String::new(), String::new()]).is_err(),
            "unknown kind"
        );
        assert!(
            param(&["flag".into(), String::new(), String::new(), String::new()]).is_err(),
            "empty name"
        );
        assert!(
            param(&["flag".into(), "x".into(), "long".into(), String::new()]).is_err(),
            "multi-character short name"
        );
    }

    /// The usage text is generated so it cannot drift from the parser.
    #[test]
    fn help_names_every_declared_parameter() {
        let text = help("tool", &params());

        assert!(
            text.starts_with("usage: tool [options] <input>\n"),
            "{text}"
        );
        assert!(text.contains("-t, --top"), "{text}");
        assert!(text.contains("-v, --verbose"), "{text}");
        assert!(text.contains("input"), "{text}");
        assert!(text.contains("the file"), "{text}");
    }

    #[test]
    fn a_parameter_without_a_short_name_still_lines_up() {
        let params = vec![
            param(&["flag".into(), "quiet".into(), String::new(), "hush".into()])
                .expect("declared"),
            param(&["flag".into(), "verbose".into(), "v".into(), "loud".into()]).expect("declared"),
        ];

        let text = help("tool", &params);

        assert!(text.contains("    --quiet"), "{text}");
        assert!(text.contains("-v, --verbose"), "{text}");
    }
}
