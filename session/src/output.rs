//! The shared product output contract: human text on a terminal, a JSON
//! envelope when piped. `--json` forces the envelope, `--styled` forces text,
//! `--quiet` prints only `data`, `--ids-only` one id per line, `--count` the
//! number of results. The flags may be combined; the mode is picked by
//! precedence: ids-only > count > quiet > json > styled > (terminal ? text :
//! envelope), as in cloud-mail.
//!
//! Envelope: `{"ok":true,"data":…,"summary":"…","meta":{…}}`; on failure
//! `{"ok":false,"error":{"code","message","hint"?}}` on stdout (text mode:
//! `error:`/`hint:` lines on stderr). `ok` is always the first key. The
//! process exit code carries the number ([`crate::cli`]'s `EXIT_*`).
//!
//! A tool's runner is, in short:
//!
//! ```ignore
//! let args = match output::parse::<Args>() { Ok(a) => a, Err(code) => return code };
//! let mode = args.out.mode();
//! match run_command(args.command) {
//!     Ok(resp) => resp.print(mode),          // returns the exit code (0 unless `.exit(n)`)
//!     Err(e) => e.report(mode),              // a `CliError`; returns its exit code
//! }
//! ```
//!
//! Mirrors cloud-mail's `crates/cloudmail/src/output.rs` and
//! cloud-contacts' `cli_output.py`.

use std::io::{IsTerminal, Write};

use serde::Serialize;
use serde_json::{Map, Value};

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

/// Whether a command may ask the user something: only in text mode with both
/// stdin and stdout a terminal (cloud-mail's `Ctx::interactive`). Everywhere
/// else a command that would ask must refuse instead (e.g. a destructive
/// command without `--yes`: exit 2).
pub fn interactive(mode: Mode) -> bool {
    mode == Mode::Human && std::io::stdin().is_terminal() && std::io::stdout().is_terminal()
}

/// The five global output flags. [`Flags::mode`] resolves them against the
/// terminal. Combinations are allowed and resolved by precedence: ids-only >
/// count > quiet > json > styled.
#[derive(Debug, Clone, Copy, Default)]
pub struct Flags {
    pub json: bool,
    pub styled: bool,
    pub quiet: bool,
    pub ids_only: bool,
    pub count: bool,
}

impl Flags {
    /// The mode: the strongest flag given wins, else text on a terminal and
    /// the JSON envelope when piped.
    pub fn mode(self) -> Mode {
        self.resolve(std::io::stdout().is_terminal())
    }

    fn resolve(self, tty: bool) -> Mode {
        if self.ids_only {
            Mode::Ids
        } else if self.count {
            Mode::Count
        } else if self.quiet {
            Mode::Quiet
        } else if self.json {
            Mode::Json
        } else if self.styled || tty {
            Mode::Human
        } else {
            Mode::Json
        }
    }

    /// Sets the flag `arg` names (`--json`, …); false for anything else.
    fn set(&mut self, arg: &str) -> bool {
        match arg {
            "--json" => self.json = true,
            "--styled" => self.styled = true,
            "--quiet" => self.quiet = true,
            "--ids-only" => self.ids_only = true,
            "--count" => self.count = true,
            _ => return false,
        }
        true
    }
}

