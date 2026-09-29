//! A scripted in-memory transport for the ported databaseClient tests (the
//! TypeScript tests stub `globalThis.fetch` the same way).
#![allow(dead_code)]

use std::cell::RefCell;
use std::path::Path;

use icloud_notes_sync::cloudkit::{CkError, Database, Transport};
use serde_json::Value;

type Handler = Box<dyn FnMut(&str, &Value) -> Result<Value, CkError>>;
type AssetHandler = Box<dyn FnMut(&str) -> Result<Vec<u8>, CkError>>;

pub struct MockTransport {
    handler: RefCell<Handler>,
    /// Answers asset GETs (`download_bytes`); `None` panics on one.
    assets: RefCell<Option<AssetHandler>>,
    /// Every asset GET's URL.
    pub downloads: RefCell<Vec<String>>,
    /// Every POST as (path with query, body).
    pub requests: RefCell<Vec<(String, Value)>>,
}

impl MockTransport {
    pub fn new(handler: impl FnMut(&str, &Value) -> Result<Value, CkError> + 'static) -> Self {
        MockTransport {
            handler: RefCell::new(Box::new(handler)),
            assets: RefCell::new(None),
            downloads: RefCell::new(Vec::new()),
            requests: RefCell::new(Vec::new()),
        }
    }

    /// Serves asset downloads from `assets`.
    pub fn with_assets(self, assets: impl FnMut(&str) -> Result<Vec<u8>, CkError> + 'static) -> Self {
        *self.assets.borrow_mut() = Some(Box::new(assets));
        self
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

    fn download_bytes(&self, url: &str) -> Result<Vec<u8>, CkError> {
        self.downloads.borrow_mut().push(url.to_owned());
        match self.assets.borrow_mut().as_mut() {
            Some(assets) => assets(url),
            None => panic!("unexpected download in test: {url}"),
        }
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
