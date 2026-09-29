//! A scripted in-memory transport for the ported databaseClient tests (the
//! TypeScript tests stub `globalThis.fetch` the same way).
#![allow(dead_code)]

use std::cell::RefCell;
use std::path::Path;

use icloud_notes_sync::cloudkit::{CkError, Database, Transport};
use serde_json::Value;

type Handler = Box<dyn FnMut(&str, &Value) -> Result<Value, CkError>>;

pub struct MockTransport {
    handler: RefCell<Handler>,
    /// Every POST as (path with query, body).
    pub requests: RefCell<Vec<(String, Value)>>,
}

impl MockTransport {
    pub fn new(handler: impl FnMut(&str, &Value) -> Result<Value, CkError> + 'static) -> Self {
        MockTransport {
            handler: RefCell::new(Box::new(handler)),
            requests: RefCell::new(Vec::new()),
        }
    }

    /// Bodies of requests whose path contains `needle`.
    pub fn bodies(&self, needle: &str) -> Vec<Value> {
        self.requests
            .borrow()
            .iter()
            .filter(|(p, _)| p.contains(needle))
            .map(|(_, b)| b.clone())
            .collect()
    }
}

impl Transport for MockTransport {
    fn post_json(&self, path: &str, body: &Value) -> Result<Value, CkError> {
        self.requests.borrow_mut().push((path.to_owned(), body.clone()));
        (self.handler.borrow_mut())(path, body)
    }

    fn download(&self, url: &str, _dest: &Path) -> Result<u64, CkError> {
        panic!("unexpected download in test: {url}")
    }
}

pub fn db(handler: impl FnMut(&str, &Value) -> Result<Value, CkError> + 'static) -> Database<MockTransport> {
    Database::new(MockTransport::new(handler))
}

/// Answers `pages` in order, one per request; panics on an extra request.
pub fn scripted(pages: Vec<Value>) -> impl FnMut(&str, &Value) -> Result<Value, CkError> {
    let mut pages = pages.into_iter();
    move |path, _| {
        Ok(pages
            .next()
            .unwrap_or_else(|| panic!("unexpected extra request to {path}")))
    }
}

pub fn no_pages() -> impl FnMut(usize) {
    |_| {}
}
