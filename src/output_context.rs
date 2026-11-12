//! Small, shared facts accompanying a business report.
use serde::Serialize;

#[derive(Debug, Clone, Serialize)]
pub struct Context {
    pub mode: String,
    pub help: String,
    pub summary: String,
    pub continuations: Vec<Continuation>,
}

#[derive(Debug, Clone, Serialize)]
pub struct Continuation {
    pub purpose: String,
    pub command: String,
}

impl Context {
    pub fn new(mode: &str, help: &str, summary: impl Into<String>) -> Self {
        Self {
            mode: mode.into(),
            help: help.into(),
            summary: summary.into(),
            continuations: Vec::new(),
        }
    }
}

/// Quote a single argument, including empty strings and embedded apostrophes.
pub fn shell_quote(value: &str) -> String {
    format!("'{}'", value.replace('\'', "'\\''"))
}

pub fn render_context(context: &Context) -> String {
    let mut text = format!("{}\nOutput help: {}", context.summary, context.help);
    for action in &context.continuations {
        text.push_str(&format!("\n{}: {}", action.purpose, action.command));
    }
    text
}
