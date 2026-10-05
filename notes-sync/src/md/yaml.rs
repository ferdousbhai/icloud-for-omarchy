//! A narrow port of the `yaml` package (2.9.0, ISC, Eemeli Aro) - just
//! enough of `parseDocument` → `Document.set/delete` → `String(doc)` to
//! reproduce icloud-md's frontmatter edits byte for byte on the YAML
//! Obsidian-style frontmatter actually holds:
//!
//! - a block mapping (or nothing) at the top level, nested block mappings;
//! - keys: plain scalars that resolve to strings;
//! - values: plain / single-quoted / double-quoted one-line scalars
//!   (strings, and core-schema null, bool, int, float), block sequences of
//!   those, and one-line flow sequences/mappings of those;
//! - blank lines (kept as yaml's `spaceBefore`).
//!
//! Anything else - comments, block scalars, anchors, aliases, tags,
//! multi-line flow collections or plain scalars, directives - is
//! [`Parsed::Unsupported`]: the caller then falls back to a line-level edit
//! (see `frontmatter`). A document yaml rejects outright (e.g. duplicate
//! keys) is [`Parsed::Invalid`].
//!
//! The stringifier is a port of `stringifyString` (plain, single- and
//! double-quoted), `foldFlowLines`, `stringifyNumber`, `stringifyPair`,
//! `stringifyCollection` and `stringifyDocument` with the default options.

use crate::js;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Style {
    Plain,
    Single,
    Double,
    /// `|` block scalar.
    Literal,
    /// `>` block scalar.
    Folded,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NumFormat {
    Decimal,
    Hex,
    Oct,
    Exp,
}

#[derive(Debug, Clone, PartialEq)]
pub enum Scalar {
    Str {
        value: String,
        style: Style,
    },
    Null {
        source: String,
    },
    Bool {
        source: String,
        value: bool,
    },
    Num {
        value: f64,
        format: NumFormat,
        min_fraction_digits: Option<usize>,
    },
}

#[derive(Debug, Clone, PartialEq)]
pub enum Value {
    Scalar(Scalar),
    Seq {
        flow: bool,
        items: Vec<Item>,
        space_before: bool,
    },
    Map {
        flow: bool,
        pairs: Vec<Pair>,
        space_before: bool,
    },
}

#[derive(Debug, Clone, PartialEq)]
pub struct Item {
    pub value: Value,
    pub space_before: bool,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Pair {
    pub key: String,
    pub value: Value,
    /// The key node's `spaceBefore`.
    pub space_before: bool,
    /// A pair made by `Document.set` holds raw JS values, not nodes: its
    /// scalar value gets no `indentAtStart` when folded.
    pub raw: bool,
}

/// A parsed frontmatter document: `None` contents is an empty document.
#[derive(Debug, Clone, PartialEq)]
pub struct Document {
    pub contents: Option<Vec<Pair>>,
}

#[derive(Debug, Clone, PartialEq)]
pub enum Parsed {
    Ok(Document),
    /// yaml reports errors for this text.
    Invalid,
    /// Valid or not, outside what this module models.
    Unsupported,
}

// --- parsing -------------------------------------------------------------

#[derive(Clone, Copy)]
struct Line<'a> {
    indent: usize,
    /// Without indentation and trailing whitespace.
    text: &'a str,
}

/// `parseDocument(text)` restricted to the subset above.
pub fn parse_document(text: &str) -> Parsed {
    let mut lines: Vec<Option<Line>> = Vec::new();
    let mut raws: Vec<&str> = Vec::new();
    // A final line break ends the last line; it doesn't start another.
    let body = text.strip_suffix('\n').unwrap_or(text);
    for raw in body.split('\n') {
        let raw = raw.strip_suffix('\r').unwrap_or(raw);
        if raw.contains('\r') {
            return Parsed::Unsupported;
        }
        raws.push(raw);
        let trimmed = raw.trim_end_matches([' ', '\t']);
        if trimmed.is_empty() {
            lines.push(None);
            continue;
        }
        let indent = trimmed.len() - trimmed.trim_start_matches(' ').len();
        lines.push(Some(Line {
            indent,
            text: &trimmed[indent..],
        }));
    }
    let mut parser = BlockParser { lines, raws, at: 0 };
    parser.skip_blank();
    if parser.at >= parser.lines.len() {
        return Parsed::Ok(Document { contents: None });
    }
    let first = parser.lines[parser.at].unwrap();
    if first.indent != 0 {
        return Parsed::Unsupported;
    }
    // A bare scalar or a sequence isn't a mapping icloud-md can set keys on.
    if (first.text == "-" || first.text.starts_with("- "))
        || (split_key(first.text).is_none() && !is_structural(first.text))
    {
        return Parsed::Invalid;
    }
    match parser.block_map(0) {
        Ok(pairs) => {
            parser.skip_blank();
            if parser.at < parser.lines.len() {
                return Parsed::Unsupported;
            }
            Parsed::Ok(Document { contents: Some(pairs) })
        }
        Err(failure) => failure,
    }
}

/// Open `[`/`{` minus closing ones, outside quotes.
fn bracket_depth(text: &str) -> isize {
    let mut depth = 0;
    let mut quote: Option<char> = None;
    let mut token_start = true;
    for c in text.chars() {
        match (quote, c) {
            (Some(q), c) if c == q => quote = None,
            (Some(_), _) => {}
            (None, '\'' | '"') if token_start => quote = Some(c),
            (None, '[' | '{') => depth += 1,
            (None, ']' | '}') => depth -= 1,
            _ => {}
        }
        if quote.is_none() && c != ' ' {
            token_start = matches!(c, '[' | '{' | ',' | ':');
        }
    }
    depth
}

/// Lines this module doesn't model at all (comments, directives, markers,
/// tab indentation).
fn is_structural(text: &str) -> bool {
    text.starts_with(['#', '%', '\t']) || text.starts_with("---") || text.starts_with("...")
}

struct BlockParser<'a> {
    lines: Vec<Option<Line<'a>>>,
    raws: Vec<&'a str>,
    at: usize,
}

impl<'a> BlockParser<'a> {
    /// Skips blank lines; whether there were any.
    fn skip_blank(&mut self) -> bool {
        let start = self.at;
        while self.at < self.lines.len() && self.lines[self.at].is_none() {
            self.at += 1;
        }
        self.at > start
    }