/// The global output flags as `clap` arguments, flattened into a tool's
/// parser (`#[command(flatten)] out: OutputFlags`). They may be repeated and
/// combined (see [`Flags`]).
#[cfg(feature = "clap")]
#[derive(Debug, Clone, Copy, clap::Args)]
pub struct OutputFlags {
    /// JSON envelope on stdout (the default when piped).
    #[arg(long, global = true, overrides_with = "json")]
    pub json: bool,
    /// Human-readable text (the default on a terminal).
    #[arg(long, global = true, overrides_with = "styled")]
    pub styled: bool,
    /// Only the result data, as JSON.
    #[arg(long, global = true, overrides_with = "quiet")]
    pub quiet: bool,
    /// One id per line.
    #[arg(long = "ids-only", global = true, overrides_with = "ids_only")]
    pub ids_only: bool,
    /// Only the number of results.
    #[arg(long, global = true, overrides_with = "count")]
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
/// reporting a usage error the contract's way. Stops at `--`.
pub fn argv_mode() -> Mode {
    mode_of(std::env::args_os().skip(1).map(|a| a.to_string_lossy().into_owned())).mode()
}

fn mode_of(args: impl IntoIterator<Item = String>) -> Flags {
    let mut flags = Flags::default();
    for arg in args {
        if arg == "--" {
            break;
        }
        flags.set(&arg);
    }
    flags
}

/// clap's error message on one line, without its `error:` prefix, usage and
/// "try --help" lines.
#[cfg(feature = "clap")]
pub fn usage_message(e: &clap::Error) -> String {
    flatten_clap_message(&e.render().to_string())
}

#[cfg(any(feature = "clap", test))]
fn flatten_clap_message(rendered: &str) -> String {
    let first = rendered.split("\n\n").next().unwrap_or_default();
    let message = first.strip_prefix("error: ").unwrap_or(first);
    message.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// The hint every usage error carries.
pub fn usage_hint(bin: &str) -> String {
    format!("run `{bin} --help`")
}

/// Parses the process arguments. `--help`/`--version` print and give
/// `Err(EXIT_OK)`; a usage error gives `Err(EXIT_USAGE)` after the contract
/// error object with a ``run `<bin> --help` `` hint (machine modes) or
/// `clap`'s own message (a terminal).
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
        let bin = P::command().get_name().to_string();
        CliError::new("usage", EXIT_USAGE, usage_message(&e))
            .hint(usage_hint(&bin))
            .report(mode)
    })
}

/// What a command produced; rendered according to the [`Mode`] by
/// [`Response::print`].
#[derive(Debug, Default)]
pub struct Response {
    data: Value,
    summary: String,
    meta: Map<String, Value>,
    human: String,
    ids: Option<Vec<String>>,
    count: Option<usize>,
    exit: u8,
    silent: bool,
}

/// `{"ok":true,"data","summary","meta"?}`, in this key order.
#[derive(Serialize)]
struct Envelope<'a> {
    ok: bool,
    data: &'a Value,
    summary: &'a str,
    #[serde(skip_serializing_if = "Map::is_empty")]
    meta: &'a Map<String, Value>,
}

impl Response {
    /// `data` (anything `Serialize`; `null` if it cannot be serialized) and
    /// the one-line `summary`, which is also the text-mode output unless
    /// [`Response::human`] gives more.
    pub fn new(data: impl Serialize, summary: impl Into<String>) -> Self {
        Self {
            data: serde_json::to_value(data).unwrap_or(Value::Null),
            summary: summary.into(),
            ..Default::default()
        }
    }

    /// The command already wrote its output: print nothing (exit 0 unless
    /// [`Response::exit`]).
    pub fn silent() -> Self {
        Self {
            silent: true,
            ..Default::default()
        }
    }

    /// Text for [`Mode::Human`] (when the summary alone is not enough).
    pub fn human(mut self, text: impl Into<String>) -> Self {
        self.human = text.into();
        self
    }

    /// The ids for `--ids-only`, one per line. Without them `--ids-only`
    /// prints `data` with a note on stderr.
    pub fn ids(mut self, ids: Vec<String>) -> Self {
        self.ids = Some(ids);
        self
    }

    /// The number `--count` prints. Without it: the number of ids if set,
    /// else the length of an array `data`, 0 for `null`, 1 for anything else.
    pub fn count(mut self, n: usize) -> Self {
        self.count = Some(n);
        self
    }

    /// A non-zero exit code alongside the data, for a batch command that
    /// reports every item but failed some (each item carrying its own
    /// `error`). [`Response::print`] returns it; the runner must exit with it.
    pub fn exit(mut self, code: u8) -> Self {
        self.exit = code;
        self
    }

