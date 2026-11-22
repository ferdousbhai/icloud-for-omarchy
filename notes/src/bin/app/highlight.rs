//! The editor's Markdown, styled in the theme's colours: formatting only,
//! the text stays the Markdown as written. Headings are bold and larger,
//! quotes dim and italic, list markers and checkboxes in the accent (a done
//! item struck through), and inline bold, italic, strikethrough, code and
//! links as they read. Their marks (`#`, `**`, a link's target) show, muted,
//! only on the line the cursor is on, and are hidden elsewhere.

use std::cell::Cell;

use gtk::{pango, prelude::*};

/// The longest line (in characters) whose inline emphasis, code and links
/// are styled: longer ones are not prose (a pasted blob), and keep their
/// line shape only.
const INLINE_LIMIT: usize = 8 * 1024;

pub struct Highlighter {
    buffer: gtk::TextBuffer,
    /// The line whose marks show; None while the editor has no focus.
    active: Cell<Option<i32>>,
}

/// Every tag this sets, so a line is cleared of them before it is styled again.
const TAGS: [&str; 15] = [
    "h1", "h2", "h3", "mark", "hidden", "muted", "quote", "accent", "done", "bold", "italic", "strike", "code", "link",
    "rule",
];

impl Highlighter {
    pub fn new(buffer: &gtk::TextBuffer) -> Highlighter {
        let table = buffer.tag_table();
        let add = |name: &str| {
            let tag = gtk::TextTag::new(Some(name));
            table.add(&tag);
            tag
        };
        for (name, scale) in [("h1", 1.6), ("h2", 1.25), ("h3", 1.08)] {
            add(name).set_properties(&[("weight", &700), ("scale", &scale)]);
        }
        add("mark");
        add("hidden").set_invisible(true);
        add("muted");
        add("rule");
        add("quote").set_style(pango::Style::Italic);
        add("accent");
        add("done").set_strikethrough(true);
        add("bold").set_weight(700);
        add("italic").set_style(pango::Style::Italic);
        add("strike").set_strikethrough(true);
        add("code").set_family(Some("monospace"));
        add("link").set_underline(pango::Underline::Single);
        let this = Highlighter {
            buffer: buffer.clone(),
            active: Cell::new(None),
        };
        if let Some(theme) = omui::Theme::get() {
            this.set_colours(&theme.tokens());
        }
        this
    }

    /// The theme's colours on the tags.
    pub fn set_colours(&self, t: &omui::Tokens) {
        let table = self.buffer.tag_table();
        let muted = omui::blend(&t.dim_text, &t.background, 0.25);
        let code = omui::blend(&t.text, &t.background, 0.9);
        for (name, colour) in [
            ("mark", muted.as_str()),
            ("muted", &muted),
            ("rule", &muted),
            ("done", &muted),
            ("quote", &t.dim_text),
            ("accent", &t.accent),
            ("link", &t.accent),
        ] {
            if let Some(tag) = table.lookup(name) {
                tag.set_foreground(Some(colour));
            }
        }
        if let Some(tag) = table.lookup("code") {
            tag.set_background(Some(&code));
        }
    }

    /// Moves the line whose marks show (None: no line's), restyling the two.
    pub fn set_active_line(&self, line: Option<i32>) {
        let was = self.active.replace(line);
        if was != line {
            for line in [was, line].into_iter().flatten() {
                self.line(line);
            }
        }
    }

    /// Styles every line again.
    pub fn all(&self) {
        for line in 0..self.buffer.line_count() {
            self.line(line);
        }
    }

    fn line(&self, line: i32) {
        let Some(start) = self.buffer.iter_at_line(line) else {
            return;
        };
        let mut end = start;
        if !end.ends_line() {
            end.forward_to_line_end();
        }
        for tag in TAGS {
            self.buffer.remove_tag_by_name(tag, &start, &end);
        }
        let text: Vec<char> = self.buffer.text(&start, &end, true).chars().collect();
        let offset = start.offset();
        let tag = |name: &str, from: usize, to: usize| {
            if to > from {
                let a = self.buffer.iter_at_offset(offset + from as i32);
                let b = self.buffer.iter_at_offset(offset + to as i32);
                self.buffer.apply_tag_by_name(name, &a, &b);
            }
        };
        let active = self.active.get() == Some(line);
        let mark = |from: usize, to: usize| tag(if active { "mark" } else { "hidden" }, from, to);
        for span in spans(&text) {
            match span {
                Span::Mark(a, b) => mark(a, b),
                Span::Style(name, a, b) => tag(name, a, b),
            }
        }
    }
}

