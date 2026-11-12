//! Response envelope and exit-code contract.
//!
//! Every command ends in [`emit`] (success) or [`fail`] (failure): exactly one
//! response document is written, then the process exits with the matching code.
//! `--json` selects the machine format; without it the same payload is rendered
//! as plain text and the exit code is unchanged.

use std::io::Write as _;
use std::sync::atomic::{AtomicBool, Ordering};

use crate::output_context::{render_context, Context};
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
    NeedsConfirm = 3,
    /// Another process holds the repository lock.
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
    context: Context,
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
    /// Process exit code; `0` never appears in a failure document.
    exit: i32,
    /// What `ok=false` and this exit code do and do not prove.
    meaning: &'static str,
    /// What to inspect before retrying.
    next: &'static str,
    /// What is known to be retained or already applied; unknown when unlisted.
    state: &'static str,
    #[serde(skip_serializing_if = "Option::is_none")]
    detail: Option<Value>,
}

/// What a failure exit code proves, independent of the repository state.
///
/// These strings are part of the public error contract: they must stay true
/// even when the command failed before it could open the repository.
fn meaning(exit: Exit) -> &'static str {
    match exit {
        Exit::Ok => "ok=false with exit 0 is not a state this build produces",
        Exit::Error => {
            "ok=false: the command ran but found error-level problems. This does not prove that nothing was written; earlier steps may already have taken effect."
        }
        Exit::Usage => {
            "ok=false: the arguments or the environment were rejected. Some steps may have run before the refusal (for example engine initialisation), so this does not prove the repository is untouched."
        }
        Exit::NeedsConfirm => {
            "ok=false: the command stopped at a decision only a human can make. Work already recorded is preserved, not rolled back."
        }
        Exit::Busy => {
            "ok=false: another process holds the repository lock. This command did nothing; retry after the other command finishes."
        }
    }
}

fn known_state(code: &str) -> &'static str {
    match code {
        "E_LOCKED" => "No business work started; engine initialisation or repair may already have run.",
        "E_SOURCE_DRIFT" => {
            "The task and its source todo are retained and staging is kept; the merge did not complete."
        }
        "E_PAGE_CONFLICT" => {
            "The conflicting page was not overwritten; other work from this invocation may already have landed."
        }
        "E_NOT_INITIALISED" => "Repository work did not start.",
        _ => {
            "No general retention claim can be made beyond the message and detail; treat unlisted effects as unknown."
        }
    }
}

fn next_step(code: &str) -> &'static str {
    match code {
        "E_LOCKED" => "Wait for the other command to finish, then rerun this one.",
        "E_NOT_INITIALISED" => "Run `llm-wiki doctor` to see what is missing, then `llm-wiki doctor --fix` to repair it.",
        "E_SOURCE_DRIFT" => "Re-read the named source; if it changed on purpose, rerun ingest to rebuild its task, otherwise restore the bytes.",
        "E_PAGE_CONFLICT" => "Inspect the named page and decide whether to keep or regenerate it; nothing was overwritten.",
        _ => "Inspect the message and detail, then use `llm-wiki doctor` or `llm-wiki task status` to observe what actually happened before retrying.",
    }
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
    let context = Context::new(cmd, &format!("llm-wiki {cmd} --help"),
        "Normal business report. Read the reported scope and outstanding work before inferring completion.");
    emit_context(cmd, data, exit, context, human)
}

/// Emit the same invocation facts in JSON and human-readable output.
pub fn emit_context<T: Serialize>(
    cmd: &str,
    data: T,
    exit: Exit,
    context: Context,
    human: impl FnOnce(&T) -> String,
) -> ! {
    if json_mode() {
        let envelope = Success {
            ok: true,
            cmd,
            context,
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
        write_line(&format!("{}\n{}", render_context(&context), human(&data)));
    }
    std::process::exit(exit.code())
}

/// Reports a failed command and exits with `exit`. Human-readable diagnostics
/// go to stderr, where they cannot be mistaken for the machine-readable stdout.
pub fn fail(cmd: &str, exit: Exit, code: &str, msg: &str) -> ! {
    fail_detail(cmd, exit, code, msg, None)
}

/// Like [`fail`], with a structured `detail` object in the JSON envelope.
///
/// The detail carries the data a caller needs to act on a refusal (which page
/// drifted, which source went stale). The field is omitted when absent, so
/// commands without one keep the envelope they always had.
pub fn fail_detail(cmd: &str, exit: Exit, code: &str, msg: &str, detail: Option<Value>) -> ! {
    if json_mode() {
        let envelope = Failure {
            ok: false,
            cmd,
            error: ErrorBody {
                code,
                message: msg,
                exit: exit.code(),
                meaning: meaning(exit),
                next: next_step(code),
                state: known_state(code),
                detail,
            },
        };
        let line =
            serde_json::to_string(&envelope).unwrap_or_else(|_| String::from(INTERNAL_FAILURE));
        write_line(&line);
    } else {
        let _ = writeln!(std::io::stderr(), "error [{code}]: {msg}");
        let _ = writeln!(std::io::stderr(), "exit: {} (cmd {cmd})", exit.code());
        let _ = writeln!(std::io::stderr(), "meaning: {}", meaning(exit));
        let _ = writeln!(std::io::stderr(), "next: {}", next_step(code));
        let _ = writeln!(std::io::stderr(), "state: {}", known_state(code));
        if let Some(detail) = &detail {
            let _ = writeln!(std::io::stderr(), "detail: {detail}");
        }
    }
    std::process::exit(exit.code())
}

/// Fallback document for the case where the envelope itself cannot be
/// serialized. Unreachable with `&str` payloads, but a missing document would
/// break the one-document contract, so it is still spelled out.
const INTERNAL_FAILURE: &str = r#"{"ok":false,"cmd":"","error":{"code":"E_INTERNAL","message":"failed to serialize response","exit":1,"meaning":"ok=false: the command's report could not be produced; unknown effects must be checked before retrying.","next":"Inspect the repository state before retrying; run `llm-wiki doctor`.","state":"No claim can be made beyond what this document names; treat unlisted effects as unknown."}}"#;

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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn known_states_name_retention_or_admit_ignorance() {
        assert!(known_state("E_SOURCE_DRIFT").contains("retained"));
        assert!(known_state("E_PAGE_CONFLICT").contains("not overwritten"));
        assert!(known_state("E_NOT_INITIALISED").contains("did not start"));
        assert!(known_state("E_ANYTHING_ELSE").contains("unknown"));
        assert!(
            !known_state("E_LOCKED").contains("No work ran"),
            "initialisation may already have happened"
        );
    }
}
