//! Note file names. Ports icloud-md `src/notes/filename.ts`. Owner:
//! workstream C.
//!
//! Plan deviation: the plan's `note_filename(title, taken)` is two calls in
//! icloud-md - `noteFileNameFor(titleLine, titleMode)` then
//! `uniqueFileName(name, usedNames)` (per directory) - kept separate here.
#![allow(unused_variables)]

use std::collections::HashSet;

use crate::vault::state::TitleMode;

/// `noteFileNameFor`.
pub fn note_file_name_for(title_line: &str, title_mode: TitleMode) -> String {
    todo!()
}

/// `titleNeedingFrontmatter`.
pub fn title_needing_frontmatter(title_line: &str, title_mode: TitleMode) -> Option<String> {
    todo!()
}

/// `fileNameCarriesTitle`.
pub fn file_name_carries_title(file_name: &str, title_line: &str) -> bool {
    todo!()
}

/// `noteFileName`: the in-body vault's slug (≤ 80 UTF-16 units).
pub fn note_file_name(title: &str) -> String {
    todo!()
}

/// `uniqueFileName`: `Foo.md`, `Foo 2.md`, `Foo 3.md`, ...
pub fn unique_file_name(file_name: &str, used_file_names: &HashSet<String>) -> String {
    todo!()
}
