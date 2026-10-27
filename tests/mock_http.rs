//! A one-shot HTTP server for the `external-http` parser tests.
//!
//! There is no network in CI, so every external-backend test talks to a
//! `TcpListener` bound to a random loopback port. The server accepts exactly one
//! connection, records the request (method, path, headers and body), answers
//! with the status and body it was given, then closes. Every test can therefore
//! assert both sides of the exchange: what the adapter sent and what it made of
//! the answer.
//!
//! This file sits next to the test crates and is pulled in with `mod mock_http;`
//! by `tests/parse_external.rs`; it is also compiled as its own (empty) test
//! target, hence the `dead_code` allowance.
//!
//! Delays are part of the API because timeout handling cannot be tested without
//! a server that answers late. Dropping a [`MockServer`] without calling
//! [`MockServer::finish`] detaches the thread, so a timeout test does not wait
//! for the deliberately slow response.

#![allow(dead_code)]

use std::collections::BTreeMap;
use std::io::{BufRead, BufReader, Read, Write};
use std::net::{TcpListener, TcpStream};
use std::thread::{self, JoinHandle};
use std::time::Duration;

use llm_wiki::config::{ExternalCfg, ExternalResponse};

/// One HTTP request, as the mock server saw it.
#[derive(Debug, Clone)]
pub struct RecordedRequest {
    pub method: String,
    pub path: String,
    /// Header names keep the spelling the client sent; lookups are
    /// case-insensitive.
    pub headers: Vec<(String, String)>,
    pub body: Vec<u8>,
}

impl RecordedRequest {
    /// The value of `name`, compared case-insensitively.
    pub fn header(&self, name: &str) -> Option<&str> {
        self.headers
            .iter()
            .find(|(key, _)| key.eq_ignore_ascii_case(name))
            .map(|(_, value)| value.as_str())
    }

    /// The body decoded as UTF-8, for `contains` assertions.
    pub fn body_text(&self) -> &str {
        std::str::from_utf8(&self.body).unwrap_or("")
    }
}

/// A loopback server that answers exactly one request and then stops.
pub struct MockServer {
    /// `http://127.0.0.1:<port>/parse`, ready for `ExternalCfg.url`.
    pub url: String,
    handle: Option<JoinHandle<Option<RecordedRequest>>>,
}

impl MockServer {
    /// Joins the server thread and returns the request it recorded.
    ///
    /// Only call this once the exchange is over; a timed-out client leaves the
    /// mock thread sleeping, and waiting for it would defeat the timeout test.
    pub fn finish(mut self) -> RecordedRequest {
        self.handle
            .take()
            .expect("the mock server is only joined once")
            .join()
            .expect("the mock server thread must not panic")
            .expect("the client must have sent a request")
    }
}

/// Serves one response with `status` and `body`, with no delay.
pub fn serve_once(status: u16, body: &str) -> MockServer {
    serve_once_delayed(status, body, Duration::ZERO)
}

/// Serves one response after sleeping for `delay`.
pub fn serve_once_delayed(status: u16, body: &str, delay: Duration) -> MockServer {
    let listener = TcpListener::bind(("127.0.0.1", 0)).expect("bind a random loopback port");
    let port = listener
        .local_addr()
        .expect("the listener has an address")
        .port();
    let body = String::from(body);

    let handle = thread::spawn(move || {
        let (stream, _) = listener.accept().ok()?;
        let request = read_request(&stream);
        if !delay.is_zero() {
            thread::sleep(delay);
        }
        write_response(&stream, status, &body);
        request
    });

    MockServer {
        url: format!("http://127.0.0.1:{port}/parse"),
        handle: Some(handle),
    }
}

/// Reads the request line, the headers, and a `Content-Length` body.
///
/// `ureq` sizes a multipart body up front, so it always sends `Content-Length`;
/// a request without one is treated as an empty body instead of blocking on a
/// socket that is still waiting for the response.
fn read_request(stream: &TcpStream) -> Option<RecordedRequest> {
    let mut reader = BufReader::new(stream);
    let mut line = String::new();
    if reader.read_line(&mut line).ok()? == 0 {
        return None;
    }
    let mut parts = line.split_whitespace();
    let method = String::from(parts.next()?);
    let path = String::from(parts.next()?);

    let mut headers = Vec::new();
    loop {
        let mut header = String::new();
        if reader.read_line(&mut header).ok()? == 0 {
            break;
        }
        let header = header.trim_end_matches(['\r', '\n']);
        if header.is_empty() {
            break;
        }
        if let Some((name, value)) = header.split_once(':') {
            headers.push((String::from(name.trim()), String::from(value.trim())));
        }
    }

    let length = headers
        .iter()
        .find(|(name, _)| name.eq_ignore_ascii_case("content-length"))
        .and_then(|(_, value)| value.parse::<usize>().ok())
        .unwrap_or(0);
    let mut body = vec![0u8; length];
    reader.read_exact(&mut body).ok()?;

    Some(RecordedRequest {
        method,
        path,
        headers,
        body,
    })
}

fn write_response(mut stream: &TcpStream, status: u16, body: &str) {
    let reason = match status {
        200 => "OK",
        400 => "Bad Request",
        404 => "Not Found",
        500 => "Internal Server Error",
        _ => "Status",
    };
    let head = format!(
        "HTTP/1.1 {status} {reason}\r\n\
         Content-Type: application/json; charset=utf-8\r\n\
         Content-Length: {}\r\n\
         Connection: close\r\n\r\n",
        body.len()
    );
    let _ = stream.write_all(head.as_bytes());
    let _ = stream.write_all(body.as_bytes());
    let _ = stream.flush();
}

/// An external config pointing at `url`, with the plan's generic field set.
pub fn external_at(url: &str) -> ExternalCfg {
    ExternalCfg {
        kind: String::from("http"),
        url: String::from(url),
        auth_env: String::from("WIKI_PARSE_TOKEN"),
        fields: BTreeMap::from([
            (String::from("file"), String::from("@file")),
            (String::from("fileFlag"), String::from("N")),
            (String::from("system"), String::from("BSS")),
        ]),
        response: ExternalResponse {
            text: String::from("data"),
            ok_code: 200,
        },
        timeout_s: 120,
    }
}

/// An external config for tests that only ask about ordering, never connect.
pub fn sample_external() -> ExternalCfg {
    external_at("http://127.0.0.1:9/parse")
}
