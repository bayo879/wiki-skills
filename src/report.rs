//! Response envelope and exit-code contract.
//!
//! Every command ends in [`emit`] (success) or [`fail`] (failure): exactly one
//! response document is written, then the process exits with the matching code.
//! `--json` selects the machine format; without it the same payload is rendered
//! as plain text and the exit code is unchanged.

use std::io::Write as _;
use std::sync::atomic::{AtomicBool, Ordering};

use serde::Serialize;
use serde_json::Value;

/// Exit codes fixed by the CLI contract.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[repr(i32)]
pub enum Exit {
    /// Success.
    Ok = 0,
    /// The command ran but found error-level problems.
    Error = 1,
    /// Bad usage or bad environment.
    Usage = 2,
    /// The command stopped and needs a human decision.
    #[allow(dead_code)] // Constructed once the decision-gated commands land.
    NeedsConfirm = 3,
    /// Another process holds the repository lock.
    #[allow(dead_code)] // Constructed once the write commands take the lock.
    Busy = 4,
}

impl Exit {
    /// Numeric process exit code.
    pub fn code(self) -> i32 {
        self as i32
    }
}

/// Whether output should be JSON. Set from the raw arguments, before parsing,
/// so that argument-parsing failures are reported in the requested format.
static JSON_MODE: AtomicBool = AtomicBool::new(false);

/// Chooses the output format. Must run before the first [`emit`] or [`fail`].
pub fn init(json: bool) {
    JSON_MODE.store(json, Ordering::Relaxed);
}

fn json_mode() -> bool {
    JSON_MODE.load(Ordering::Relaxed)
}

#[derive(Serialize)]
struct Success<'a, T> {
    ok: bool,
    cmd: &'a str,
    data: T,
}

#[derive(Serialize)]
struct Failure<'a> {
    ok: bool,
    cmd: &'a str,
    error: ErrorBody<'a>,
}

#[derive(Serialize)]
struct ErrorBody<'a> {
    code: &'a str,
    message: &'a str,
}

/// Reports a successful command and exits 0.
pub fn emit<T: Serialize>(cmd: &str, data: T) -> ! {
    emit_with(cmd, data, Exit::Ok, |data| {
        match serde_json::to_value(data) {
            Ok(value) => render_human(cmd, &value),
            Err(_) => cmd.to_string(),
        }
    })
}

/// Reports a successful command whose payload decides the exit code.
///
/// `exit` is `Exit::Error` when the command ran to completion but found
/// error-level problems (lint under `--strict`, for one). `human` renders the
/// text form, because a payload like a finding list reads better as one line
/// per item than as a `key: value` blob.
pub fn emit_with<T: Serialize>(
    cmd: &str,
    data: T,
    exit: Exit,
    human: impl FnOnce(&T) -> String,
) -> ! {
    if json_mode() {
        let envelope = Success {
            ok: true,
            cmd,
            data,
        };
        match serde_json::to_string(&envelope) {
            Ok(line) => write_line(&line),
            Err(_) => fail(
                cmd,
                Exit::Error,
                "E_INTERNAL",
                "failed to serialize response",
            ),
        }
    } else {
        write_line(&human(&data));
    }
    std::process::exit(exit.code())
}

/// Reports a failed command and exits with `exit`. Human-readable diagnostics
/// go to stderr, where they cannot be mistaken for the machine-readable stdout.
pub fn fail(cmd: &str, exit: Exit, code: &str, msg: &str) -> ! {
    if json_mode() {
        let envelope = Failure {
            ok: false,
            cmd,
            error: ErrorBody { code, message: msg },
        };
        let line =
            serde_json::to_string(&envelope).unwrap_or_else(|_| String::from(INTERNAL_FAILURE));
        write_line(&line);
    } else {
        let _ = writeln!(std::io::stderr(), "error [{code}]: {msg}");
    }
    std::process::exit(exit.code())
}

/// Fallback document for the case where the envelope itself cannot be
/// serialized. Unreachable with `&str` payloads, but a missing document would
/// break the one-document contract, so it is still spelled out.
const INTERNAL_FAILURE: &str = r#"{"ok":false,"cmd":"","error":{"code":"E_INTERNAL","message":"failed to serialize response"}}"#;

/// Writes one line to stdout. A closed pipe means the reader stopped caring;
/// there is nobody left to report that to, and the exit code still carries the
/// outcome.
fn write_line(text: &str) {
    let mut out = std::io::stdout().lock();
    let _ = out.write_all(text.as_bytes());
    let _ = out.write_all(b"\n");
    let _ = out.flush();
}

/// Human rendering of a payload: one `key: value` line per top-level field, or
/// just the command name when there is nothing to render. Commands that need a
/// richer layout can take over rendering later.
fn render_human(cmd: &str, value: &Value) -> String {
    match value {
        Value::Object(map) if map.is_empty() => cmd.to_string(),
        Value::Object(map) => map
            .iter()
            .map(|(key, value)| format!("{key}: {}", render_scalar(value)))
            .collect::<Vec<_>>()
            .join("\n"),
        other => render_scalar(other),
    }
}

fn render_scalar(value: &Value) -> String {
    match value {
        Value::String(text) => text.clone(),
        other => other.to_string(),
    }
}
