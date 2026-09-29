//! Base copies: `.icloud-md/base/<recordName>.md`, the body-only
//! last-synced text. Ports icloud-md `src/notes/baseCopy.ts`. Owner:
//! workstream D.
#![allow(unused_variables)]

use std::path::Path;

use crate::cmd::errors::Error;

pub fn read_base_copy(target_dir: &Path, record_name: &str) -> Result<Option<String>, Error> {
    todo!()
}

pub fn write_base_copy(target_dir: &Path, record_name: &str, content: &str) -> Result<(), Error> {
    todo!()
}

pub fn remove_base_copy(target_dir: &Path, record_name: &str) -> Result<(), Error> {
    todo!()
}