    fn peek(&self) -> Option<Line<'a>> {
        self.lines.get(self.at).copied().flatten()
    }

    /// A `|`/`>` block scalar under a key at `parent` indentation.
    fn block_scalar(&mut self, parent: usize, header: &str) -> Result<Value, Parsed> {
        let style = if header.starts_with('|') {
            Style::Literal
        } else {
            Style::Folded
        };
        let chomp = match &header[1..] {
            "" => ' ',
            "-" => '-',
            "+" => '+',
            _ => return Err(Parsed::Unsupported),
        };
        // Content indentation: the first non-blank line's.
        let mut look = self.at;
        while look < self.lines.len() && self.lines[look].is_none() {
            look += 1;
        }
        let content_indent = self
            .lines
            .get(look)
            .copied()
            .flatten()
            .map(|l| l.indent)
            .filter(|&n| n > parent);
        let mut content: Vec<&str> = Vec::new();
        let mut end = self.at;
        if let Some(n) = content_indent {
            while end < self.lines.len() {
                match self.lines[end] {
                    None => {
                        if self.raws[end].len() > n {
                            return Err(Parsed::Unsupported);
                        }
                        content.push("");
                    }
                    Some(line) if line.indent >= n => content.push(&self.raws[end][n..]),
                    // Less indented than the content, more than the key: an error.
                    Some(line) if line.indent > parent => return Err(Parsed::Invalid),
                    Some(_) => break,
                }
                end += 1;
            }
        }
        // Trailing blank lines belong to the scalar only when kept.
        let trailing = content.iter().rev().take_while(|l| l.is_empty()).count();
        if trailing > 0 && chomp != '+' {
            if end < self.lines.len() {
                return Err(Parsed::Unsupported);
            }
            content.truncate(content.len() - trailing);
            end -= trailing;
        }
        self.at = end;
        let lines = &content[..content.len() - if chomp == '+' { trailing } else { 0 }];
        let mut value = String::new();
        if style == Style::Literal {
            for line in lines {
                value.push_str(line);
                value.push('\n');
            }
        } else {
            let mut previous_text = false;
            let mut pending = 0;
            for line in lines {
                if line.is_empty() {
                    pending += 1;
                    continue;
                }
                if line.starts_with([' ', '\t']) {
                    return Err(Parsed::Unsupported);
                }
                if pending > 0 {
                    value.push_str(&"\n".repeat(pending));
                } else if previous_text {
                    value.push(' ');
                }
                value.push_str(line);
                pending = 0;
                previous_text = true;
            }
            if !lines.is_empty() {
                value.push_str(&"\n".repeat(pending + 1));
            }
        }
        match chomp {
            '-' => value.truncate(value.trim_end_matches('\n').len()),
            ' ' => {
                value.truncate(value.trim_end_matches('\n').len());
                if !value.is_empty() {
                    value.push('\n');
                }
            }
            _ => value.push_str(&"\n".repeat(trailing)),
        }
        if value.is_empty() && chomp == '+' {
            return Err(Parsed::Unsupported);
        }
        Ok(Value::Scalar(Scalar::Str { value, style }))
    }

    /// A quoted scalar continued over following lines, with YAML's flow
    /// folding: a line break is a space, blank lines are line breaks,
    /// continuation indentation and trailing whitespace are dropped, and in
    /// double quotes `\` at the end of a line joins the lines.
    fn multi_line_quoted(&mut self, first: &str, parent: usize) -> Result<Value, Parsed> {
        let mut raw = first.to_string();
        loop {
            if self.at >= self.lines.len() {
                return Err(Parsed::Invalid);
            }
            let next = self.raws[self.at];
            if let Some(line) = self.lines[self.at]
                && line.indent <= parent
            {
                return Err(Parsed::Invalid);
            }
            self.at += 1;
            raw.push('\n');
            raw.push_str(next);
            if let Ok((scalar, used)) = quoted_multi(&raw) {
                if !raw[used..].trim_matches([' ', '\t']).is_empty() {
                    return Err(Parsed::Unsupported);
                }
                return Ok(Value::Scalar(scalar));
            }
        }
    }

    /// A flow collection, possibly continued over following lines.
    fn flow_value(&mut self, first: &str) -> Result<Value, Parsed> {
        let mut text = first.to_string();
        let mut depth = bracket_depth(first);
        while depth > 0 {
            // Unclosed: yaml continues it on the next lines (folded to spaces),
            // and a collection still open at the end is an error.
            if self.at >= self.lines.len() {
                return Err(Parsed::Invalid);
            }
            let next = self.lines[self.at].map_or("", |l| l.text);
            self.at += 1;
            text.push(' ');
            text.push_str(next);
            depth = bracket_depth(&text);
        }
        let mut p = FlowParser {
            s: text.as_bytes(),
            text: &text,
            at: 0,
        };
        let value = p.collection()?;
        p.skip_spaces();
        if p.at == p.s.len() {
            Ok(value)
        } else {
            Err(Parsed::Unsupported)
        }
    }

    fn block_map(&mut self, indent: usize) -> Result<Vec<Pair>, Parsed> {
        let mut pairs: Vec<Pair> = Vec::new();
        let mut first = true;
        loop {
            let save = self.at;
            let blank = self.skip_blank();
            let Some(line) = self.peek() else {
                self.at = save;
                break;
            };
            if line.indent < indent {
                self.at = save;
                break;
            }
            if line.indent > indent {
                return Err(Parsed::Unsupported);
            }
            let text = line.text;
            if is_structural(text) {
                return Err(Parsed::Unsupported);
            }
            let (key, rest) = split_key(text).ok_or(Parsed::Unsupported)?;
            if pairs.iter().any(|p| p.key == key) {
                return Err(Parsed::Invalid);
            }
            self.at += 1;
            let value = if rest.is_empty() {
                self.nested_value(indent)?
            } else if rest.starts_with(['|', '>']) {
                self.block_scalar(indent, rest)?
            } else if rest.starts_with(['[', '{']) {
                self.flow_value(rest)?
            } else if rest.starts_with(['"', '\'']) && quoted(rest).is_err() {
                self.multi_line_quoted(rest, indent)?
            } else {
                inline_value(rest)?
            };
            pairs.push(Pair {
                key,
                value,
                space_before: blank && !first,
                raw: false,
            });
            first = false;
        }
        Ok(pairs)
    }

    /// The value of `key:` with nothing after the colon.
    fn nested_value(&mut self, indent: usize) -> Result<Value, Parsed> {
        let save = self.at;
        let blank = self.skip_blank();
        let Some(line) = self.peek() else {
            self.at = save;
            return Ok(Value::Scalar(Scalar::Null { source: String::new() }));
        };
        let is_item = line.text == "-" || line.text.starts_with("- ");
        if is_item && line.indent >= indent {
            let seq_indent = line.indent;
            let items = self.block_seq(seq_indent)?;
            return Ok(Value::Seq {
                flow: false,
                items,
                space_before: blank,
            });
        }
        if line.indent > indent {
            let map_indent = line.indent;
            let pairs = self.block_map(map_indent)?;
            return Ok(Value::Map {
                flow: false,
                pairs,
                space_before: blank,
            });
        }
        self.at = save;
        Ok(Value::Scalar(Scalar::Null { source: String::new() }))
    }

    fn block_seq(&mut self, indent: usize) -> Result<Vec<Item>, Parsed> {
        let mut items = Vec::new();
        let mut first = true;
        loop {
            let save = self.at;
            let blank = self.skip_blank();
            let Some(line) = self.peek() else {
                self.at = save;
                break;
            };
            let is_item = line.text == "-" || line.text.starts_with("- ");
            if line.indent != indent || !is_item {
                if line.indent > indent {
                    return Err(Parsed::Unsupported);
                }
                self.at = save;
                break;
            }
            let rest = line.text[1..].trim_start_matches(' ');
            self.at += 1;
            let value = if rest.is_empty() {
                // `-` alone: a null item, unless a nested block follows.
                if self.peek().is_some_and(|l| l.indent > indent) {
                    return Err(Parsed::Unsupported);
                }
                Value::Scalar(Scalar::Null { source: String::new() })
            } else {
                if split_key(rest).is_some() || rest.starts_with("- ") || rest == "-" {
                    return Err(Parsed::Unsupported);
                }
                inline_value(rest)?
            };
            items.push(Item {
                value,
                space_before: blank && !first,
            });
            first = false;
        }
        Ok(items)
    }
}

