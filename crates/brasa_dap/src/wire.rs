//! The Debug Adapter Protocol's framing and message shapes.
//!
//! Hand-written over `serde_json` rather than taken from a crate, and
//! that is a different call from the LSP's (BRS-92), which took
//! `lsp-types`. The reason is not that a protocol is not a schema — it
//! is — but that no DAP crate clears the bar the others did: `dap` is
//! `0.4.1-alpha1` and `debugserver-types` is a stale set of types for
//! one editor. Depending on an alpha for a wire format is worse than
//! writing nine message shapes.
//!
//! What makes it cheap is that DAP frames exactly like LSP —
//! `Content-Length: N\r\n\r\n` then a JSON body — so the transport is
//! the part already understood.

use std::io::{BufRead, Write};

use serde_json::{Value, json};

/// A message read off the wire.
pub struct Request {
    pub seq: i64,
    pub command: String,
    pub arguments: Value,
}

/// The stdio transport.
pub struct Connection<R: BufRead, W: Write> {
    input: R,
    output: W,
    /// Server-originated sequence numbers. The protocol wants them
    /// monotonic and 1-based; nothing correlates on them except
    /// `request_seq`, which echoes the client's.
    seq: i64,
}

impl<R: BufRead, W: Write> Connection<R, W> {
    pub fn new(input: R, output: W) -> Self {
        Connection {
            input,
            output,
            seq: 0,
        }
    }

    /// Reads one message, or `None` at end of input — which is how a
    /// client that went away is distinguished from one that is idle.
    pub fn read(&mut self) -> std::io::Result<Option<Request>> {
        let mut length: Option<usize> = None;

        loop {
            let mut line = String::new();
            if self.input.read_line(&mut line)? == 0 {
                return Ok(None);
            }

            let trimmed = line.trim_end_matches(['\r', '\n']);
            if trimmed.is_empty() {
                break;
            }

            if let Some(value) = trimmed.strip_prefix("Content-Length:") {
                length = value.trim().parse().ok();
            }
        }

        let Some(length) = length else {
            return Ok(None);
        };

        let mut body = vec![0u8; length];
        self.input.read_exact(&mut body)?;

        let value: Value = serde_json::from_slice(&body)?;

        Ok(Some(Request {
            seq: value["seq"].as_i64().unwrap_or(0),
            command: value["command"].as_str().unwrap_or_default().to_string(),
            arguments: value.get("arguments").cloned().unwrap_or(Value::Null),
        }))
    }

    /// A successful response to `request`, carrying `body`.
    pub fn respond(&mut self, request: &Request, body: Value) -> std::io::Result<()> {
        self.seq += 1;

        self.send(json!({
            "seq": self.seq,
            "type": "response",
            "request_seq": request.seq,
            "success": true,
            "command": request.command,
            "body": body,
        }))
    }

    /// A failed response. The message is shown to the user by every
    /// client, so it is worded like a diagnostic rather than like an
    /// internal error.
    pub fn respond_error(&mut self, request: &Request, message: &str) -> std::io::Result<()> {
        self.seq += 1;

        self.send(json!({
            "seq": self.seq,
            "type": "response",
            "request_seq": request.seq,
            "success": false,
            "command": request.command,
            "message": message,
        }))
    }

    pub fn event(&mut self, event: &str, body: Value) -> std::io::Result<()> {
        self.seq += 1;

        self.send(json!({
            "seq": self.seq,
            "type": "event",
            "event": event,
            "body": body,
        }))
    }

    fn send(&mut self, value: Value) -> std::io::Result<()> {
        let body = serde_json::to_vec(&value)?;

        write!(self.output, "Content-Length: {}\r\n\r\n", body.len())?;
        self.output.write_all(&body)?;
        self.output.flush()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_framed_request_round_trips() {
        let body = br#"{"seq":1,"type":"request","command":"initialize","arguments":{"adapterID":"brasa"}}"#;
        let framed = format!(
            "Content-Length: {}\r\n\r\n{}",
            body.len(),
            String::from_utf8_lossy(body)
        );

        let mut conn = Connection::new(framed.as_bytes(), Vec::new());
        let request = conn.read().expect("reads").expect("a message");

        assert_eq!(request.seq, 1);
        assert_eq!(request.command, "initialize");
        assert_eq!(request.arguments["adapterID"], json!("brasa"));
    }

    /// The header block ends at the blank line, and headers this
    /// adapter does not know are skipped rather than rejected — a
    /// client is allowed to send `Content-Type`.
    #[test]
    fn unknown_headers_are_skipped() {
        let body = br#"{"seq":7,"command":"continue"}"#;
        let framed = format!(
            "Content-Length: {}\r\nContent-Type: application/vscode-jsonrpc\r\n\r\n{}",
            body.len(),
            String::from_utf8_lossy(body)
        );

        let mut conn = Connection::new(framed.as_bytes(), Vec::new());
        let request = conn.read().expect("reads").expect("a message");

        assert_eq!(request.seq, 7);
        assert_eq!(request.command, "continue");
    }

    #[test]
    fn end_of_input_is_not_an_error() {
        let mut conn = Connection::new(&b""[..], Vec::new());
        assert!(conn.read().expect("reads").is_none());
    }

    /// A response echoes the request's seq so the client can correlate,
    /// and carries its own — the two are different counters and mixing
    /// them is the classic way a client hangs.
    #[test]
    fn a_response_echoes_the_request_seq_and_carries_its_own() {
        let mut out = Vec::new();
        {
            let mut conn = Connection::new(&b""[..], &mut out);
            let request = Request {
                seq: 42,
                command: "threads".to_string(),
                arguments: Value::Null,
            };
            conn.respond(&request, json!({ "threads": [] }))
                .expect("writes");
        }

        let text = String::from_utf8(out).expect("utf-8");
        let (header, body) = text.split_once("\r\n\r\n").expect("framed");

        assert!(header.starts_with("Content-Length: "));
        let value: Value = serde_json::from_str(body).expect("valid JSON");

        assert_eq!(value["request_seq"], json!(42));
        assert_eq!(value["seq"], json!(1));
        assert_eq!(value["success"], json!(true));
        assert_eq!(value["command"], json!("threads"));
    }
}
