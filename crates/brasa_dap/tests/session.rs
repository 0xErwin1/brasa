//! The adapter driven the way an editor drives it (BRS-119).
//!
//! A whole conversation is scripted into a buffer and the responses
//! are read back, which is what makes the protocol testable without
//! VS Code in the loop. What is pinned is the exchange: the
//! capabilities, the events that must arrive unprompted, and that
//! every advertised capability answers.

use std::path::PathBuf;

use serde_json::{Value, json};

fn scratch(name: &str, source: &str) -> PathBuf {
    let dir = std::env::temp_dir().join("brasa-dap-tests");
    std::fs::create_dir_all(&dir).expect("the scratch directory is writable");

    let path = dir.join(name);
    std::fs::write(&path, source).expect("the fixture is writable");
    path
}

/// Frames a sequence of request bodies the way a client would.
fn script(requests: &[Value]) -> Vec<u8> {
    let mut wire = Vec::new();

    for (ix, request) in requests.iter().enumerate() {
        let mut request = request.clone();
        request["seq"] = json!(ix + 1);
        request["type"] = json!("request");

        let body = serde_json::to_vec(&request).expect("serialises");
        wire.extend_from_slice(format!("Content-Length: {}\r\n\r\n", body.len()).as_bytes());
        wire.extend_from_slice(&body);
    }

    wire
}

/// Every message the adapter wrote, in order.
fn converse(requests: &[Value]) -> Vec<Value> {
    let input = script(requests);
    let mut output = Vec::new();

    {
        let mut conn = brasa_dap::wire::Connection::new(&input[..], &mut output);
        brasa_dap::serve(&mut conn).expect("the adapter runs");
    }

    let mut text = String::from_utf8(output).expect("utf-8");
    let mut messages = Vec::new();

    while let Some(split) = text.find("\r\n\r\n") {
        let header = &text[..split];
        let length: usize = header
            .lines()
            .find_map(|line| line.strip_prefix("Content-Length:"))
            .and_then(|value| value.trim().parse().ok())
            .expect("a framed message");

        let body_start = split + 4;
        let body = text[body_start..body_start + length].to_string();
        messages.push(serde_json::from_str(&body).expect("valid JSON"));

        text = text[body_start + length..].to_string();
    }

    messages
}

fn responses_to<'a>(messages: &'a [Value], command: &str) -> Vec<&'a Value> {
    messages
        .iter()
        .filter(|m| m["type"] == json!("response") && m["command"] == json!(command))
        .collect()
}

fn events(messages: &[Value], event: &str) -> Vec<Value> {
    messages
        .iter()
        .filter(|m| m["type"] == json!("event") && m["event"] == json!(event))
        .cloned()
        .collect()
}

const COUNTER: &str = r#"def bump(n: int): int
  let doubled = n * 2
  doubled + 1
end

def main()
  let a = bump(20)
  puts a
end
"#;

/// `initialize` answers with capabilities and the `initialized` event
/// follows unprompted — a client that never gets that event never
/// sends breakpoints, so this is the handshake that must not regress.
#[test]
fn initialize_answers_and_announces_initialized() {
    let script = scratch("init.bras", COUNTER);

    let messages = converse(&[
        json!({ "command": "initialize", "arguments": { "adapterID": "brasa" } }),
        json!({ "command": "launch", "arguments": { "program": script } }),
        json!({ "command": "disconnect" }),
    ]);

    let initialize = responses_to(&messages, "initialize");
    assert_eq!(initialize.len(), 1);
    assert_eq!(
        initialize[0]["body"]["supportsConfigurationDoneRequest"],
        json!(true)
    );

    assert_eq!(events(&messages, "initialized").len(), 1);
}

/// A breakpoint on a real line verifies; the adapter stops there and
/// says so with a `stopped` event, which is what moves an editor's
/// cursor.
#[test]
fn a_breakpoint_verifies_and_stops_the_program() {
    let script = scratch("stop.bras", COUNTER);

    let messages = converse(&[
        json!({ "command": "initialize", "arguments": {} }),
        json!({ "command": "launch", "arguments": { "program": script } }),
        json!({
            "command": "setBreakpoints",
            "arguments": {
                "source": { "path": script },
                "breakpoints": [{ "line": 2 }],
            }
        }),
        json!({ "command": "configurationDone" }),
        json!({ "command": "stackTrace", "arguments": { "threadId": 1 } }),
        json!({ "command": "disconnect" }),
    ]);

    let set = responses_to(&messages, "setBreakpoints");
    assert_eq!(set[0]["body"]["breakpoints"][0]["verified"], json!(true));

    let stopped = events(&messages, "stopped");
    assert_eq!(stopped.len(), 1, "the run stopped once");
    assert_eq!(stopped[0]["body"]["threadId"], json!(1));

    let trace = responses_to(&messages, "stackTrace");
    let frames = trace[0]["body"]["stackFrames"]
        .as_array()
        .expect("frames is an array");

    assert_eq!(frames.len(), 2, "paused in `bump`, called from `main`");
    assert_eq!(
        frames[0]["name"],
        json!("bump"),
        "innermost first, as DAP wants"
    );
    assert_eq!(frames[1]["name"], json!("main"));
}