/// `key: rest` → `(key, rest)` for a plain key that resolves to a string.
fn split_key(text: &str) -> Option<(String, &str)> {
    let bytes = text.as_bytes();
    let mut colon = None;
    for i in 0..bytes.len() {
        if bytes[i] == b':' && (i + 1 == bytes.len() || bytes[i + 1] == b' ') {
            colon = Some(i);
            break;
        }
        if bytes[i] == b'#' && i > 0 && bytes[i - 1] == b' ' {
            return None;
        }
    }
    let colon = colon?;
    let key = text[..colon].trim_end_matches(' ');
    if key.is_empty() || !plain_start_ok(key) || key.contains(": ") || key.contains(" #") {
        return None;
    }
    if !matches!(resolve_plain(key), Scalar::Str { .. }) {
        return None;
    }
    Some((key.to_string(), text[colon + 1..].trim_start_matches(' ')))
}

/// A plain scalar can't start with an indicator (`?`, `:`, `-` only when
/// followed by a space).
fn plain_start_ok(text: &str) -> bool {
    let first = text.as_bytes()[0];
    if b",[]{}#&*!|>'\"%@`".contains(&first) {
        return false;
    }
    if matches!(first, b'?' | b':' | b'-') {
        return text.len() > 1 && text.as_bytes()[1] != b' ';
    }
    true
}

fn inline_value(rest: &str) -> Result<Value, Parsed> {
    let first = rest.as_bytes()[0];
    match first {
        b'[' | b'{' => {
            let mut p = FlowParser {
                s: rest.as_bytes(),
                text: rest,
                at: 0,
            };
            let value = p.collection()?;
            p.skip_spaces();
            if p.at != p.s.len() {
                return Err(Parsed::Unsupported);
            }
            Ok(value)
        }
        b'\'' | b'"' => {
            let (scalar, used) = quoted(rest)?;
            if !rest[used..].trim_start_matches(' ').is_empty() {
                return Err(Parsed::Unsupported);
            }
            Ok(Value::Scalar(scalar))
        }
        _ => {
            // yaml rejects a nested mapping or a sequence item on the key's line.
            if rest.contains(": ") || rest.ends_with(':') || rest == "-" || rest.starts_with("- ") {
                return Err(Parsed::Invalid);
            }
            if !plain_start_ok(rest) || rest.contains(" #") {
                return Err(Parsed::Unsupported);
            }
            Ok(Value::Scalar(resolve_plain(rest)))
        }
    }
}

/// A quoted scalar spanning lines: fold them, then read it as one line.
fn quoted_multi(text: &str) -> Result<(Scalar, usize), Parsed> {
    let quote = text.as_bytes()[0];
    let double = quote == b'"';
    // Find the closing quote first (so the rest can be checked).
    let bytes = text.as_bytes();
    let mut i = 1;
    let close = loop {
        if i >= bytes.len() {
            return Err(Parsed::Unsupported);
        }
        match bytes[i] {
            b'\\' if double => i += 2,
            b'\'' if !double && bytes.get(i + 1) == Some(&b'\'') => i += 2,
            c if c == quote => break i,
            _ => i += 1,
        }
    };
    let inner = &text[1..close];
    // Fold the lines of the quoted content.
    let lines: Vec<&str> = inner.split('\n').collect();
    let mut folded = String::new();
    let mut k = 0;
    while k < lines.len() {
        let mut line = lines[k];
        if k > 0 {
            line = line.trim_start_matches([' ', '\t']);
        }
        let last = k + 1 == lines.len();
        if !last {
            let escaped_break = double && line.ends_with('\\') && !line.ends_with("\\\\");
            if escaped_break {
                folded.push_str(&line[..line.len() - 1]);
                k += 1;
                continue;
            }
            let keep_escaped_space = double && line.ends_with("\\ ");
            let trimmed = if keep_escaped_space {
                line
            } else {
                line.trim_end_matches([' ', '\t'])
            };
            folded.push_str(trimmed);
            // Blank continuation lines are line breaks; otherwise a space.
            let mut blanks = 0;
            while k + 1 + blanks < lines.len() - 1 && lines[k + 1 + blanks].trim_matches([' ', '\t']).is_empty() {
                blanks += 1;
            }
            if blanks > 0 {
                folded.push_str(&"\n".repeat(blanks));
                k += 1 + blanks;
                continue;
            }
            folded.push(' ');
        } else {
            folded.push_str(line);
        }
        k += 1;
    }
    let one_line = format!("{}{folded}{}", quote as char, quote as char);
    // Escaped newlines inside double quotes were folded above; read the rest.
    let one_line = if double {
        one_line.replace('\n', "\\n")
    } else {
        one_line
    };
    let (scalar, _) = quoted(&one_line)?;
    Ok((scalar, close + 1))
}

/// A quoted scalar at the start of `text`: `(scalar, bytes used)`.
fn quoted(text: &str) -> Result<(Scalar, usize), Parsed> {
    let quote = text.as_bytes()[0];
    let mut value = String::new();
    let mut chars = text[1..].char_indices();
    while let Some((i, c)) = chars.next() {
        if quote == b'\'' {
            if c == '\'' {
                if text[1 + i + 1..].starts_with('\'') {
                    value.push('\'');
                    chars.next();
                    continue;
                }
                return Ok((
                    Scalar::Str {
                        value,
                        style: Style::Single,
                    },
                    1 + i + 1,
                ));
            }
            value.push(c);
            continue;
        }
        match c {
            '"' => {
                return Ok((
                    Scalar::Str {
                        value,
                        style: Style::Double,
                    },
                    1 + i + 1,
                ));
            }
            '\\' => {
                let (_, e) = chars.next().ok_or(Parsed::Invalid)?;
                let hex = |chars: &mut std::str::CharIndices, n: usize| -> Result<char, Parsed> {
                    let mut code = 0u32;
                    for _ in 0..n {
                        let (_, h) = chars.next().ok_or(Parsed::Invalid)?;
                        code = code * 16 + h.to_digit(16).ok_or(Parsed::Invalid)?;
                    }
                    char::from_u32(code).ok_or(Parsed::Unsupported)
                };
                value.push(match e {
                    '0' => '\0',
                    'a' => '\u{7}',
                    'b' => '\u{8}',
                    't' | '\t' => '\t',
                    'n' => '\n',
                    'v' => '\u{b}',
                    'f' => '\u{c}',
                    'r' => '\r',
                    'e' => '\u{1b}',
                    ' ' => ' ',
                    '"' => '"',
                    '/' => '/',
                    '\\' => '\\',
                    'N' => '\u{85}',
                    '_' => '\u{a0}',
                    'L' => '\u{2028}',
                    'P' => '\u{2029}',
                    'x' => hex(&mut chars, 2)?,
                    'u' => hex(&mut chars, 4)?,
                    'U' => hex(&mut chars, 8)?,
                    _ => return Err(Parsed::Invalid),
                });
            }
            c => value.push(c),
        }
    }
    // Unterminated on this line: a multi-line quoted scalar or an error.
    Err(Parsed::Unsupported)
}