    /// One `meta` entry of the envelope (omitted when there are none).
    pub fn meta(mut self, key: &str, value: impl Serialize) -> Self {
        self.meta
            .insert(key.into(), serde_json::to_value(value).unwrap_or(Value::Null));
        self
    }

    fn envelope(&self) -> Envelope<'_> {
        Envelope {
            ok: true,
            data: &self.data,
            summary: &self.summary,
            meta: &self.meta,
        }
    }

    fn number(&self) -> usize {
        if let Some(n) = self.count {
            return n;
        }
        match (&self.ids, &self.data) {
            (Some(ids), _) => ids.len(),
            (None, Value::Array(a)) => a.len(),
            (None, Value::Null) => 0,
            _ => 1,
        }
    }

    /// What goes to stdout in `mode`, and a note for stderr if any.
    fn render(&self, mode: Mode) -> (String, Option<&'static str>) {
        if self.silent {
            return (String::new(), None);
        }
        let text = match mode {
            Mode::Json => pretty(&self.envelope()),
            Mode::Quiet => pretty(&self.data),
            Mode::Count => self.number().to_string(),
            Mode::Ids => match &self.ids {
                Some(ids) => ids
                    .iter()
                    .map(|id| terminal_safe(id).replace(['\n', '\t'], " "))
                    .collect::<Vec<_>>()
                    .join("\n"),
                None => {
                    return (
                        pretty(&self.data),
                        Some("note: this command has no ids to list; printing its result instead"),
                    );
                }
            },
            Mode::Human => terminal_safe(if self.human.trim().is_empty() {
                &self.summary
            } else {
                &self.human
            }),
        };
        (text, None)
    }

    /// Writes the result to stdout in `mode` and returns the exit code (0
    /// unless set with [`Response::exit`]).
    pub fn print(&self, mode: Mode) -> u8 {
        let (text, note) = self.render(mode);
        if let Some(note) = note {
            eprintln!("{note}");
        }
        write_line(&text);
        self.exit
    }
}

/// `{"ok":false,"error":{…}}`, in this key order.
#[derive(Serialize)]
pub(crate) struct ErrorEnvelope<'a> {
    ok: bool,
    error: ErrorBody<'a>,
}

/// `{"code","message","hint"?}`, in this key order.
#[derive(Serialize)]
struct ErrorBody<'a> {
    code: &'a str,
    message: &'a str,
    #[serde(skip_serializing_if = "Option::is_none")]
    hint: Option<&'a str>,
}

impl<'a> ErrorEnvelope<'a> {
    pub(crate) fn new(code: &'a str, message: &'a str, hint: Option<&'a str>) -> Self {
        ErrorEnvelope {
            ok: false,
            error: ErrorBody { code, message, hint },
        }
    }
}

/// A failure, rendered according to the [`Mode`]: the contract error object on
/// stdout for a machine, `error:`/`hint:` on stderr for a person. Put the
/// remediation (``run `icloud-session sign-in` ``) in the hint, not the
/// message.
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

    fn envelope(&self) -> ErrorEnvelope<'_> {
        ErrorEnvelope::new(&self.code, &self.message, self.hint.as_deref())
    }

    /// The text-mode lines: `error: …` and maybe `hint: …`.
    fn text(&self) -> String {
        let mut msg = format!("error: {}", self.message);
        if let Some(h) = &self.hint {
            msg.push_str(&format!("\nhint: {h}"));
        }
        terminal_safe(&msg)
    }

    /// Prints the error (machine: stdout; human: stderr) and returns the exit
    /// code.
    pub fn report(&self, mode: Mode) -> u8 {
        if mode.is_machine() {
            write_line(&pretty(&self.envelope()));
        } else {
            eprintln!("{}", self.text());
        }
        self.exit
    }
}

/// JSON, pretty-printed, with the DEL and C1 controls escaped (terminals act
/// on C1 codes, and strings can carry device names a sender chose).
pub fn pretty<T: Serialize + ?Sized>(v: &T) -> String {
    json_safe(&serde_json::to_string_pretty(v).unwrap_or_default())
}

