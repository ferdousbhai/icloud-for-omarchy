//! The shared product output contract: human text on a terminal, a JSON
//! envelope when piped. `--json` forces the envelope, `--styled` forces text,
//! `--quiet` prints only `data`, `--ids-only` one id per line, `--count` the
//! number of results.
//!
//! Envelope: `{"ok":true,"data":…,"summary":"…","meta":{…}}`; on failure
//! `{"ok":false,"error":{"code","message","hint"?}}` on stdout (text mode:
//! `error:`/`hint:` lines on stderr). The process exit code carries the number
//! ([`crate::cli`]'s `EXIT_*`).
//!
//! Mirrors cloud-mail's `crates/cloudmail/src/output.rs` and
//! cloud-contacts' `cli_output.py`.

use std::io::{IsTerminal, Write};

use serde::Serialize;
use serde_json::{Value, json};

/// How a result is rendered.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Mode {
    /// Human-readable text (default on a terminal).
    Human,
    /// The full JSON envelope (default when stdout is not a terminal).
    Json,
    /// Only the `data` field as JSON.
    Quiet,
    /// One id per line.
    Ids,
    /// Only the number of results.
    Count,
}

impl Mode {
    /// Anything but [`Mode::Human`]: the output is for a machine.
    pub fn is_machine(self) -> bool {
        self != Mode::Human
    }
}

/// The five global output flags. [`Flags::mode`] resolves them against the
/// terminal. The flags are mutually exclusive; the tools' `clap` groups keep
/// them so, and [`Flags::mode`] picks the first set as a fallback.
#[derive(Debug, Clone, Copy, Default)]
pub struct Flags {
    pub json: bool,
    pub styled: bool,
    pub quiet: bool,
    pub ids_only: bool,
    pub count: bool,
}

impl Flags {
    /// The mode: an explicit flag wins, else text on a terminal and the JSON
    /// envelope when piped.
    pub fn mode(self) -> Mode {
        if self.json {
            Mode::Json
        } else if self.quiet {
            Mode::Quiet
        } else if self.ids_only {
            Mode::Ids
        } else if self.count {
            Mode::Count
        } else if self.styled || std::io::stdout().is_terminal() {
            Mode::Human
        } else {
            Mode::Json
        }
    }
}

/// The global output flags as `clap` arguments, flattened into a tool's
/// parser (`#[command(flatten)] out: OutputFlags`).
#[cfg(feature = "clap")]
#[derive(Debug, Clone, Copy, clap::Args)]
pub struct OutputFlags {
    /// JSON envelope on stdout (the default when piped).
    #[arg(long, global = true, group = "output")]
    pub json: bool,
    /// Human-readable text (the default on a terminal).
    #[arg(long, global = true, group = "output")]
    pub styled: bool,
    /// Only the result data, as JSON.
    #[arg(long, global = true, group = "output")]
    pub quiet: bool,
    /// One id per line.
    #[arg(long = "ids-only", global = true, group = "output")]
    pub ids_only: bool,
    /// Only the number of results.
    #[arg(long, global = true, group = "output")]
    pub count: bool,
}

#[cfg(feature = "clap")]
impl From<OutputFlags> for Flags {
    fn from(f: OutputFlags) -> Self {
        Flags {
            json: f.json,
            styled: f.styled,
            quiet: f.quiet,
            ids_only: f.ids_only,
            count: f.count,
        }
    }
}

#[cfg(feature = "clap")]
impl OutputFlags {
    pub fn mode(self) -> Mode {
        Flags::from(self).mode()
    }
}

/// The output [`Mode`] from the process arguments (before `clap` parses), for
/// reporting a usage error the contract's way.
#[cfg(feature = "clap")]
pub fn argv_mode() -> Mode {
    let mut flags = Flags::default();
    for arg in std::env::args_os().skip(1) {
        match arg.to_str() {
            Some("--json") => flags.json = true,
            Some("--styled") => flags.styled = true,
            Some("--quiet") => flags.quiet = true,
            Some("--ids-only") => flags.ids_only = true,
            Some("--count") => flags.count = true,
            _ => {}
        }
    }
    flags.mode()
}