/// Core-schema resolution of a plain scalar (`findScalarTagByTest`).
pub fn resolve_plain(text: &str) -> Scalar {
    let source = text.to_string();
    if matches!(text, "" | "~" | "null" | "Null" | "NULL") {
        return Scalar::Null { source };
    }
    if matches!(text, "true" | "True" | "TRUE" | "false" | "False" | "FALSE") {
        let value = text.starts_with(['t', 'T']);
        return Scalar::Bool { source, value };
    }
    let digits = |s: &str, radix: u32| !s.is_empty() && s.chars().all(|c| c.is_digit(radix));
    if let Some(oct) = text.strip_prefix("0o")
        && digits(oct, 8)
    {
        return int_radix(text, oct, 8, NumFormat::Oct);
    }
    let unsigned = text.strip_prefix(['-', '+']).unwrap_or(text);
    if digits(unsigned, 10) {
        let value: f64 = unsigned.parse().unwrap_or(f64::NAN);
        let value = if text.starts_with('-') { -value } else { value };
        return Scalar::Num {
            value,
            format: NumFormat::Decimal,
            min_fraction_digits: None,
        };
    }
    if let Some(hex) = text.strip_prefix("0x")
        && digits(hex, 16)
    {
        return int_radix(text, hex, 16, NumFormat::Hex);
    }
    if matches!(unsigned, ".inf" | ".Inf" | ".INF") {
        let value = if text.starts_with('-') {
            f64::NEG_INFINITY
        } else {
            f64::INFINITY
        };
        return Scalar::Num {
            value,
            format: NumFormat::Decimal,
            min_fraction_digits: None,
        };
    }
    if matches!(text, ".nan" | ".NaN" | ".NAN") {
        return Scalar::Num {
            value: f64::NAN,
            format: NumFormat::Decimal,
            min_fraction_digits: None,
        };
    }
    // `[-+]?(?:\.[0-9]+|[0-9]+(?:\.[0-9]*)?)`
    let mantissa_ok = |m: &str| match m.split_once('.') {
        Some(("", frac)) => digits(frac, 10),
        Some((int, frac)) => digits(int, 10) && (frac.is_empty() || digits(frac, 10)),
        None => digits(m, 10),
    };
    if let Some((mantissa, exponent)) = unsigned.split_once(['e', 'E'])
        && mantissa_ok(mantissa)
        && digits(exponent.strip_prefix(['-', '+']).unwrap_or(exponent), 10)
    {
        let value = parse_float(text);
        return Scalar::Num {
            value,
            format: NumFormat::Exp,
            min_fraction_digits: None,
        };
    }
    if unsigned.contains('.') && mantissa_ok(unsigned) {
        let value = parse_float(text);
        let dot = text.find('.').unwrap();
        let min_fraction_digits = text.ends_with('0').then(|| text.len() - dot - 1);
        return Scalar::Num {
            value,
            format: NumFormat::Decimal,
            min_fraction_digits,
        };
    }
    Scalar::Str {
        value: source,
        style: Style::Plain,
    }
}

fn parse_float(text: &str) -> f64 {
    let t = text.strip_prefix('+').unwrap_or(text);
    let t = if let Some(rest) = t.strip_prefix("-.") {
        format!("-0.{rest}")
    } else {
        t.to_string()
    };
    let t = if let Some(rest) = t.strip_prefix('.') {
        format!("0.{rest}")
    } else {
        t
    };
    let t = t.replace(".e", ".0e").replace(".E", ".0E");
    let t = if t.ends_with('.') { format!("{t}0") } else { t };
    t.parse().unwrap_or(f64::NAN)
}

fn int_radix(text: &str, digits: &str, radix: u32, format: NumFormat) -> Scalar {
    match u64::from_str_radix(digits, radix) {
        Ok(v) if v < (1u64 << 53) => Scalar::Num {
            value: v as f64,
            format,
            min_fraction_digits: None,
        },
        _ => Scalar::Str {
            value: text.to_string(),
            style: Style::Plain,
        }, // not modelled
    }
}

struct FlowParser<'a> {
    s: &'a [u8],
    text: &'a str,
    at: usize,
}

impl FlowParser<'_> {
    fn skip_spaces(&mut self) {
        while self.at < self.s.len() && self.s[self.at] == b' ' {
            self.at += 1;
        }
    }

    fn collection(&mut self) -> Result<Value, Parsed> {
        let open = self.s[self.at];
        let close = if open == b'[' { b']' } else { b'}' };
        self.at += 1;
        let mut items = Vec::new();
        let mut pairs: Vec<Pair> = Vec::new();
        loop {
            self.skip_spaces();
            if self.at >= self.s.len() {
                return Err(Parsed::Unsupported);
            }
            if self.s[self.at] == close {
                self.at += 1;
                break;
            }
            if open == b'{' {
                let Scalar::Str {
                    value: key,
                    style: Style::Plain,
                } = self.scalar()?
                else {
                    return Err(Parsed::Unsupported);
                };
                self.skip_spaces();
                if self.s.get(self.at) != Some(&b':') {
                    return Err(Parsed::Unsupported);
                }
                self.at += 1;
                self.skip_spaces();
                let value = self.value()?;
                if pairs.iter().any(|p| p.key == key) {
                    return Err(Parsed::Invalid);
                }
                pairs.push(Pair {
                    key,
                    value,
                    space_before: false,
                    raw: false,
                });
            } else {
                let value = self.value()?;
                items.push(Item {
                    value,
                    space_before: false,
                });
            }
            self.skip_spaces();
            match self.s.get(self.at) {
                Some(b',') => self.at += 1,
                Some(&c) if c == close => {}
                _ => return Err(Parsed::Unsupported),
            }
        }
        Ok(if open == b'[' {
            Value::Seq {
                flow: true,
                items,
                space_before: false,
            }
        } else {
            Value::Map {
                flow: true,
                pairs,
                space_before: false,
            }
        })
    }

    fn value(&mut self) -> Result<Value, Parsed> {
        if matches!(self.s.get(self.at), Some(b'[' | b'{')) {
            self.collection()
        } else {
            self.scalar().map(Value::Scalar)
        }
    }

    fn scalar(&mut self) -> Result<Scalar, Parsed> {
        if self.at >= self.s.len() {
            return Err(Parsed::Unsupported);
        }
        let rest = &self.text[self.at..];
        match self.s[self.at] {
            b'\'' | b'"' => {
                let (scalar, used) = quoted(rest)?;
                self.at += used;
                Ok(scalar)
            }
            b'[' | b'{' | b']' | b'}' | b',' => Err(Parsed::Unsupported),
            _ => {
                let end = rest.find([',', ']', '}', '[', '{']).unwrap_or(rest.len());
                let token = rest[..end].trim_end_matches(' ');
                if token.is_empty()
                    || !plain_start_ok(token)
                    || token.contains(": ")
                    || token.contains(" #")
                    || token.ends_with(':')
                {
                    return Err(Parsed::Unsupported);
                }
                self.at += token.len();
                Ok(resolve_plain(token))
            }
        }
    }
}