/// [`pretty`], on one line.
pub fn compact<T: Serialize + ?Sized>(v: &T) -> String {
    json_safe(&serde_json::to_string(v).unwrap_or_default())
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
    use serde_json::json;

    fn flags(names: &[&str]) -> Flags {
        mode_of(names.iter().map(|s| s.to_string()))
    }

    #[test]
    fn mode_resolves_flags_by_precedence_then_the_tty() {
        let m = |names: &[&str], tty| flags(names).resolve(tty);
        assert_eq!(m(&[], true), Mode::Human);
        assert_eq!(m(&[], false), Mode::Json);
        assert_eq!(m(&["--json"], true), Mode::Json);
        assert_eq!(m(&["--styled"], false), Mode::Human);
        assert_eq!(m(&["--quiet"], true), Mode::Quiet);
        assert_eq!(m(&["--ids-only"], true), Mode::Ids);
        assert_eq!(m(&["--count"], true), Mode::Count);
        // Combinations: ids-only > count > quiet > json > styled.
        assert_eq!(m(&["--json", "--ids-only", "--count", "--quiet"], false), Mode::Ids);
        assert_eq!(m(&["--count", "--quiet", "--json"], false), Mode::Count);
        assert_eq!(m(&["--json", "--quiet"], false), Mode::Quiet);
        assert_eq!(m(&["--styled", "--json"], true), Mode::Json);
        // Nothing after `--` is a flag.
        assert_eq!(m(&["--", "--json"], true), Mode::Human);
        assert_eq!(m(&["locate", "--quiet", "--", "--count"], true), Mode::Quiet);
    }

    #[test]
    fn interactive_only_in_text_mode() {
        for mode in [Mode::Json, Mode::Quiet, Mode::Ids, Mode::Count] {
            assert!(!interactive(mode));
        }
    }

    #[test]
    fn envelope_is_ok_data_summary_meta_in_order() {
        let r = Response::new(json!({ "z": 1, "a": 2 }), "2 things").meta("locate", true);
        let s = compact(&r.envelope());
        assert_eq!(
            s,
            r#"{"ok":true,"data":{"a":2,"z":1},"summary":"2 things","meta":{"locate":true}}"#
        );
        let s = compact(&Response::new(json!([]), "x").envelope());
        assert_eq!(s, r#"{"ok":true,"data":[],"summary":"x"}"#);
        // What `--json` prints parses back to the same.
        let (out, note) = r.render(Mode::Json);
        assert!(note.is_none());
        let v: Value = serde_json::from_str(&out).unwrap();
        assert_eq!(v["data"]["z"], 1);
        assert!(out.trim_start().starts_with("{\n  \"ok\": true,"), "{out}");
    }

    #[test]
    fn error_is_ok_error_code_message_hint_in_order() {
        let e = CliError::new("not_found", 4, "no such device").hint("list the devices");
        assert_eq!(
            compact(&e.envelope()),
            r#"{"ok":false,"error":{"code":"not_found","message":"no such device","hint":"list the devices"}}"#
        );
        let e = CliError::new("usage", 2, "bad");
        assert_eq!(
            compact(&e.envelope()),
            r#"{"ok":false,"error":{"code":"usage","message":"bad"}}"#
        );
        assert_eq!(e.text(), "error: bad");
        let e = CliError::new("x", 1, "a\u{1b}[2Jb").hint("do \u{9b}this");
        assert_eq!(e.text(), "error: a?[2Jb\nhint: do ?this");
    }

    #[test]
    fn count_override_then_ids_then_array_length() {
        assert_eq!(Response::new(json!({ "a": 1 }), "").ids(vec!["x".into()]).number(), 1);
        assert_eq!(Response::new(json!([1, 2, 3]), "").number(), 3);
        assert_eq!(Response::new(Value::Null, "").number(), 0);
        assert_eq!(Response::new(json!({ "a": 1 }), "").number(), 1);
        let r = Response::new(json!({ "points": [1, 2, 3, 4] }), "").count(4);
        assert_eq!(r.render(Mode::Count).0, "4");
        let r = Response::new(json!([1]), "").ids(vec!["a".into(), "b".into()]).count(0);
        assert_eq!(r.render(Mode::Count).0, "0");
    }

    #[test]
    fn quiet_is_data_and_human_falls_back_to_summary() {
        let r = Response::new(json!({ "a": 1 }), "one thing");
        assert_eq!(
            serde_json::from_str::<Value>(&r.render(Mode::Quiet).0).unwrap(),
            json!({ "a": 1 })
        );
        assert_eq!(r.render(Mode::Human).0, "one thing");
        let r = r.human("line 1\n\tline 2 \u{1b}[31m");
        assert_eq!(r.render(Mode::Human).0, "line 1\n\tline 2 ?[31m");
    }

    #[test]
    fn ids_are_one_per_line_and_fall_back_to_data() {
        let r = Response::new(json!([]), "").ids(vec!["a".into(), "b\nc".into(), "d\u{1b}e".into()]);
        assert_eq!(r.render(Mode::Ids), ("a\nb c\nd?e".to_string(), None));
        let (out, note) = Response::new(json!({ "k": "v" }), "").render(Mode::Ids);
        assert_eq!(serde_json::from_str::<Value>(&out).unwrap(), json!({ "k": "v" }));
        assert!(note.unwrap().starts_with("note: "));
    }

    #[test]
    fn exit_rides_along_with_the_data() {
        assert_eq!(Response::new(json!([]), "").exit, 0);
        let r = Response::new(json!([{ "id": "a", "error": "x" }]), "1 failed").exit(1);
        assert_eq!(r.exit, 1);
        assert!(r.render(Mode::Json).0.contains("\"ok\": true"));
        let s = Response::silent().exit(3);
        assert_eq!((s.render(Mode::Json).0.as_str(), s.exit), ("", 3));
    }

    #[test]
    fn clap_messages_flatten_to_one_line() {
        let rendered = "error: the following required arguments were not provided:\n  <NAME|ID>\n\n\
                        Usage: icloud-findmy locate <NAME|ID>\n\nFor more information, try '--help'.\n";
        assert_eq!(
            flatten_clap_message(rendered),
            "the following required arguments were not provided: <NAME|ID>"
        );
        assert_eq!(flatten_clap_message("no prefix here"), "no prefix here");
        assert_eq!(usage_hint("icloud-findmy"), "run `icloud-findmy --help`");
    }

    #[cfg(feature = "clap")]
    #[test]
    fn usage_message_from_a_real_clap_error() {
        use clap::Parser;
        #[derive(Parser, Debug)]
        #[command(name = "t")]
        struct T {
            #[command(flatten)]
            out: OutputFlags,
            name: String,
        }
        let e = T::try_parse_from(["t"]).unwrap_err();
        assert_eq!(
            usage_message(&e),
            "the following required arguments were not provided: <NAME>"
        );
        // Output flags combine and repeat without a usage error.
        let t = T::try_parse_from(["t", "--json", "x", "--quiet", "--quiet", "--styled", "--count"]).unwrap();
        assert_eq!(Flags::from(t.out).resolve(false), Mode::Count);
    }

    #[test]
    fn terminal_safe_keeps_newline_and_tab_only() {
        assert_eq!(terminal_safe("a\u{1b}[2J\u{7}b\u{9b}c\u{0}\n\td"), "a?[2J?b?c?\n\td");
        assert_eq!(terminal_safe("Dous’s iPhone"), "Dous’s iPhone");
    }

    #[test]
    fn json_output_escapes_c1_controls() {
        let v = json!({ "name": "a\u{9b}2Jb\u{7f}" });
        let out = pretty(&v);
        assert!(!out.contains('\u{9b}') && !out.contains('\u{7f}'), "{out:?}");
        assert_eq!(serde_json::from_str::<Value>(&out).unwrap(), v);
    }
}
