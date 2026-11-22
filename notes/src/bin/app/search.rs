//! The search boxes: text matches at once, on every key, then local Recall
//! (when it is installed) ranks the same notes by meaning once typing
//! pauses. A plain query gains Recall's other hits below its text matches,
//! as related; `~ query` shows Recall's ranking alone. Recall keeps its
//! index of the vault under a collection named for the vault, the one the
//! Qt window used.

use std::cell::{Cell, RefCell};
use std::collections::HashMap;
use std::io::Write;
use std::path::PathBuf;
use std::process::{Command, Stdio};
use std::rc::Rc;
use std::time::{Duration, Instant};

use gtk::glib;
use icloud_notes::backend::ReadNote;
use omui::tr;
use serde_json::{Value, json};

use crate::lists::Hit;
use crate::window::Ui;

/// How long typing pauses before Recall ranks.
const PAUSE: Duration = Duration::from_millis(400);
/// How long Recall may take.
const LIMIT: Duration = Duration::from_secs(6);

/// Which search Recall's answer would be for, and its pending start.
#[derive(Default)]
pub struct Meaning {
    generation: Cell<u64>,
    delay: RefCell<Option<glib::SourceId>>,
}

fn hit(read: &icloud_notes::backend::SearchHit, related: bool) -> Hit {
    Hit {
        folder: read.folder.clone(),
        file: read.file.clone(),
        title: read.title.clone(),
        snippet: read.snippet.clone(),
        related,
    }
}

impl Ui {
    /// Searches again (the query, or the vault, changed): at least two characters.
    pub fn refresh_search(&self) {
        let generation = self.meaning.generation.get() + 1;
        self.meaning.generation.set(generation);
        if let Some(source) = self.meaning.delay.take() {
            source.remove();
        }
        let query = self.query.borrow().trim().to_string();
        if query.chars().count() < 2 {
            self.hits.replace(None);
            self.search_truncated.set(false);
            return;
        }
        let recall = glib::find_program_in_path("recall");
        let found = self.backend.borrow_mut().search_window(&query, recall.is_some());
        self.search_truncated.set(found.truncated);
        self.hits
            .replace(Some(found.hits.iter().map(|found| hit(found, false)).collect()));
        let alone = query.starts_with('~');
        let Some(ui) = self.weak().upgrade() else { return };
        let Some(recall) = recall else {
            if alone {
                ui.set_notice(|| tr("Recall unavailable or warming up · showing text matches"));
            }
            return;
        };
        let asked = query.trim_start_matches('~').trim().to_string();
        let collection = collection(self.backend.borrow().vault().root());
        let weak = Rc::downgrade(&ui);
        let source = glib::timeout_add_local_once(if alone { Duration::ZERO } else { PAUSE }, move || {
            let Some(ui) = weak.upgrade() else { return };
            ui.meaning.delay.take();
            let read = found.read;
            let records: Vec<Value> = read
                .iter()
                .map(|note| {
                    json!({
                        "id": note.path,
                        "title": note.hit.title.chars().take(1000).collect::<String>(),
                        "text": note.text,
                    })
                })
                .collect();
            let weak = Rc::downgrade(&ui);
            omui::background(
                move || rank(&recall, &collection, &asked, &records),
                move |ranked| {
                    if let Some(ui) = weak.upgrade()
                        && ui.meaning.generation.get() == generation
                    {
                        ui.ranked(alone, &read, ranked.ok().flatten());
                    }
                },
            );
        });
        self.meaning.delay.replace(Some(source));
    }

    /// Recall's ranking is in (`None`: it could not rank).
    fn ranked(self: &Rc<Self>, alone: bool, read: &[ReadNote], ranked: Option<Vec<String>>) {
        let by_path: HashMap<&str, &ReadNote> = read.iter().map(|note| (note.path.as_str(), note)).collect();
        // An id Recall made up, or kept from an older vault, is no ranking of these notes.
        let ranked = ranked.and_then(|ids| {
            ids.iter()
                .map(|id| by_path.get(id.as_str()).map(|note| &note.hit))
                .collect::<Option<Vec<_>>>()
        });
        if alone {
            if let Some(ranked) = &ranked {
                self.hits
                    .replace(Some(ranked.iter().map(|found| hit(found, false)).collect()));
            }
            if ranked.is_some() {
                self.set_notice(|| tr("Matches by meaning · local Recall"));
            } else {
                self.set_notice(|| tr("Recall unavailable or warming up · showing text matches"));
            }
        } else if let Some(ranked) = ranked {
            let mut hits = self.hits.borrow_mut();
            let Some(hits) = hits.as_mut() else { return };
            let shown: Vec<(String, String)> = hits.iter().map(|h| (h.folder.clone(), h.file.clone())).collect();
            hits.extend(
                ranked
                    .iter()
                    .filter(|found| !shown.contains(&(found.folder.clone(), found.file.clone())))
                    .map(|found| hit(found, true)),
            );
        }
        self.show_vault();
    }
}