// --- document edits --------------------------------------------------------

impl Document {
    /// `doc.get(key)` when it is a string.
    pub fn get_str(&self, key: &str) -> Option<&str> {
        let pair = self.contents.as_ref()?.iter().find(|p| p.key == key)?;
        match &pair.value {
            Value::Scalar(Scalar::Str { value, .. }) => Some(value),
            _ => None,
        }
    }

    pub fn has(&self, key: &str) -> bool {
        self.contents
            .as_ref()
            .is_some_and(|pairs| pairs.iter().any(|p| p.key == key))
    }

    /// `doc.set(key, value)` for a string value: a scalar keeps its node
    /// (and quoting style), anything else is replaced; new keys append.
    pub fn set_str(&mut self, key: &str, value: &str) {
        let pairs = self.contents.get_or_insert_with(Vec::new);
        if let Some(pair) = pairs.iter_mut().find(|p| p.key == key) {
            match &mut pair.value {
                Value::Scalar(scalar) => {
                    let style = match scalar {
                        Scalar::Str { style, .. } => *style,
                        _ => Style::Plain,
                    };
                    *scalar = Scalar::Str {
                        value: value.to_string(),
                        style,
                    };
                }
                other => {
                    *other = Value::Scalar(Scalar::Str {
                        value: value.to_string(),
                        style: Style::Plain,
                    });
                    pair.raw = true;
                }
            }
            return;
        }
        pairs.push(Pair {
            key: key.to_string(),
            value: Value::Scalar(Scalar::Str {
                value: value.to_string(),
                style: Style::Plain,
            }),
            space_before: false,
            raw: true,
        });
    }

    /// `doc.delete(key)`.
    pub fn delete(&mut self, key: &str) {
        if let Some(pairs) = &mut self.contents {
            pairs.retain(|p| p.key != key);
        }
    }
}

// --- stringifying ------------------------------------------------------------

const LINE_WIDTH: usize = 80;
const MIN_CONTENT_WIDTH: usize = 20;
const INDENT_STEP: &str = "  ";

#[derive(Debug, Clone, Default)]
struct Ctx {
    indent: String,
    indent_at_start: Option<usize>,
    implicit_key: bool,
    in_flow: Option<bool>,
}

/// `String(doc)`.
pub fn stringify_document(doc: &Document) -> String {
    let ctx = Ctx::default();
    let body = match &doc.contents {
        Some(pairs) => stringify_collection(&Collection::Map(pairs), false, &ctx),
        None => "null".to_string(),
    };
    format!("{body}\n")
}

