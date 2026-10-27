//! The `external-http` backend: ships a source to an HTTP parsing service.
//!
//! This backend exists because the builtin parsers cannot read a scanned PDF or
//! a page whose text lives in a canvas. The service is configured entirely from
//! `[parse.external]`; the only secret is the bearer token, and it is read from
//! the environment variable named by `auth_env` at request time. A token is
//! never stored in `wiki.toml` and never appears in error messages.
//!
//! Field order follows [`std::collections::BTreeMap`], i.e. alphabetical by field
//! name, because the config is a map: the service must not depend on TOML key
//! order. The one special value is `@file`, which marks the field that carries
//! the source bytes.

use std::path::Path;
use std::time::Duration;

use serde_json::Value;
use ureq::unversioned::multipart::{Form, Part};

use crate::config::ExternalCfg;
use crate::parse::{Extracted, ParseError, Parser, Result, Source};

/// Stable id recorded in the raw frontmatter.
pub const ID: &str = "external-http";

/// The field value that means "send the source file here".
const FILE_FIELD: &str = "@file";

/// How much of an unexpected response body an error message quotes.
const BODY_PREVIEW_CHARS: usize = 200;

/// Extensions the service is asked about, in the order `pick_backend` uses them.
const HANDLED_EXTS: [&str; 6] = ["md", "markdown", "txt", "html", "htm", "pdf"];

/// A parsing backend that ships the source to an HTTP service.
#[derive(Debug, Clone)]
pub struct ExternalParser {
    cfg: ExternalCfg,
}

impl ExternalParser {
    /// Builds the backend from `[parse.external]`, or explains what is missing.
    ///
    /// Everything that can be checked without the network is checked here, so a
    /// misconfigured wiki fails at the first `convert` instead of in the middle
    /// of a batch, and a test never has to send a request to find out.
    pub fn from_config(external: &Option<ExternalCfg>) -> Result<ExternalParser> {
        let Some(cfg) = external else {
            return Err(ParseError::ExternalConfig {
                message: String::from("no [parse.external] section is configured"),
            });
        };
        if cfg.kind != "http" {
            return Err(ParseError::ExternalConfig {
                message: format!(
                    "unsupported external type {:?}, only \"http\" exists",
                    cfg.kind
                ),
            });
        }
        if cfg.url.trim().is_empty() {
            return Err(ParseError::ExternalConfig {
                message: String::from("[parse.external].url is empty"),
            });
        }
        if cfg.auth_env.trim().is_empty() {
            return Err(ParseError::ExternalConfig {
                message: String::from(
                    "[parse.external].auth_env must name the environment variable holding the token",
                ),
            });
        }
        if cfg.timeout_s == 0 {
            return Err(ParseError::ExternalConfig {
                message: String::from("[parse.external].timeout_s must be greater than zero"),
            });
        }
        if !cfg.fields.values().any(|value| value == FILE_FIELD) {
            return Err(ParseError::ExternalConfig {
                message: format!(
                    "[parse.external].fields needs one field whose value is {FILE_FIELD:?}"
                ),
            });
        }
        Ok(ExternalParser { cfg: cfg.clone() })
    }

    /// The bearer token, read from the environment at request time.
    ///
    /// The error names the variable so the user can fix it, and deliberately
    /// says nothing about the value.
    fn token(&self) -> Result<String> {
        std::env::var(&self.cfg.auth_env).map_err(|_| ParseError::NoAuthEnv {
            name: self.cfg.auth_env.clone(),
        })
    }

    /// The multipart form described by `[parse.external].fields`.
    ///
    /// The form borrows the source bytes and the configured strings, so it must
    /// be sent while this value is alive.
    fn form<'a>(&'a self, file_name: &'a str, bytes: &'a [u8]) -> Form<'a> {
        let mut form = Form::new();
        for (name, value) in &self.cfg.fields {
            if value == FILE_FIELD {
                form = form.part(name, Part::bytes(bytes).file_name(file_name));
            } else {
                form = form.text(name, value);
            }
        }
        form
    }

    /// Sends the source and returns the markdown body of the answer.
    fn call_service(&self, input: &Source) -> Result<String> {
        let token = self.token()?;
        let agent = ureq::Agent::new_with_config(
            ureq::Agent::config_builder()
                .timeout_global(Some(Duration::from_secs(self.cfg.timeout_s)))
                // A 500 is a parse failure like any other, not a transport
                // error: reading the body is what turns it into a message.
                .http_status_as_error(false)
                .build(),
        );
        let file_name = Path::new(&input.key)
            .file_name()
            .map(|name| name.to_string_lossy().into_owned())
            .unwrap_or_else(|| String::from("source"));

        let mut response = agent
            .post(&self.cfg.url)
            .header("Authorization", format!("Bearer {token}"))
            .send(self.form(&file_name, &input.bytes))
            .map_err(|err| ParseError::External {
                message: err.to_string(),
            })?;

        let status = response.status().as_u16();
        let body = response
            .body_mut()
            .read_to_string()
            .map_err(|err| ParseError::External {
                message: format!("cannot read the response body: {err}"),
            })?;
        if status != self.cfg.response.ok_code {
            return Err(ParseError::ExternalStatus {
                status,
                body: preview(&body),
            });
        }
        self.text_from(&body)
    }

    /// Extracts `response.text` from the JSON answer.
    fn text_from(&self, body: &str) -> Result<String> {
        let value: Value = serde_json::from_str(body).map_err(|err| ParseError::External {
            message: format!("the response is not JSON: {err}"),
        })?;
        let path = &self.cfg.response.text;
        let found = lookup(&value, path).ok_or_else(|| ParseError::External {
            message: format!("the response has no {path:?} field"),
        })?;
        match found {
            Value::String(text) => Ok(text.clone()),
            other => Err(ParseError::External {
                message: format!("the {path:?} field is not a string: {other}"),
            }),
        }
    }
}

impl Parser for ExternalParser {
    fn id(&self) -> &'static str {
        ID
    }

    fn can_handle(&self, ext: &str) -> bool {
        let ext = ext.to_ascii_lowercase();
        HANDLED_EXTS.contains(&ext.as_str())
    }

    fn parse(&self, input: &Source) -> Result<Extracted> {
        Ok(Extracted::markdown(self.call_service(input)?))
    }
}

/// Walks a dotted path through objects and array indices.
fn lookup<'v>(value: &'v Value, path: &str) -> Option<&'v Value> {
    if path.is_empty() {
        return None;
    }
    let mut current = value;
    for segment in path.split('.') {
        current = match current {
            Value::Object(map) => map.get(segment)?,
            Value::Array(items) => items.get(segment.parse::<usize>().ok()?)?,
            _ => return None,
        };
    }
    Some(current)
}

/// Quotes the start of an unexpected body without dumping it into the log.
fn preview(body: &str) -> String {
    let mut out: String = body.chars().take(BODY_PREVIEW_CHARS).collect();
    if body.chars().count() > BODY_PREVIEW_CHARS {
        out.push('…');
    }
    out
}
