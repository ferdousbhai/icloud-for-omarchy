//! The one-pane sidebar of a narrow window: the folder list and the note
//! list as one tree, flattened into the rows a list shows. Pure: nothing
//! here touches disk. The C++ `NoteTree` (src/notetree.h), held to the same
//! answers by tests/fixtures/syncmodel.json.

use std::collections::{HashMap, HashSet};

use crate::text::split_path;

/// A note of the vault, as the tree lists it.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct TreeNote {
    /// Vault-relative.
    pub path: String,
    /// The directory it is in ("" for the vault root).
    pub folder: String,
    pub file: String,
    pub title: String,
    pub modified_ms: i64,
}

/// What the tree needs of a note; the rest of it rides along in its row.
pub trait Listed {
    /// Vault-relative.
    fn path(&self) -> &str;
    /// The directory it is in ("" for the vault root).
    fn folder(&self) -> &str;
}

impl Listed for TreeNote {
    fn path(&self) -> &str {
        &self.path
    }

    fn folder(&self) -> &str {
        &self.folder
    }
}

/// One row: a folder (`note` is `None`; `folder` "" is All Notes) or a note
/// (`folder` is the one it is in, `count` 0, never expandable).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Row<N> {
    pub folder: String,
    pub depth: usize,
    pub count: i64,
    pub expandable: bool,
    pub expanded: bool,
    pub note: Option<N>,
}

/// The folder a folder (vault-relative, "" for the vault root) sits in.
pub fn parent_of(folder: &str) -> &str {
    split_path(folder).0
}

/// `folders`: the backend's folder list ("" first, every parent before its
/// children). `counts`: how many notes each holds. `notes`: every note of
/// the vault, newest first. `expanded`: the folders shown open ("" is All
/// Notes).
///
/// The rows: All Notes, open on every note; then each top-level folder,
/// open on its subfolders, then its own notes. While `searching`, only the
/// notes in `matches` (vault-relative paths) show, and only the folders that
/// hold one somewhere below them, all of them open; All Notes shows when
/// anything matched.
pub fn rows<N: Listed + Clone>(
    folders: &[String],
    counts: &HashMap<String, i64>,
    notes: &[N],
    expanded: &[String],
    searching: bool,
    matches: &[String],
) -> Vec<Row<N>> {
    let open: HashSet<&str> = expanded.iter().map(String::as_str).collect();
    let hits: HashSet<&str> = matches.iter().map(String::as_str).collect();
    let mut shown: Vec<&N> = Vec::new();
    let mut notes_in: HashMap<&str, Vec<&N>> = HashMap::new();
    let mut holding: HashSet<&str> = HashSet::new(); // folders with a shown note somewhere below them
    for note in notes {
        if searching && !hits.contains(note.path()) {
            continue;
        }
        shown.push(note);
        notes_in.entry(note.folder()).or_default().push(note);
        let mut folder = note.folder();
        while !folder.is_empty() {
            holding.insert(folder);
            folder = parent_of(folder);
        }
    }
    let mut children_of: HashMap<&str, Vec<&str>> = HashMap::new();
    for folder in folders.iter().filter(|f| !f.is_empty()) {
        children_of.entry(parent_of(folder)).or_default().push(folder);
    }

    let tree = Tree {
        counts,
        open,
        searching,
        holding,
        notes_in,
        children_of,
    };
    let mut out = Vec::new();
    if (!searching || !shown.is_empty()) && tree.add_folder(&mut out, "", 0, !shown.is_empty()) {
        add_notes(&mut out, &shown, 1);
    }
    for top in tree.children_of.get("").into_iter().flatten() {
        tree.add_subtree(&mut out, top, 0);
    }
    out
}

fn add_notes<N: Listed + Clone>(out: &mut Vec<Row<N>>, notes: &[&N], depth: usize) {
    out.extend(notes.iter().map(|note| Row {
        folder: note.folder().to_string(),
        depth,
        count: 0,
        expandable: false,
        expanded: false,
        note: Some((*note).clone()),
    }));
}

struct Tree<'a, N> {
    counts: &'a HashMap<String, i64>,
    open: HashSet<&'a str>,
    searching: bool,
    holding: HashSet<&'a str>,
    notes_in: HashMap<&'a str, Vec<&'a N>>,
    children_of: HashMap<&'a str, Vec<&'a str>>,
}

impl<N: Listed + Clone> Tree<'_, N> {
    /// Adds the folder's own row; whether it is shown open.
    fn add_folder(&self, out: &mut Vec<Row<N>>, folder: &str, depth: usize, expandable: bool) -> bool {
        let expanded = expandable && (self.searching || self.open.contains(folder));
        out.push(Row {
            folder: folder.to_string(),
            depth,
            count: self.counts.get(folder).copied().unwrap_or(0),
            expandable,
            expanded,
            note: None,
        });
        expanded
    }

    fn add_subtree(&self, out: &mut Vec<Row<N>>, folder: &str, depth: usize) {
        if self.searching && !self.holding.contains(folder) {
            return;
        }
        let none = Vec::new();
        let children = self.children_of.get(folder).unwrap_or(&none);
        let own = self.notes_in.get(folder).map_or(&[][..], Vec::as_slice);
        if !self.add_folder(out, folder, depth, !children.is_empty() || !own.is_empty()) {
            return;
        }
        for child in children {
            self.add_subtree(out, child, depth + 1);
        }
        add_notes(out, own, depth + 1);
    }
}
