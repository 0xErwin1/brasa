//! Backend-agnostic glue for `std::http` (`docs/spec/05-stdlib.md`,
//! BRS-113): one blocking request, one response, no runtime.
//!
//! Decisions recorded here (mirrored in the spec):
//!
//! - **Blocking, never async.** Brasa is a synchronous scripting
//!   language; an async runtime would leak colored functions into it
//!   and put a scheduler on the startup path for a feature most scripts
//!   never call.
//! - **A non-2xx status is an answer, not a failure.** A 404 is data the
//!   caller asked for. Only a request that never produced a response —
//!   DNS, connection, TLS, timeout — raises `http.RequestError`. This is
//!   the same split `std::proc` draws between `tryRun` and a
//!   `SpawnError`.
//! - **Nothing initializes at process start.** The agent is built on the
//!   first request and never before, because cold start is the
//!   language's strongest differentiator and a TLS stack is the largest
//!   thing this stdlib pulls in.
//! - Header names are lowercased on the way out, so a script indexes
//!   them by one spelling rather than guessing the server's.

use std::collections::HashMap;
use std::time::Duration;

/// Everything observed from one finished HTTP exchange.
pub struct RawResponse {
    pub status: i64,
    pub body: String,
    pub headers: Vec<(String, String)>,
}

/// The default request timeout. A script that forgets one should still
/// terminate: an unbounded wait in an automation script is a hang
/// nobody debugs.
const DEFAULT_TIMEOUT_MS: u64 = 30_000;

fn agent(timeout_ms: Option<i64>) -> ureq::Agent {
    let millis = timeout_ms
        .and_then(|ms| u64::try_from(ms).ok())
        .filter(|ms| *ms > 0)
        .unwrap_or(DEFAULT_TIMEOUT_MS);

    // A non-2xx status is an ANSWER: the caller gets the code, the
    // headers and the body. `ureq` treats it as an error by default,
    // which would throw away the body of a 404 — usually the part that
    // says why.
    ureq::Agent::config_builder()
        .timeout_global(Some(Duration::from_millis(millis)))
        .http_status_as_error(false)
        .build()
        .into()
}

/// `GET url`. `Err` carries the `http.RequestError` message.
pub fn get(
    url: &str,
    headers: &HashMap<String, String>,
    timeout_ms: Option<i64>,
) -> Result<RawResponse, String> {
    let mut request = agent(timeout_ms).get(url);
    for (name, value) in headers {
        request = request.header(name, value);
    }

    finish(request.call(), url)
}

/// `POST url` with `body` as the request body.
pub fn post(
    url: &str,
    body: &str,
    headers: &HashMap<String, String>,
    timeout_ms: Option<i64>,
) -> Result<RawResponse, String> {
    let mut request = agent(timeout_ms).post(url);
    for (name, value) in headers {
        request = request.header(name, value);
    }

    finish(request.send(body), url)
}

/// Turns one `ureq` outcome into a response or a message.
///
/// Only a request that never produced a response reaches the `Err` arm:
/// the agent is configured not to treat a status as an error, so a 404
/// is never confused with a DNS failure.
fn finish(
    result: Result<ureq::http::Response<ureq::Body>, ureq::Error>,
    url: &str,
) -> Result<RawResponse, String> {
    let response = match result {
        Ok(response) => response,
        Err(err) => return Err(format!("cannot request `{url}`: {err}")),
    };

    let status = i64::from(response.status().as_u16());
    let headers = response
        .headers()
        .iter()
        .map(|(name, value)| {
            (
                name.as_str().to_lowercase(),
                String::from_utf8_lossy(value.as_bytes()).into_owned(),
            )
        })
        .collect();

    let body = response
        .into_body()
        .read_to_string()
        .map_err(|err| format!("cannot read the response body of `{url}`: {err}"))?;

    Ok(RawResponse {
        status,
        body,
        headers,
    })
}