/// What styling a line asks for, in the order it is applied.
#[derive(Debug, PartialEq, Eq)]
enum Span {
    /// A Markdown mark, shown muted on the active line and hidden elsewhere.
    Mark(usize, usize),
    Style(&'static str, usize, usize),
}

/// The spans of one line (character offsets).
fn spans(text: &[char]) -> Vec<Span> {
    let mut out = Vec::new();
    let n = text.len();
    let indent = text.iter().take_while(|c| c.is_whitespace()).count();
    // Line shapes first, so inline emphasis inside them still shows.
    let hashes = text.iter().take_while(|c| **c == '#').count();
    if (1..=6).contains(&hashes) && text.get(hashes).is_some_and(|c| c.is_whitespace()) {
        let level = match hashes {
            1 => "h1",
            2 => "h2",
            _ => "h3",
        };
        out.push(Span::Style(level, 0, n));
        let marks = hashes + text[hashes..].iter().take_while(|c| c.is_whitespace()).count();
        out.push(Span::Mark(0, marks));
    } else if is_rule(text) {
        out.push(Span::Style("rule", 0, n));
    } else if text.get(indent) == Some(&'>') {
        out.push(Span::Style("quote", 0, n));
        out.push(Span::Style("accent", 0, indent + 1));
    } else if let Some(marker_end) = list_marker(text, indent) {
        let after = marker_end + text[marker_end..].iter().take_while(|c| c.is_whitespace()).count();
        let checkbox = after > marker_end
            && text.get(after) == Some(&'[')
            && text.get(after + 1).is_some_and(|c| matches!(c, ' ' | 'x' | 'X'))
            && text.get(after + 2) == Some(&']')
            && text.get(after + 3).is_some_and(|c| c.is_whitespace());
        if checkbox {
            if text[after + 1] != ' ' {
                out.push(Span::Style("done", after + 3, n));
            }
            out.push(Span::Style("accent", 0, after + 3));
        } else {
            out.push(Span::Style("accent", indent, marker_end));
        }
    }
    if n > INLINE_LIMIT {
        return out;
    }
    for (open, inner, close) in paired(text, &['*', '*']).into_iter().chain(paired(text, &['_', '_'])) {
        out.push(Span::Style("bold", inner.0, inner.1));
        out.push(Span::Mark(open.0, open.1));
        out.push(Span::Mark(close.0, close.1));
    }
    for (open, inner, close) in italics(text) {
        out.push(Span::Style("italic", inner.0, inner.1));
        out.push(Span::Mark(open.0, open.1));
        out.push(Span::Mark(close.0, close.1));
    }
    for (open, inner, close) in paired(text, &['~', '~']) {
        out.push(Span::Style("strike", inner.0, inner.1));
        out.push(Span::Mark(open.0, open.1));
        out.push(Span::Mark(close.0, close.1));
    }
    for (open, _, close) in code(text) {
        out.push(Span::Style("code", open.0, close.1));
        out.push(Span::Mark(open.0, open.1));
        out.push(Span::Mark(close.0, close.1));
    }
    for (open, inner, close) in links(text) {
        out.push(Span::Mark(open.0, open.1));
        out.push(Span::Style("link", inner.0, inner.1));
        out.push(Span::Mark(close.0, close.1));
    }
    out
}

/// One line as Pango markup, for a page (export-pdf): styled as in the
/// editor off the cursor's line, its marks left out. Its colours are fixed
/// inks, not the theme's: a page is printed on white paper, whatever the
/// desktop looks like.
pub fn markup(line: &str) -> String {
    let text: Vec<char> = line.chars().collect();
    let n = text.len();
    let mut styles: Vec<Vec<&str>> = vec![Vec::new(); n];
    let mut hidden = vec![false; n];
    for span in spans(&text) {
        match span {
            Span::Mark(a, b) => hidden[a.min(n)..b.min(n)].iter_mut().for_each(|h| *h = true),
            Span::Style(name, a, b) => {
                for style in &mut styles[a.min(n)..b.min(n)] {
                    style.push(name);
                }
            }
        }
    }
    let mut out = String::new();
    let mut i = 0;
    while i < n {
        if hidden[i] {
            i += 1;
            continue;
        }
        let mut j = i + 1;
        while j < n && !hidden[j] && styles[j] == styles[i] {
            j += 1;
        }
        let run: String = text[i..j].iter().collect();
        let run = gtk::glib::markup_escape_text(&run);
        let attributes: String = styles[i]
            .iter()
            .map(|style| match *style {
                "h1" => r#" size="160%" weight="bold""#,
                "h2" => r#" size="125%" weight="bold""#,
                "h3" => r#" size="108%" weight="bold""#,
                "bold" => r#" weight="bold""#,
                "italic" => r#" style="italic""#,
                "quote" => r##" style="italic" foreground="#555555""##,
                "strike" => r#" strikethrough="true""#,
                "done" => r##" strikethrough="true" foreground="#777777""##,
                "rule" | "muted" => r##" foreground="#777777""##,
                "code" => r##" font_family="monospace" background="#eeeeee""##,
                "link" => r##" underline="single" foreground="#1a5fb4""##,
                _ => "",
            })
            .collect();
        if attributes.is_empty() {
            out.push_str(&run);
        } else {
            out.push_str(&format!("<span{attributes}>{run}</span>"));
        }
        i = j;
    }
    out
}

type Range = (usize, usize);

/// `---`, `***` or `___`, spaces allowed between.
fn is_rule(text: &[char]) -> bool {
    let marks: Vec<char> = text.iter().copied().filter(|c| !c.is_whitespace()).collect();
    marks.len() >= 3
        && matches!(marks[0], '-' | '*' | '_')
        && text.iter().all(|c| c.is_whitespace() || matches!(c, '-' | '*' | '_'))
        && marks[..3].iter().all(|c| *c == marks[0])
}

/// Where a list marker (`-`, `*`, `+`, `1.`, `1)`) ends, when the line starts
/// with one followed by a space.
fn list_marker(text: &[char], indent: usize) -> Option<usize> {
    let first = *text.get(indent)?;
    let end = if matches!(first, '-' | '*' | '+') {
        indent + 1
    } else {
        let digits = text[indent..].iter().take_while(|c| c.is_ascii_digit()).count();
        if digits == 0 || !text.get(indent + digits).is_some_and(|c| matches!(c, '.' | ')')) {
            return None;
        }
        indent + digits + 1
    };
    text.get(end).filter(|c| c.is_whitespace()).map(|_| end)
}

/// `<d>text<d>` for a two-character delimiter `d`, its text not starting or
/// ending with a space: the opening mark, the text, the closing mark.
fn paired(text: &[char], d: &[char; 2]) -> Vec<(Range, Range, Range)> {
    let mut out = Vec::new();
    let at = |i: usize| text.get(i..i + 2) == Some(&d[..]);
    let mut i = 0;
    while i + 2 < text.len() {
        if at(i) && !text[i + 2].is_whitespace() {
            let found = (i + 3..text.len().saturating_sub(1)).find(|&k| at(k) && !text[k - 1].is_whitespace());
            if let Some(k) = found {
                out.push(((i, i + 2), (i + 2, k), (k, k + 2)));
                i = k + 2;
                continue;
            }
        }
        i += 1;
    }
    out
}

fn is_word(c: char) -> bool {
    c.is_alphanumeric() || c == '_'
}

/// `*text*` or `_text_`: not part of a word or of `**`, its text with no
/// `*` or `_` in it and no space at either end.
fn italics(text: &[char]) -> Vec<(Range, Range, Range)> {
    let mut out = Vec::new();
    let mut i = 0;
    while i + 2 < text.len() {
        let c = text[i];
        let starts = matches!(c, '*' | '_')
            && (i == 0 || !(text[i - 1] == '*' || is_word(text[i - 1])))
            && !text[i + 1].is_whitespace()
            && !matches!(text[i + 1], '*' | '_');
        if starts {
            let mut k = i + 2;
            let mut found = None;
            while k < text.len() && !matches!(text[k - 1], '*' | '_') {
                let after = text.get(k + 1).copied();
                if text[k] == c && !text[k - 1].is_whitespace() && !after.is_some_and(|a| a == '*' || is_word(a)) {
                    found = Some(k);
                    break;
                }
                k += 1;
            }
            if let Some(k) = found {
                out.push(((i, i + 1), (i + 1, k), (k, k + 1)));
                i = k + 1;
                continue;
            }
        }
        i += 1;
    }
    out
}

/// `` `code` ``.
fn code(text: &[char]) -> Vec<(Range, Range, Range)> {
    let mut out = Vec::new();
    let mut i = 0;
    while i < text.len() {
        if text[i] == '`'
            && let Some(len) = text[i + 1..].iter().position(|c| *c == '`')
            && len > 0
        {
            let k = i + 1 + len;
            out.push(((i, i + 1), (i + 1, k), (k, k + 1)));
            i = k + 1;
            continue;
        }
        i += 1;
    }
    out
}

/// `[text](target)` and `![text](target)`: the marks before the text, the
/// text, and `](target)`.
fn links(text: &[char]) -> Vec<(Range, Range, Range)> {
    let mut out = Vec::new();
    let mut i = 0;
    while i < text.len() {
        let open = if text[i] == '!' && text.get(i + 1) == Some(&'[') {
            2
        } else {
            usize::from(text[i] == '[')
        };
        if open > 0
            && let Some(len) = text[i + open..].iter().position(|c| *c == ']')
        {
            let close = i + open + len;
            if text.get(close + 1) == Some(&'(')
                && let Some(target) = text[close + 2..].iter().position(|c| *c == ')')
            {
                let end = close + 2 + target + 1;
                out.push(((i, i + open), (i + open, close), (close, end)));
                i = end;
                continue;
            }
        }
        i += 1;
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn of(line: &str) -> Vec<Span> {
        spans(&line.chars().collect::<Vec<_>>())
    }

    #[test]
    fn a_heading_is_styled_and_its_hashes_are_marks() {
        assert_eq!(of("## Weekend"), [Span::Style("h2", 0, 10), Span::Mark(0, 3)]);
        assert_eq!(of("#hashtag"), []);
    }

    #[test]
    fn a_done_checkbox_strikes_its_text() {
        assert_eq!(
            of("- [x] coffee"),
            [Span::Style("done", 5, 12), Span::Style("accent", 0, 5)]
        );
        assert_eq!(of("- [ ] milk"), [Span::Style("accent", 0, 5)]);
        assert_eq!(of("12. step"), [Span::Style("accent", 0, 3)]);
    }

    #[test]
    fn inline_emphasis_needs_text_against_its_marks() {
        assert_eq!(
            of("a **b** c"),
            [Span::Style("bold", 4, 5), Span::Mark(2, 4), Span::Mark(5, 7)]
        );
        assert_eq!(of("a ** b ** c"), []);
        assert_eq!(
            of("*maybe* it"),
            [Span::Style("italic", 1, 6), Span::Mark(0, 1), Span::Mark(6, 7)]
        );
        assert_eq!(of("snake_case_name"), []);
    }

    #[test]
    fn a_link_shows_its_text_and_hides_its_target() {
        assert_eq!(
            of("[the bakery](https://x)"),
            [Span::Mark(0, 1), Span::Style("link", 1, 11), Span::Mark(11, 23)]
        );
    }

    #[test]
    fn a_page_shows_the_styles_without_the_marks() {
        assert_eq!(
            markup("## Weekend"),
            r#"<span size="125%" weight="bold">Weekend</span>"#
        );
        assert_eq!(markup("a **b** <c>"), r#"a <span weight="bold">b</span> &lt;c&gt;"#);
    }

    #[test]
    fn rules_and_quotes_are_whole_lines() {
        assert_eq!(of("- - -"), [Span::Style("rule", 0, 5)]);
        assert_eq!(of("> bags"), [Span::Style("quote", 0, 6), Span::Style("accent", 0, 1)]);
    }
}