/// A line with no code comes back unverified rather than as an error:
/// clicking a blank line is ordinary, and the editor greys the marker.
#[test]
fn a_line_without_code_is_unverified_not_an_error() {
    let script = scratch("blank.bras", COUNTER);

    let messages = converse(&[
        json!({ "command": "initialize", "arguments": {} }),
        json!({ "command": "launch", "arguments": { "program": script } }),
        json!({
            "command": "setBreakpoints",
            "arguments": {
                "source": { "path": script },
                "breakpoints": [{ "line": 5 }],
            }
        }),
        json!({ "command": "disconnect" }),
    ]);

    let set = responses_to(&messages, "setBreakpoints");
    assert_eq!(set[0]["success"], json!(true), "not an error");
    assert_eq!(set[0]["body"]["breakpoints"][0]["verified"], json!(false));
}

/// `scopes` and `variables` compose: the scope's reference is what the
/// client sends back, and the locals come out with the paused values.
#[test]
fn scopes_and_variables_read_the_paused_frame() {
    let script = scratch("vars.bras", COUNTER);

    let messages = converse(&[
        json!({ "command": "initialize", "arguments": {} }),
        json!({ "command": "launch", "arguments": { "program": script } }),
        json!({
            "command": "setBreakpoints",
            "arguments": {
                "source": { "path": script },
                "breakpoints": [{ "line": 2 }],
            }
        }),
        json!({ "command": "configurationDone" }),
        // Frame 1 is `bump`, the innermost, in the substrate's order.
        json!({ "command": "scopes", "arguments": { "frameId": 1 } }),
        json!({ "command": "variables", "arguments": { "variablesReference": 2 } }),
        json!({ "command": "disconnect" }),
    ]);

    let scopes = responses_to(&messages, "scopes");
    assert_eq!(scopes[0]["body"]["scopes"][0]["name"], json!("Locals"));
    assert_eq!(
        scopes[0]["body"]["scopes"][0]["variablesReference"],
        json!(2)
    );

    let variables = responses_to(&messages, "variables");
    let slots = variables[0]["body"]["variables"]
        .as_array()
        .expect("variables is an array");

    assert_eq!(slots[0]["name"], json!("slot 0"));
    assert_eq!(slots[0]["value"], json!("20"), "`main` called `bump(20)`");
}

/// Stepping is wired to the substrate: `next` advances and the client
/// gets another `stopped`.
#[test]
fn stepping_produces_another_stop() {
    let script = scratch("step.bras", COUNTER);

    let messages = converse(&[
        json!({ "command": "initialize", "arguments": {} }),
        json!({ "command": "launch", "arguments": { "program": script } }),
        json!({
            "command": "setBreakpoints",
            "arguments": {
                "source": { "path": script },
                "breakpoints": [{ "line": 2 }],
            }
        }),
        json!({ "command": "configurationDone" }),
        json!({ "command": "next", "arguments": { "threadId": 1 } }),
        json!({ "command": "disconnect" }),
    ]);

    assert_eq!(
        events(&messages, "stopped").len(),
        2,
        "the breakpoint, then the step"
    );
}

/// Running with no breakpoints terminates, and the client is told.
#[test]
fn a_run_with_no_breakpoints_terminates() {
    let script = scratch("plain.bras", COUNTER);

    let messages = converse(&[
        json!({ "command": "initialize", "arguments": {} }),
        json!({ "command": "launch", "arguments": { "program": script } }),
        json!({ "command": "configurationDone" }),
        json!({ "command": "disconnect" }),
    ]);

    assert!(events(&messages, "stopped").is_empty());
    assert_eq!(events(&messages, "terminated").len(), 1);
}

/// `evaluate` answers a plain variable read and refuses anything else.
/// Guessing at an expression would make a debugger answer the wrong
/// question silently, which is worse than saying it cannot.
#[test]
fn evaluate_reads_a_variable_and_refuses_an_expression() {
    let script = scratch("eval.bras", COUNTER);

    let messages = converse(&[
        json!({ "command": "initialize", "arguments": {} }),
        json!({ "command": "launch", "arguments": { "program": script } }),
        json!({
            "command": "setBreakpoints",
            "arguments": {
                "source": { "path": script },
                "breakpoints": [{ "line": 2 }],
            }
        }),
        json!({ "command": "configurationDone" }),
        json!({ "command": "evaluate", "arguments": { "expression": "slot 0", "frameId": 1 } }),
        json!({ "command": "evaluate", "arguments": { "expression": "n * 2 + 1", "frameId": 1 } }),
        json!({ "command": "disconnect" }),
    ]);

    let evaluated = responses_to(&messages, "evaluate");

    assert_eq!(evaluated[0]["success"], json!(true));
    assert_eq!(evaluated[0]["body"]["result"], json!("20"));

    assert_eq!(evaluated[1]["success"], json!(false));
    assert!(
        evaluated[1]["message"]
            .as_str()
            .expect("a message")
            .contains("plain variable read")
    );
}

/// A program that does not compile ends the session with an explanation
/// rather than launching into nothing.
#[test]
fn a_program_that_does_not_compile_terminates_with_a_message() {
    let script = scratch("broken.bras", "def main()\n  let x = missingName\nend\n");

    let messages = converse(&[
        json!({ "command": "initialize", "arguments": {} }),
        json!({ "command": "launch", "arguments": { "program": script } }),
    ]);

    let output = events(&messages, "output");
    assert_eq!(output.len(), 1);
    assert!(
        output[0]["body"]["output"]
            .as_str()
            .expect("output text")
            .contains("did not compile")
    );
    assert_eq!(events(&messages, "terminated").len(), 1);
}
