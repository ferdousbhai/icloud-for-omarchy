//! The shared shape of the human changelists (`status`, `push`, `pull`).
//! Originally derived from icloud-md, without the colours: plain text, as
//! when stdout isn't a terminal.

/// `LISTING_INDENT`.
pub const LISTING_INDENT: &str = "        ";

/// `labelledLine`: the label padded to `width`, then the subject.
pub fn labelled_line(label: &str, width: usize, subject: &str) -> String {
    format!("{label:<width$} {subject}")
}

/// `remarkLine`: indented to sit under the subject.
pub fn remark_line(width: usize, message: &str) -> String {
    format!("{}{message}", " ".repeat(width + 1))
}