/// Parses the process arguments. `--help`/`--version` print and give
/// `Err(EXIT_OK)`; a usage error gives `Err(EXIT_USAGE)` after the contract
/// error object (machine modes) or `clap`'s own message (a terminal).
#[cfg(feature = "clap")]
pub fn parse<P: clap::Parser>() -> Result<P, u8> {
    use crate::cli::{EXIT_OK, EXIT_USAGE};
    P::try_parse().map_err(|e| {
        if !e.use_stderr() {
            let _ = e.print();
            return EXIT_OK;
        }
        let mode = argv_mode();
        if !mode.is_machine() {
            let _ = e.print();
            return EXIT_USAGE;
        }
        // clap's error without its usage and "try --help" lines, on one line.
        let rendered = e.render().to_string();
        let first = rendered.split("\n\n").next().unwrap_or_default();
        let message = first.strip_prefix("error: ").unwrap_or(first);
        let message = message.split_whitespace().collect::<Vec<_>>().join(" ");
        CliError::new("usage", EXIT_USAGE, message).report(mode)
    })
}

/// What a command produced; rendered according to the [`Mode`].
#[derive(Debug, Default)]
pub struct Response {
    pub data: Value,
    pub summary: String,
    pub meta: serde_json::Map<String, Value>,
    /// Text for terminals; falls back to `summary` when empty.
    pub human: String,
    /// Ids for `--ids-only`; `--count` uses their number when set, else the
    /// length of `data`.
    pub ids: Option<Vec<String>>,
    /// The command already wrote its output; print nothing more.
    pub silent: bool,
}

impl Response {
    pub fn new(data: impl Serialize, summary: impl Into<String>) -> Self {
        Self {
            data: serde_json::to_value(data).unwrap_or(Value::Null),
            summary: summary.into(),
            ..Default::default()
        }
    }

    /// Text for [`Mode::Human`] (when the summary alone is not enough).
    pub fn human(mut self, text: impl Into<String>) -> Self {
        self.human = text.into();
        self
    }

    pub fn ids(mut self, ids: Vec<String>) -> Self {
        self.ids = Some(ids);
        self
    }

    pub fn meta(mut self, key: &str, value: impl Serialize) -> Self {
        self.meta
            .insert(key.into(), serde_json::to_value(value).unwrap_or(Value::Null));
        self
    }

    fn envelope(&self) -> Value {
        let mut v = json!({ "ok": true, "data": self.data, "summary": self.summary });
        if !self.meta.is_empty() {
            v["meta"] = Value::Object(self.meta.clone());
        }
        v
    }

    fn count(&self) -> usize {
        match (&self.ids, &self.data) {
            (Some(ids), _) => ids.len(),
            (None, Value::Array(a)) => a.len(),
            (None, Value::Null) => 0,
            _ => 1,
        }
    }

    /// Writes the result to stdout in `mode`.
    pub fn print(&self, mode: Mode) {
        if self.silent {
            return;
        }
        let text = match mode {
            Mode::Json => pretty(&self.envelope()),
            Mode::Quiet => pretty(&self.data),
            Mode::Count => self.count().to_string(),
            Mode::Ids => match &self.ids {
                Some(ids) => ids
                    .iter()
                    .map(|id| terminal_safe(id).replace(['\n', '\t'], " "))
                    .collect::<Vec<_>>()
                    .join("\n"),
                None => {
                    eprintln!("note: this command has no ids to list; printing its result instead");
                    pretty(&self.data)
                }
            },
            Mode::Human => terminal_safe(if self.human.trim().is_empty() {
                &self.summary
            } else {
                &self.human
            }),
        };
        write_line(&text);
    }
}

/// A failure, rendered according to the [`Mode`]: the contract error object on
/// stdout for a machine, `error:`/`hint:` on stderr for a person.
#[derive(Debug, Clone)]
pub struct CliError {
    pub code: String,
    pub message: String,
    pub hint: Option<String>,
    pub exit: u8,
}

impl CliError {
    pub fn new(code: &str, exit: u8, message: impl Into<String>) -> Self {
        Self {
            code: code.into(),
            message: message.into(),
            hint: None,
            exit,
        }
    }

    pub fn hint(mut self, hint: impl Into<String>) -> Self {
        self.hint = Some(hint.into());
        self
    }

    pub fn maybe_hint(mut self, hint: Option<String>) -> Self {
        self.hint = hint;
        self
    }