/// Recall's collection for the vault at `root`: the Qt window's name for it.
fn collection(root: &str) -> String {
    let digest = glib::compute_checksum_for_data(glib::ChecksumType::Sha256, root.as_bytes()).unwrap_or_default();
    format!("icloud-notes:{}", &digest[..24.min(digest.len())])
}

/// `recall rank <collection> <query>`, given the notes as records on stdin:
/// the ids it ranks, best first; `None` when it is not ready to rank by
/// meaning, fails, or takes too long.
fn rank(recall: &PathBuf, collection: &str, query: &str, records: &[Value]) -> Option<Vec<String>> {
    let mut child = Command::new(recall)
        .args(["rank", collection, query])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .ok()?;
    let input = serde_json::to_vec(records).unwrap_or_default();
    let mut stdin = child.stdin.take()?;
    let mut stdout = child.stdout.take()?;
    // Both pipes at once: a long answer would otherwise fill its pipe while
    // the notes are still being written to the other.
    let writer = std::thread::spawn(move || stdin.write_all(&input));
    let reader = std::thread::spawn(move || {
        let mut out = Vec::new();
        std::io::Read::read_to_end(&mut stdout, &mut out).map(|_| out)
    });
    let begun = Instant::now();
    let finished = loop {
        match child.try_wait() {
            Ok(Some(status)) => break Some(status),
            Ok(None) if begun.elapsed() < LIMIT => std::thread::sleep(Duration::from_millis(20)),
            _ => break None,
        }
    };
    if finished.is_none() {
        let _ = child.kill();
        let _ = child.wait();
    }
    let _ = writer.join();
    let out = reader.join().ok()?.ok()?;
    let status = finished.filter(|status| status.success())?;
    let reply: Value = serde_json::from_slice(&out).ok()?;
    let ready = status.success() && reply["api"] == 1 && reply["meaning"] == true && reply.get("error").is_none();
    ready.then(|| {
        reply["hits"]
            .as_array()
            .map(|hits| {
                hits.iter()
                    .map(|hit| hit["id"].as_str().unwrap_or_default().to_string())
                    .collect()
            })
            .unwrap_or_default()
    })
}

#[cfg(test)]
mod tests {
    use std::os::unix::fs::PermissionsExt;

    use super::*;

    /// A `recall` that reads the records and answers `answer`, exiting `code`.
    fn recall(dir: &std::path::Path, answer: &str, code: i32) -> PathBuf {
        let path = dir.join("recall");
        std::fs::write(
            &path,
            format!("#!/bin/sh\ncat > \"$0.in\"\necho '{answer}'\nexit {code}\n"),
        )
        .unwrap();
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
        path
    }

    #[test]
    fn recall_ranks_the_records_it_is_given() {
        let dir = tempfile::tempdir().unwrap();
        let records = [json!({"id": "a.md", "title": "A", "text": "# A\n"})];
        let ready = recall(
            dir.path(),
            r#"{"api":1,"meaning":true,"hits":[{"id":"b.md"},{"id":"a.md"}]}"#,
            0,
        );
        assert_eq!(
            rank(&ready, "icloud-notes:x", "q", &records),
            Some(vec!["b.md".to_string(), "a.md".to_string()])
        );
        let given: Value = serde_json::from_slice(&std::fs::read(dir.path().join("recall.in")).unwrap()).unwrap();
        assert_eq!(given, json!(records));
        // Not ready to rank by meaning, an error, or a failure: no ranking.
        for (answer, code) in [
            (r#"{"api":1,"meaning":false,"hits":[]}"#, 0),
            (r#"{"api":1,"meaning":true,"error":"warming up","hits":[]}"#, 0),
            (r#"{"api":1,"meaning":true,"hits":[]}"#, 1),
            ("not json", 0),
        ] {
            assert_eq!(
                rank(&recall(dir.path(), answer, code), "c", "q", &records),
                None,
                "{answer}"
            );
        }
    }

    #[test]
    fn the_collection_is_named_for_the_vault_as_the_qt_window_named_it() {
        assert_eq!(
            collection("/home/me/Documents/Notes"),
            "icloud-notes:fbae688d4c81d09019a92a40"
        );
    }
}