enum Collection<'a> {
    Map(&'a [Pair]),
    Seq(&'a [Item]),
}

/// `YAMLMap.toString` / `YAMLSeq.toString` → `stringifyCollection`.
fn stringify_collection(collection: &Collection, flow: bool, ctx: &Ctx) -> String {
    let flow = ctx.in_flow.unwrap_or(flow);
    let (start, end, prefix, base_indent) = match collection {
        Collection::Map(_) => ("{", "}", "", ctx.indent.clone()),
        Collection::Seq(_) => ("[", "]", "- ", format!("{}  ", ctx.indent)),
    };
    let item_ctx = if flow {
        Ctx {
            indent: format!("{base_indent}{INDENT_STEP}"),
            in_flow: Some(true),
            ..ctx.clone()
        }
    } else {
        Ctx {
            indent: base_indent,
            ..ctx.clone()
        }
    };
    let parts: Vec<(bool, String)> = match collection {
        Collection::Map(pairs) => pairs
            .iter()
            .map(|p| (p.space_before, stringify_pair(p, &item_ctx)))
            .collect(),
        Collection::Seq(items) => items
            .iter()
            .map(|i| (i.space_before, stringify_value(&i.value, &item_ctx)))
            .collect(),
    };
    let indent = &ctx.indent;
    let mut lines: Vec<String> = Vec::new();
    if flow {
        // `stringifyFlowCollection`
        let count = parts.len();
        let mut req_newline = false;
        let mut lines_at_value = 0;
        for (i, (space_before, mut s)) in parts.into_iter().enumerate() {
            if space_before {
                lines.push(String::new());
            }
            req_newline = req_newline || lines.len() > lines_at_value || s.contains('\n');
            if i + 1 < count {
                s.push(',');
            }
            lines.push(s);
            lines_at_value = lines.len();
        }
        if lines.is_empty() {
            return format!("{start}{end}");
        }
        if !req_newline {
            let len: usize = lines.iter().map(|l| js::len16(l) + 2).sum::<usize>() + 2;
            req_newline = len > LINE_WIDTH;
        }
        if !req_newline {
            return format!("{start} {} {end}", lines.join(" "));
        }
        let mut s = start.to_string();
        for line in &lines {
            if line.is_empty() {
                s.push('\n');
            } else {
                s.push_str(&format!("\n{INDENT_STEP}{indent}{line}"));
            }
        }
        return format!("{s}\n{indent}{end}");
    }
    for (space_before, s) in parts {
        if space_before {
            lines.push(String::new());
        }
        lines.push(format!("{prefix}{s}"));
    }
    if lines.is_empty() {
        return format!("{start}{end}");
    }
    let mut s = lines[0].clone();
    for line in &lines[1..] {
        if line.is_empty() {
            s.push('\n');
        } else {
            s.push_str(&format!("\n{indent}{line}"));
        }
    }
    s
}

/// `stringifyPair` (implicit keys, no comments).
fn stringify_pair(pair: &Pair, item_ctx: &Ctx) -> String {
    let mut ctx = Ctx {
        indent: format!("{}{INDENT_STEP}", item_ctx.indent),
        implicit_key: true,
        ..item_ctx.clone()
    };
    let key = stringify_string(&pair.key, Style::Plain, &ctx);
    let mut s = format!("{key}:");
    ctx.implicit_key = false;
    if matches!(pair.value, Value::Scalar(_)) && !pair.raw {
        ctx.indent_at_start = Some(js::len16(&s) + 1);
    }
    let value = stringify_value(&pair.value, &ctx);
    let mut ws = " ".to_string();
    match &pair.value {
        Value::Seq { space_before: true, .. } | Value::Map { space_before: true, .. } => {
            ws = "\n".into();
            if !(value.is_empty() && ctx.in_flow != Some(true)) {
                ws.push_str(&format!("\n{}", ctx.indent));
            }
        }
        Value::Seq { flow, .. } | Value::Map { flow, .. } => {
            if value.contains('\n') || !ctx.in_flow.unwrap_or(*flow) {
                ws = format!("\n{}", ctx.indent);
            }
        }
        Value::Scalar(_) => {
            if value.is_empty() || value.starts_with('\n') {
                ws = String::new();
            }
        }
    }
    s.push_str(&ws);
    s.push_str(&value);
    s
}

fn stringify_value(value: &Value, ctx: &Ctx) -> String {
    match value {
        Value::Scalar(scalar) => stringify_scalar(scalar, ctx),
        Value::Seq { flow, items, .. } => stringify_collection(&Collection::Seq(items), *flow, ctx),
        Value::Map { flow, pairs, .. } => stringify_collection(&Collection::Map(pairs), *flow, ctx),
    }
}

fn stringify_scalar(scalar: &Scalar, ctx: &Ctx) -> String {
    match scalar {
        Scalar::Str { value, style } => stringify_string(value, *style, ctx),
        Scalar::Null { source } => source.clone(),
        Scalar::Bool { source, .. } => source.clone(),
        Scalar::Num {
            value,
            format,
            min_fraction_digits,
        } => match format {
            NumFormat::Hex => format!("0x{:x}", *value as u64),
            NumFormat::Oct => format!("0o{:o}", *value as u64),
            NumFormat::Exp if value.is_finite() => js_to_exponential(*value),
            _ => stringify_number(*value, *min_fraction_digits),
        },
    }
}

/// `stringifyNumber`.
fn stringify_number(value: f64, min_fraction_digits: Option<usize>) -> String {
    if !value.is_finite() {
        return if value.is_nan() {
            ".nan".into()
        } else if value < 0.0 {
            "-.inf".into()
        } else {
            ".inf".into()
        };
    }
    let mut n = if value == 0.0 && value.is_sign_negative() {
        "-0".to_string()
    } else {
        js_number_to_string(value)
    };
    if let Some(min) = min_fraction_digits
        && min > 0
        && (n.starts_with(|c: char| c.is_ascii_digit())
            || n.starts_with("-") && n[1..].starts_with(|c: char| c.is_ascii_digit()))
        && !n.contains('e')
    {
        let i = match n.find('.') {
            Some(i) => i,
            None => {
                n.push('.');
                n.len() - 1
            }
        };
        let have = n.len() - i - 1;
        for _ in have..min {
            n.push('0');
        }
    }
    n
}

/// Shortest round-trip digits and decimal exponent `n` (value =
/// 0.d1d2... × 10^n), as ECMAScript's Number::toString defines them.
fn shortest_digits(value: f64) -> (String, i32) {
    let formatted = format!("{:e}", value.abs());
    let (mantissa, exponent) = formatted.split_once('e').unwrap();
    let digits: String = mantissa.chars().filter(|c| *c != '.').collect();
    (digits, exponent.parse::<i32>().unwrap() + 1)
}

/// `Number.prototype.toString()` / `JSON.stringify(number)`.
pub fn js_number_to_string(value: f64) -> String {
    if value == 0.0 {
        return "0".into();
    }
    let sign = if value < 0.0 { "-" } else { "" };
    let (digits, n) = shortest_digits(value);
    let k = digits.len() as i32;
    let body = if k <= n && n <= 21 {
        format!("{digits}{}", "0".repeat((n - k) as usize))
    } else if 0 < n && n <= 21 {
        format!("{}.{}", &digits[..n as usize], &digits[n as usize..])
    } else if -6 < n && n <= 0 {
        format!("0.{}{digits}", "0".repeat((-n) as usize))
    } else {
        let e = n - 1;
        let exp = if e >= 0 { format!("+{e}") } else { e.to_string() };
        if k == 1 {
            format!("{digits}e{exp}")
        } else {
            format!("{}.{}e{exp}", &digits[..1], &digits[1..])
        }
    };
    format!("{sign}{body}")
}

/// `Number.prototype.toExponential()`.
fn js_to_exponential(value: f64) -> String {
    let sign = if value < 0.0 { "-" } else { "" };
    if value == 0.0 {
        return "0e+0".into();
    }
    let (digits, n) = shortest_digits(value);
    let e = n - 1;
    let exp = if e >= 0 { format!("+{e}") } else { e.to_string() };
    if digits.len() == 1 {
        format!("{sign}{digits}e{exp}")
    } else {
        format!("{sign}{}.{}e{exp}", &digits[..1], &digits[1..])
    }
}

/// `/^(%|---|\.\.\.)/m`
fn contains_document_marker(s: &str) -> bool {
    s.split('\n')
        .any(|line| line.starts_with('%') || line.starts_with("---") || line.starts_with("..."))
}

/// `stringifyString` for a string value (with `actualString`).
fn stringify_string(value: &str, style: Style, ctx: &Ctx) -> String {
    let force_double = value.chars().any(|c| {
        let u = c as u32;
        u <= 0x08 || (0x0b..=0x1f).contains(&u) || (0x7f..=0x9f).contains(&u)
    });
    let style = if force_double { Style::Double } else { style };
    match style {
        Style::Double => double_quoted_string(value, ctx),
        Style::Single => single_quoted_string(value, ctx),
        Style::Plain => plain_string(value, ctx),
        Style::Literal | Style::Folded => {
            if ctx.implicit_key || ctx.in_flow == Some(true) {
                quoted_string(value, ctx)
            } else {
                block_string(value, style, ctx)
            }
        }
    }
}

/// `blockString` for a scalar that was a block scalar in the source.
fn block_string(value: &str, style: Style, ctx: &Ctx) -> String {
    // `/\n[\t ]+$/`
    let tail_ws = value.trim_end_matches([' ', '\t']);
    if tail_ws.len() < value.len() && tail_ws.ends_with('\n') {
        return quoted_string(value, ctx);
    }
    let indent = if !ctx.indent.is_empty() {
        ctx.indent.clone()
    } else if contains_document_marker(value) {
        "  ".to_string()
    } else {
        String::new()
    };
    let literal = style == Style::Literal;
    if value.is_empty() {
        return if literal { "|\n".into() } else { ">\n".into() };
    }
    let end_start = value.trim_end_matches(['\n', '\t', ' ']).len();
    let mut end = value[end_start..].to_string();
    let chomp = match end.find('\n') {
        None => "-",
        Some(pos) if end_start == 0 || pos != end.len() - 1 => "+",
        Some(_) => "",
    };
    let mut value = value.to_string();
    if !end.is_empty() {
        value.truncate(value.len() - end.len());
        if end.ends_with('\n') {
            end.pop();
        }
        end = add_indent_after_inner_newlines(&end, &indent);
    }
    // Leading spaces/newlines: an indentation indicator, and the leading
    // newlines kept verbatim.
    let mut start_with_space = false;
    let mut start_nl: isize = -1;
    let mut start_end = 0;
    for (i, c) in value.char_indices() {
        start_end = i;
        match c {
            ' ' => start_with_space = true,
            '\n' => start_nl = i as isize,
            _ => break,
        }
        start_end = i + 1;
    }
    let cut = if start_nl < start_end as isize {
        (start_nl + 1) as usize
    } else {
        start_end
    };
    let mut start = value[..cut].to_string();
    if !start.is_empty() {
        value = value[start.len()..].to_string();
        start = indent_newline_runs(&start, &indent);
    }
    let indent_size = if indent.is_empty() { "1" } else { "2" };
    let header = format!("{}{chomp}", if start_with_space { indent_size } else { "" });
    if !literal {
        let doubled = double_newline_runs(&value);
        let unfolded = keep_more_indented(&doubled);
        let folded_value = indent_newline_runs(&unfolded, &indent);
        let body = fold_flow_lines(
            &format!("{start}{folded_value}{end}"),
            &indent,
            FoldMode::Block,
            Some(js::len16(&ctx.indent)),
        );
        return format!(">{header}\n{indent}{body}");
    }
    let value = indent_newline_runs(&value, &indent);
    format!("|{header}\n{indent}{start}{value}{end}")
}

/// `.replace(/\n+/g, `$&${indent}`)`
fn indent_newline_runs(text: &str, indent: &str) -> String {
    let mut out = String::new();
    let mut chars = text.chars().peekable();
    while let Some(c) = chars.next() {
        out.push(c);
        if c == '\n' && chars.peek() != Some(&'\n') {
            out.push_str(indent);
        }
    }
    out
}

/// `.replace(/\n+/g, '\n$&')`
fn double_newline_runs(text: &str) -> String {
    let mut out = String::new();
    let mut previous_newline = false;
    for c in text.chars() {
        if c == '\n' && !previous_newline {
            out.push('\n');
        }
        out.push(c);
        previous_newline = c == '\n';
    }
    out
}

/// `.replace(/(?:^|\n)([\t ].*)(?:([\n\t ]*)\n(?![\n\t ]))?/g, '$1$2')`:
/// the source's folded scalars never hold more-indented lines (the parser
/// refuses them), so only the no-match case is reachable.
fn keep_more_indented(text: &str) -> String {
    text.to_string()
}

/// `end.replace(/(^|(?<!\n))\n+(?!\n|$)/g, `$&${indent}`)`: a newline run
/// not at the end of the string gets the indent after it.
fn add_indent_after_inner_newlines(text: &str, indent: &str) -> String {
    let mut out = String::new();
    let chars: Vec<char> = text.chars().collect();
    let mut i = 0;
    while i < chars.len() {
        if chars[i] == '\n' {
            let mut j = i;
            while j < chars.len() && chars[j] == '\n' {
                j += 1;
            }
            out.extend(&chars[i..j]);
            if j < chars.len() {
                out.push_str(indent);
            }
            i = j;
        } else {
            out.push(chars[i]);
            i += 1;
        }
    }
    out
}

fn quoted_string(value: &str, ctx: &Ctx) -> String {
    let has_double = value.contains('"');
    let has_single = value.contains('\'');
    if has_double && !has_single {
        single_quoted_string(value, ctx)
    } else {
        double_quoted_string(value, ctx)
    }
}

fn plain_string(value: &str, ctx: &Ctx) -> String {
    let in_flow = ctx.in_flow == Some(true);
    if (ctx.implicit_key && value.contains('\n')) || (in_flow && value.contains(['[', ']', '{', '}', ','])) {
        return quoted_string(value, ctx);
    }
    if plain_not_allowed(value) {
        // (blockString for multi-line values is outside the subset.)
        return quoted_string(value, ctx);
    }
    if contains_document_marker(value) && ctx.implicit_key && ctx.indent == INDENT_STEP {
        return quoted_string(value, ctx);
    }
    // `actualString`: a plain spelling that reads back as another type.
    if !matches!(resolve_plain_for_test(value), Scalar::Str { .. }) {
        return quoted_string(value, ctx);
    }
    if ctx.implicit_key {
        value.to_string()
    } else {
        fold_flow_lines(value, &ctx.indent, FoldMode::Flow, ctx.indent_at_start)
    }
}

/// Which tag's `test` a plain spelling matches (every string tag test in
/// the core schema, including the big-number ones `resolve_plain` models as
/// strings).
fn resolve_plain_for_test(value: &str) -> Scalar {
    let is = |s: &str, radix: u32| !s.is_empty() && s.chars().all(|c| c.is_digit(radix));
    if value.strip_prefix("0o").is_some_and(|d| is(d, 8)) || value.strip_prefix("0x").is_some_and(|d| is(d, 16)) {
        return Scalar::Null { source: String::new() };
    }
    resolve_plain(value)
}

/// `/^[\n\t ,[\]{}#&*!|>'"%@`]|^[?-]$|^[?-][ \t]|[\n:][ \t]|[ \t]\n|[\n\t ]#|[\n\t :]$/`
fn plain_not_allowed(value: &str) -> bool {
    let b = value.as_bytes();
    if b.is_empty() {
        return false;
    }
    if b"\n\t ,[]{}#&*!|>'\"%@`".contains(&b[0]) {
        return true;
    }
    if matches!(b[0], b'?' | b'-') && (b.len() == 1 || matches!(b[1], b' ' | b'\t')) {
        return true;
    }
    for i in 0..b.len() {
        let next = b.get(i + 1).copied();
        if matches!(b[i], b'\n' | b':') && matches!(next, Some(b' ' | b'\t')) {
            return true;
        }
        if matches!(b[i], b' ' | b'\t') && next == Some(b'\n') {
            return true;
        }
        if matches!(b[i], b'\n' | b'\t' | b' ') && next == Some(b'#') {
            return true;
        }
    }
    matches!(b[b.len() - 1], b'\n' | b'\t' | b' ' | b':')
}

fn single_quoted_string(value: &str, ctx: &Ctx) -> String {
    let ws_around_newline = value
        .as_bytes()
        .windows(2)
        .any(|w| (matches!(w[0], b' ' | b'\t') && w[1] == b'\n') || (w[0] == b'\n' && matches!(w[1], b' ' | b'\t')));
    if (ctx.implicit_key && value.contains('\n')) || ws_around_newline {
        return double_quoted_string(value, ctx);
    }
    let indent = if !ctx.indent.is_empty() {
        ctx.indent.clone()
    } else if contains_document_marker(value) {
        "  ".into()
    } else {
        String::new()
    };
    let escaped = value.replace('\'', "''");
    let mut res = String::from("'");
    let mut chars = escaped.chars().peekable();
    while let Some(c) = chars.next() {
        res.push(c);
        if c == '\n' {
            while chars.peek() == Some(&'\n') {
                res.push(chars.next().unwrap());
            }
            res.push('\n');
            res.push_str(&indent);
        }
    }
    res.push('\'');
    if ctx.implicit_key {
        res
    } else {
        fold_flow_lines(&res, &indent, FoldMode::Flow, ctx.indent_at_start)
    }
}

/// `JSON.stringify(string)`.
fn json_string(value: &str) -> String {
    let mut out = String::from("\"");
    for c in value.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\u{8}' => out.push_str("\\b"),
            '\u{c}' => out.push_str("\\f"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            c if (c as u32) < 0x20 => out.push_str(&format!("\\u{:04x}", c as u32)),
            c => out.push(c),
        }
    }
    out.push('"');
    out
}