    fn json(&self) -> Value {
        let mut err = json!({ "code": self.code, "message": self.message });
        if let Some(h) = &self.hint {
            err["hint"] = json!(h);
        }
        json!({ "ok": false, "error": err })
    }

    /// Prints the error (machine: stdout; human: stderr) and returns the exit
    /// code.
    pub fn report(&self, mode: Mode) -> u8 {
        if mode.is_machine() {
            write_line(&pretty(&self.json()));
        } else {
            let mut msg = format!("error: {}", self.message);
            if let Some(h) = &self.hint {
                msg.push_str(&format!("\nhint: {h}"));
            }
            eprintln!("{}", terminal_safe(&msg));
        }
        self.exit
    }
}

/// JSON, pretty-printed, with the DEL and C1 controls escaped (terminals act
/// on C1 codes, and strings can carry device names a sender chose).
pub fn pretty(v: &Value) -> String {
    json_safe(&serde_json::to_string_pretty(v).unwrap_or_default())
}

/// Escapes U+007F–U+009F as `\u00XX`. They only occur inside JSON strings, so
/// the JSON is unchanged.
pub fn json_safe(json: &str) -> String {
    let mut out = String::with_capacity(json.len());
    for c in json.chars() {
        if ('\u{7f}'..='\u{9f}').contains(&c) {
            out.push_str(&format!("\\u{:04x}", c as u32));
        } else {
            out.push(c);
        }
    }
    out
}

/// Text safe for a terminal: every control character except newline and tab
/// becomes `?`, so a device name cannot drive the terminal.
pub fn terminal_safe(s: &str) -> String {
    s.chars()
        .map(|c| {
            if c.is_control() && c != '\n' && c != '\t' {
                '?'
            } else {
                c
            }
        })
        .collect()
}

fn write_line(text: &str) {
    if !text.is_empty() {
        // Ignore EPIPE (e.g. `… | head`).
        let _ = writeln!(std::io::stdout(), "{text}");
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mode_resolves_flags_then_the_tty() {
        assert_eq!(
            Flags {
                json: true,
                ..Default::default()
            }
            .mode(),
            Mode::Json
        );
        assert_eq!(
            Flags {
                quiet: true,
                ..Default::default()
            }
            .mode(),
            Mode::Quiet
        );
        assert_eq!(
            Flags {
                ids_only: true,
                ..Default::default()
            }
            .mode(),
            Mode::Ids
        );
        assert_eq!(
            Flags {
                count: true,
                ..Default::default()
            }
            .mode(),
            Mode::Count
        );
        assert_eq!(
            Flags {
                styled: true,
                ..Default::default()
            }
            .mode(),
            Mode::Human
        );
    }

    #[test]
    fn envelope_shape_and_omits_empty_meta() {
        let r = Response::new(json!([1, 2]), "2 things").meta("locate", true);
        let v = r.envelope();
        assert_eq!(v["ok"], true);
        assert_eq!(v["data"], json!([1, 2]));
        assert_eq!(v["summary"], "2 things");
        assert_eq!(v["meta"]["locate"], true);
        assert_eq!(r.count(), 2);
        assert!(Response::new(json!({}), "x").envelope().get("meta").is_none());
    }

    #[test]
    fn count_prefers_ids_then_array_length() {
        assert_eq!(Response::new(json!({ "a": 1 }), "").ids(vec!["x".into()]).count(), 1);
        assert_eq!(Response::new(json!([1, 2, 3]), "").count(), 3);
        assert_eq!(Response::new(Value::Null, "").count(), 0);
        assert_eq!(Response::new(json!({ "a": 1 }), "").count(), 1);
    }

    #[test]
    fn error_object_is_the_contract_shape() {
        let e = CliError::new("not_found", 4, "no such device").hint("list the devices");
        let v = e.json();
        assert_eq!(v["ok"], false);
        assert_eq!(v["error"]["code"], "not_found");
        assert_eq!(v["error"]["message"], "no such device");
        assert_eq!(v["error"]["hint"], "list the devices");
    }

    #[test]
    fn json_output_escapes_c1_controls() {
        let v = json!({ "name": "a\u{9b}2Jb\u{7f}" });
        let out = pretty(&v);
        assert!(!out.contains('\u{9b}') && !out.contains('\u{7f}'), "{out:?}");
        assert_eq!(serde_json::from_str::<Value>(&out).unwrap(), v);
    }
}