fn double_quoted_string(value: &str, ctx: &Ctx) -> String {
    let json: Vec<char> = json_string(value).chars().collect();
    let implicit_key = ctx.implicit_key;
    let min_multi_line_length = 40;
    let indent = if !ctx.indent.is_empty() {
        ctx.indent.clone()
    } else if contains_document_marker(value) {
        "  ".into()
    } else {
        String::new()
    };
    let slice = |a: usize, b: usize| -> String { json[a.min(json.len())..b.min(json.len())].iter().collect() };
    let mut s = String::new();
    let mut start = 0usize;
    let mut i = 0usize;
    while i < json.len() {
        let mut ch = json[i];
        if ch == ' ' && json.get(i + 1) == Some(&'\\') && json.get(i + 2) == Some(&'n') {
            s.push_str(&slice(start, i));
            s.push_str("\\ ");
            i += 1;
            start = i;
            ch = '\\';
        }
        if ch == '\\' {
            match json.get(i + 1) {
                Some('u') => {
                    s.push_str(&slice(start, i));
                    let code = slice(i + 2, i + 6);
                    match code.as_str() {
                        "0000" => s.push_str("\\0"),
                        "0007" => s.push_str("\\a"),
                        "000b" => s.push_str("\\v"),
                        "001b" => s.push_str("\\e"),
                        "0085" => s.push_str("\\N"),
                        "00a0" => s.push_str("\\_"),
                        "2028" => s.push_str("\\L"),
                        "2029" => s.push_str("\\P"),
                        _ => {
                            if let Some(low) = code.strip_prefix("00") {
                                s.push_str("\\x");
                                s.push_str(low);
                            } else {
                                s.push_str(&slice(i, i + 6));
                            }
                        }
                    }
                    i += 5;
                    start = i + 1;
                }
                Some('n') => {
                    if implicit_key || json.get(i + 2) == Some(&'"') || json.len() < min_multi_line_length {
                        i += 1;
                    } else {
                        s.push_str(&slice(start, i));
                        s.push_str("\n\n");
                        while json.get(i + 2) == Some(&'\\')
                            && json.get(i + 3) == Some(&'n')
                            && json.get(i + 4) != Some(&'"')
                        {
                            s.push('\n');
                            i += 2;
                        }
                        s.push_str(&indent);
                        if json.get(i + 2) == Some(&' ') {
                            s.push('\\');
                        }
                        i += 1;
                        start = i + 1;
                    }
                }
                _ => i += 1,
            }
        }
        i += 1;
    }
    let s = if start > 0 {
        format!("{s}{}", slice(start, json.len()))
    } else {
        json.iter().collect()
    };
    if implicit_key {
        s
    } else {
        fold_flow_lines(&s, &indent, FoldMode::Quoted, ctx.indent_at_start)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum FoldMode {
    Flow,
    Quoted,
    Block,
}

/// `consumeMoreIndentedLines`.
fn consume_more_indented_lines(text: &[u16], mut i: isize, indent: isize) -> isize {
    let at = |k: isize| -> Option<u16> { if k >= 0 { text.get(k as usize).copied() } else { None } };
    let mut end = i;
    let mut start = i + 1;
    let mut ch = at(start);
    while ch == Some(b' ' as u16) || ch == Some(b'\t' as u16) {
        if i < start + indent {
            i += 1;
            ch = at(i);
        } else {
            loop {
                i += 1;
                ch = at(i);
                if ch.is_none() || ch == Some(b'\n' as u16) {
                    break;
                }
            }
            end = i;
            start = i + 1;
            ch = at(start);
        }
    }
    end
}

/// `foldFlowLines` (flow and quoted modes, default widths). Indexes are
/// UTF-16 units, like the JS.
fn fold_flow_lines(text: &str, indent: &str, mode: FoldMode, indent_at_start: Option<usize>) -> String {
    let t: Vec<u16> = text.encode_utf16().collect();
    let indent_len = js::len16(indent) as isize;
    let line_width = LINE_WIDTH as isize;
    let min_content_width = MIN_CONTENT_WIDTH as isize;
    let end_step = (1 + min_content_width).max(1 + line_width - indent_len);
    if (t.len() as isize) <= end_step {
        return text.to_string();
    }
    let at = |i: isize| -> Option<u16> { if i >= 0 { t.get(i as usize).copied() } else { None } };
    let mut folds: Vec<isize> = Vec::new();
    let mut escaped_folds: Vec<isize> = Vec::new();
    let mut end = line_width - indent_len;
    if let Some(start) = indent_at_start {
        let start = start as isize;
        if start > line_width - 2.max(min_content_width) {
            folds.push(0);
        } else {
            end = line_width - start;
        }
    }
    let mut split: Option<isize> = None;
    let mut prev: Option<u16> = None;
    let mut i: isize = -1;
    let mut esc_start: isize = -1;
    let mut esc_end: isize = -1;
    let (sp, nl, tab, bs) = (b' ' as u16, b'\n' as u16, b'\t' as u16, b'\\' as u16);
    if mode == FoldMode::Block {
        i = consume_more_indented_lines(&t, i, indent_len);
        if i != -1 {
            end = i + end_step;
        }
    }
    loop {
        i += 1;
        let Some(mut ch) = at(i) else { break };
        if mode == FoldMode::Quoted && ch == bs {
            esc_start = i;
            match at(i + 1).map(|c| c as u8 as char) {
                Some('x') => i += 3,
                Some('u') => i += 5,
                Some('U') => i += 9,
                _ => i += 1,
            }
            esc_end = i;
        }
        if ch == nl {
            if mode == FoldMode::Block {
                i = consume_more_indented_lines(&t, i, indent_len);
            }
            end = i + indent_len + end_step;
            split = None;
        } else {
            if ch == sp && prev.is_some_and(|p| p != sp && p != nl && p != tab) {
                let next = at(i + 1);
                if next.is_some_and(|n| n != sp && n != nl && n != tab) {
                    split = Some(i);
                }
            }
            if i >= end {
                if let Some(s) = split.filter(|&s| s != 0) {
                    folds.push(s);
                    end = s + end_step;
                    split = None;
                } else if mode == FoldMode::Quoted {
                    while prev == Some(sp) || prev == Some(tab) {
                        prev = Some(ch);
                        i += 1;
                        match at(i) {
                            Some(c) => ch = c,
                            None => {
                                ch = 0;
                            }
                        }
                    }
                    let j = if i > esc_end + 1 { i - 2 } else { esc_start - 1 };
                    if escaped_folds.contains(&j) {
                        return text.to_string();
                    }
                    folds.push(j);
                    escaped_folds.push(j);
                    end = j + end_step;
                    split = None;
                }
            }
        }
        prev = Some(ch);
        if ch == 0 && at(i).is_none() {
            break;
        }
    }
    if folds.is_empty() {
        return text.to_string();
    }
    let slice = |a: isize, b: isize| -> String {
        let len = t.len() as isize;
        let a = a.clamp(0, len) as usize;
        let b = b.clamp(0, len) as usize;
        String::from_utf16_lossy(&t[a.min(b)..b])
    };
    let mut res = slice(0, folds[0]);
    for (k, &fold) in folds.iter().enumerate() {
        let end = match folds.get(k + 1) {
            Some(&f) if f != 0 => f,
            _ => t.len() as isize,
        };
        if fold == 0 {
            res = format!("\n{indent}{}", slice(0, end));
        } else {
            if mode == FoldMode::Quoted && escaped_folds.contains(&fold) {
                res.push_str(&slice(fold, fold + 1));
                res.push('\\');
            }
            res.push_str(&format!("\n{indent}{}", slice(fold + 1, end)));
        }
    }
    res
}
