//! The vault as the app works on it: its folders and notes with the badges
//! the window shows, the open note, and every change to them, with the
//! guardrails that keep a change from costing a note in iCloud. The window
//! and the command line both go through here; nothing in it draws.
//!
//! This is the non-UI half of the C++ `NotesBackend` (src/notesbackend.cpp),
//! which the Qt app still runs. Engine runs and the sign-in are in
//! `sync.rs`.

use std::collections::HashMap;
use std::fs;
use std::io;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::time::{Duration, Instant};

use rustix::fs::{CWD, RenameFlags, renameat_with};
use serde_json::{Map, Value};

use crate::attachments::{self, Attachment, attachments_for};
use crate::engine::{self, VaultInfo};
use crate::files::{for_each_note_file, modified_ms, note_folders, os_error_text, read_text, write_text};
use crate::lock::{Taken, VaultLock};
use crate::notetree::TreeNote;
use crate::syncmodel::{self, split_envelope};
use crate::text::{eq_ignoring_case, folded, folded_contains, join_path, left_utf16, split_path, utf16_len};
use crate::trash::move_to_trash;
use crate::vault::Vault;

const CHANGED_ON_DISK: &str = "The note changed on disk. Look it over, then try again.";

/// A note larger than this is not a note anymore; the guardrail scans stop
/// here so a stray huge file cannot stall the list.
const SCAN_LIMIT: usize = 2 * 1024 * 1024;
/// How much of a note is read for its title alone, and to find its id.
const HEAD_LIMIT: usize = 64 * 1024;
/// How much of a note a search reads.
const SEARCH_LIMIT: usize = 1024 * 1024;
/// The most notes a search answers with.
const SEARCH_HITS: usize = 100;
/// The most notes the window's search reads: it searches on every key.
pub const WINDOW_SEARCH_NOTES: usize = 2000;
/// The most the sync engine uploads as one attachment (notes-sync
/// MAX_NEW_ATTACHMENT_BYTES).
const MAX_ATTACHMENT_BYTES: u64 = 100 * 1024 * 1024;
/// How long an engine that failed to answer vault-info is left alone before
/// it is asked again (the answer is asked for on every re-read of the vault).
const VAULT_INFO_RETRY: Duration = Duration::from_secs(2);

/// Who holds the vault's lock, as it describes itself to whoever waits.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Role {
    /// The window, which holds the lock for as long as it is open.
    App,
    /// A command of the command line.
    Cli,
    /// `icloud-notes --sync`, the timer's run.
    Background,
}

/// Why the backend would not do something: the message, and the class a
/// caller can branch on (`error`, `read_only`, `usage`, `exists`, ...).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Refusal {
    pub code: &'static str,
    pub message: String,
    pub hint: String,
}

impl Refusal {
    pub(crate) fn new(code: &'static str, message: impl Into<String>) -> Refusal {
        Refusal {
            code,
            message: message.into(),
            hint: String::new(),
        }
    }

    fn error(message: impl Into<String>) -> Refusal {
        Refusal::new("error", message)
    }

    fn hint(mut self, hint: &str) -> Refusal {
        self.hint = hint.into();
        self
    }
}

/// What a caller with a window wants to hear of, in the order it happened.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Event {
    /// Files changed here (a save, a move, a clean merge): worth a sync.
    VaultChanged,
    /// The open note's file holds something else than what was loaded, so a
    /// save was not written; the edits go through
    /// [`Backend::keep_edits_as_conflict`].
    NoteChangedOnDisk,
    /// Unsaved edits could not go into their note and are in a new one; the
    /// message says why and where.
    EditsKeptAsNote(String),
    /// A save that waited for the sync was written, with this body.
    QueuedSaveWritten(String),
}

/// A note as the list shows it.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct NoteDetails {
    pub title: String,
    pub snippet: String,
    pub modified_ms: i64,
    /// Its apple-note-id, "" without one.
    pub id: String,
}

/// What was read of a note's file, kept while the file stays as it was.
#[derive(Debug, Clone, Default, PartialEq)]
struct NoteScan {
    modified_ms: i64,
    size: u64,
    mode: String,
    title: String,
    snippet: String,
    id: String,
    conflict: bool,
    table: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ConflictLine {
    pub text: String,
    /// The other side has no such line.
    pub changed: bool,
}

/// One conflict block of the open note, for the version picker.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Conflict {
    pub local: Vec<ConflictLine>,
    pub remote: Vec<ConflictLine>,
    /// Up to two lines of the untouched text before the block, and after it.
    pub before: Vec<String>,
    pub after: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SearchHit {
    pub folder: String,
    pub file: String,
    pub title: String,
    pub snippet: String,
}

/// The window's search ([`Backend::search_window`]).
#[derive(Debug, Default)]
pub struct WindowSearch {
    pub hits: Vec<SearchHit>,
    /// There were more notes than it reads.
    pub truncated: bool,
    /// The notes it read, when asked for them: what Recall ranks by meaning.
    pub read: Vec<ReadNote>,
}

/// A note the window's search read: its vault-relative path, the start of
/// its text, and its row as a search shows it.
#[derive(Debug, Clone)]
pub struct ReadNote {
    pub path: String,
    pub text: String,
    pub hit: SearchHit,
}

/// A file copied into a note's attachments/ directory.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Added {
    /// The name it has there.
    pub name: String,
    pub path: PathBuf,
    /// What the note's text must hold for the engine to upload it.
    pub link: String,
}

/// A note replaced by `recover`: what to tell, and where the copy is.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Recovered {
    pub message: String,
    pub backup: PathBuf,
}

/// The open note as it was when its file went away.
#[derive(Debug, Clone, Default)]
pub(crate) struct LostNote {
    folder: String,
    title: String,
    content: String,
}

/// A save that waits for the running sync: the note (vault-relative, ""
/// for one that went away), the text its edits started from, and the edits.
#[derive(Debug, Clone)]
pub(crate) struct QueuedSave {
    note: String,
    base: String,
    body: String,
}

pub struct Backend {
    pub(crate) role: Role,
    pub(crate) vault: Vault,
    pub(crate) lock: VaultLock,
    /// The sync engine, when there is one to run.
    pub(crate) engine: Option<PathBuf>,

    vault_info: Rc<VaultInfo>,
    vault_info_key: String,
    vault_info_failed: Option<Instant>,

    folders: Vec<String>,
    folder_note_counts: HashMap<String, i64>,
    current_folder: String,
    current_note: String,
    notes: Vec<String>,
    note_states: HashMap<String, Vec<&'static str>>,
    note_details: HashMap<String, NoteDetails>,
    scans: HashMap<PathBuf, NoteScan>,
    title_scans: HashMap<PathBuf, NoteScan>,

    pub(crate) note_content: String,
    note_attachments: Vec<Attachment>,
    read_only_reason: String,

    lost_note: Option<LostNote>,
    pub(crate) queued_save: Option<QueuedSave>,
    merging: bool,
    following: bool,
    events: Vec<Event>,

    pub(crate) sync: crate::sync::State,
}

fn io_refusal(what: &str, e: &io::Error) -> Refusal {
    Refusal::error(format!("{what}: {}", os_error_text(e)))
}

/// Renames a file, never over another one.
fn rename_file(from: &Path, to: &Path) -> bool {
    match renameat_with(CWD, from, CWD, to, RenameFlags::NOREPLACE) {
        Ok(()) => true,
        // a filesystem without RENAME_NOREPLACE
        Err(rustix::io::Errno::INVAL) => !to.exists() && fs::rename(from, to).is_ok(),
        Err(_) => false,
    }
}

/// Copies a file with its permissions, never over another one: a file at
/// `to` is not this copy's to replace or to remove (something else writing
/// the folder may put one there at any moment). A copy that fails after it
/// made the file leaves none.
fn copy_file(from: &Path, to: &Path) -> io::Result<()> {
    let mut source = fs::File::open(from)?;
    let mut target = fs::File::options().write(true).create_new(true).open(to)?;
    let copied =
        io::copy(&mut source, &mut target).and_then(|_| target.set_permissions(source.metadata()?.permissions()));
    if copied.is_err() {
        let _ = fs::remove_file(to);
    }
    copied
}

impl Backend {
    /// The backend of `vault`, with its folders and notes read, and the
    /// engine this computer has. Takes no lock: a caller that changes the
    /// vault or syncs it does ([`Backend::lock_vault`]).
    pub fn new(vault: Vault, role: Role) -> Backend {
        Backend::with_engine(vault, role, engine::sync_tool_path())
    }

    /// [`Backend::new`] with `engine` as the sync engine (`None`: none is installed).
    pub fn with_engine(vault: Vault, role: Role, engine: Option<PathBuf>) -> Backend {
        let lock = VaultLock::new(vault.lock_path());
        let mut backend = Backend {
            role,
            vault,
            lock,
            engine,
            vault_info: Rc::default(),
            vault_info_key: String::new(),
            vault_info_failed: None,
            folders: Vec::new(),
            folder_note_counts: HashMap::new(),
            current_folder: String::new(),
            current_note: String::new(),
            notes: Vec::new(),
            note_states: HashMap::new(),
            note_details: HashMap::new(),
            scans: HashMap::new(),
            title_scans: HashMap::new(),
            note_content: String::new(),
            note_attachments: Vec::new(),
            read_only_reason: String::new(),
            lost_note: None,
            queued_save: None,
            merging: false,
            following: false,
            events: Vec::new(),
            sync: crate::sync::State::default(),
        };
        backend.refresh();
        backend
    }

    pub fn vault(&self) -> &Vault {
        &self.vault
    }

    /// The sync engine's binary, when one is installed.
    pub fn engine(&self) -> Option<&Path> {
        self.engine.as_deref()
    }

    /// What happened since the last call.
    pub fn take_events(&mut self) -> Vec<Event> {
        std::mem::take(&mut self.events)
    }

    // ---- the lock ----------------------------------------------------------------

    /// Takes the vault's lock for this process, without waiting.
    pub fn lock_vault(&mut self) -> Taken {
        let owner = match self.role {
            Role::App => "Notes (pid",
            Role::Cli => "the icloud-notes command line (pid",
            Role::Background => "a background sync (icloud-notes --sync, pid",
        };
        self.lock.try_lock(&format!("{owner} {})", std::process::id()))
    }

    pub fn unlock_vault(&mut self) {
        self.lock.release();
    }

    /// Who holds the lock while this process cannot have it.
    pub fn lock_holder(&self) -> String {
        let holder = self.lock.holder();
        if holder.is_empty() {
            "another sync".into()
        } else {
            holder
        }
    }

    // ---- what the engine knows -----------------------------------------------------

    fn vault_info_key(&self) -> String {
        format!(
            "{}\n{}\n{}",
            self.engine.as_deref().unwrap_or(Path::new("")).display(),
            self.vault.absolute().display(),
            self.vault.state_stamp()
        )
    }

    /// Takes `answer` (vault-info's, or the one a sync carries) as what the
    /// engine knows of the vault as it is now.
    pub(crate) fn adopt_vault_info(&mut self, answer: &Map<String, Value>) {
        self.vault_info = Rc::new(VaultInfo::from_answer(answer));
        self.vault_info_key = self.vault_info_key();
        self.vault_info_failed = None;
    }

    /// What the engine knows of the vault, asked again only once the state
    /// file, the vault or the engine changed.
    fn vault_info(&mut self) -> Rc<VaultInfo> {
        // Taken before asking, so a change while the engine answers asks again.
        let key = self.vault_info_key();
        if !self.vault_info_key.is_empty() && key == self.vault_info_key {
            return self.vault_info.clone();
        }
        let Some(tool) = self.engine.clone() else {
            self.adopt_vault_info(&Map::new()); // no engine: nothing is known
            return self.vault_info.clone();
        };
        if self
            .vault_info_failed
            .is_some_and(|failed| failed.elapsed() < VAULT_INFO_RETRY)
        {
            return self.vault_info.clone();
        }
        match engine::vault_info(&tool, &self.vault.absolute()) {
            Some(info) => {
                self.vault_info = Rc::new(info);
                self.vault_info_key = key;
                self.vault_info_failed = None;
            }
            // No answer: what was known stands, and the engine is asked again later.
            None => self.vault_info_failed = Some(Instant::now()),
        }
        self.vault_info.clone()
    }

    /// "in-body" (a note's title is its first line) or "filename".
    pub fn vault_title_mode(&mut self) -> String {
        self.vault_info().title_mode.clone()
    }

    /// The directory of the account's default folder, "" when unknown.
    pub fn default_folder(&mut self) -> String {
        self.vault_info().default_folder_dir.clone()
    }

    // ---- folders and notes ---------------------------------------------------------

    /// Every folder, the vault root ("") first, in Apple Notes' order.
    pub fn folders(&self) -> &[String] {
        &self.folders
    }

    /// How many notes each folder holds, its subfolders' included.
    pub fn folder_note_counts(&self) -> &HashMap<String, i64> {
        &self.folder_note_counts
    }

    pub fn current_folder(&self) -> &str {
        &self.current_folder
    }

    /// The open note's file name, "" when none is open.
    pub fn current_note(&self) -> &str {
        &self.current_note
    }

    /// The notes of the current folder and its subfolders, newest first, as
    /// paths relative to it.
    pub fn notes(&self) -> &[String] {
        &self.notes
    }

    /// The badges of a note of [`Backend::notes`]: conflict, tables, then one
    /// of conflict-copy, new, foreign-id, missing-id, then read-only.
    pub fn note_flags(&self, name: &str) -> &[&'static str] {
        self.note_states.get(name).map_or(&[], Vec::as_slice)
    }

    pub fn note_details(&self, name: &str) -> NoteDetails {
        self.note_details.get(name).cloned().unwrap_or_default()
    }

    fn note_absolute_path(&self) -> Option<PathBuf> {
        (!self.current_note.is_empty()).then(|| self.vault.folder(&self.current_folder).join(&self.current_note))
    }

    fn vault_relative(&self, name: &str) -> String {
        join_path(&self.current_folder, name)
    }

    /// The open note's vault-relative path.
    pub fn current_path(&self) -> String {
        self.vault_relative(&self.current_note)
    }

    fn rebuild_folders(&mut self) {
        let mut folders = vec![String::new()]; // the vault root itself
        folders.extend(note_folders(self.vault.path()));
        syncmodel::sort_folders(&mut folders, &self.default_folder());

        // Every note counts toward each folder above it, the root included.
        let mut counts: HashMap<String, i64> = folders.iter().map(|f| (f.clone(), 0)).collect();
        for_each_note_file(self.vault.path(), &mut |relative, _| {
            let mut folder = relative;
            loop {
                folder = split_path(folder).0;
                if let Some(count) = counts.get_mut(folder) {
                    *count += 1;
                }
                if folder.is_empty() {
                    return true;
                }
            }
        });
        self.folders = folders;
        self.folder_note_counts = counts;
        if !self.folders.contains(&self.current_folder) {
            // The open note went with its folder: follow it, or remember it.
            let id = syncmodel::extract_note_id(&self.note_content);
            if self.current_note.is_empty() || !self.follow_note(&id) {
                if !self.current_note.is_empty() {
                    self.lose_current_note();
                }
                self.set_current_folder("");
            }
        }
    }

    /// Every note of the vault, newest first: what the sidebar tree is made of.
    pub fn all_notes(&mut self) -> Vec<TreeNote> {
        let mode = self.vault_title_mode();
        let mut notes = Vec::new();
        let mut scans = HashMap::new();
        let known = std::mem::take(&mut self.title_scans);
        for_each_note_file(self.vault.path(), &mut |relative, path| {
            let (modified, size) = fs::metadata(path).map_or((0, 0), |m| (modified_ms(&m), m.len()));
            let mut scan = known.get(path).cloned().unwrap_or_default();
            if scan.modified_ms != modified || scan.size != size || scan.mode != mode {
                let (folder_and_stem, _) = relative.rsplit_once('.').unwrap_or((relative, ""));
                let stem = split_path(folder_and_stem).1;
                scan = NoteScan {
                    modified_ms: modified,
                    size,
                    mode: mode.clone(),
                    title: syncmodel::preview_note(&read_text(path, Some(HEAD_LIMIT)), stem, &mode).title,
                    ..Default::default()
                };
            }
            let (folder, file) = split_path(relative);
            notes.push(TreeNote {
                path: relative.to_string(),
                folder: folder.to_string(),
                file: file.to_string(),
                title: scan.title.clone(),
                modified_ms: scan.modified_ms,
            });
            scans.insert(path.to_path_buf(), scan);
            true
        });
        self.title_scans = scans;
        // Newest first, as the note list (and Notes.app) orders them.
        notes.sort_by_key(|note| std::cmp::Reverse(note.modified_ms));
        notes
    }

    fn rebuild_notes(&mut self) {
        let dir = self.vault.folder(&self.current_folder);
        let mut entries: Vec<(String, i64)> = Vec::new();
        for_each_note_file(&dir, &mut |relative, path| {
            entries.push((relative.to_string(), fs::metadata(path).map_or(0, |m| modified_ms(&m))));
            true
        });
        // The sync engine syncs note mtimes, so newest-first matches Notes.app ordering.
        entries.sort_by_key(|(_, modified)| std::cmp::Reverse(*modified));
        let found: Vec<String> = entries.into_iter().map(|(relative, _)| relative).collect();
        // The open note's file is gone (a pull moved, renamed or deleted it):
        // follow it by its id, which rebuilds the list where it went.
        if !self.current_note.is_empty() && !found.contains(&self.current_note) {
            let id = syncmodel::extract_note_id(&self.note_content);
            if self.follow_note(&id) {
                return;
            }
        }
        self.notes = found;
        if !self.current_note.is_empty() && !self.notes.contains(&self.current_note) {
            self.lose_current_note();
        }
        self.classify_notes();
    }

    /// Per-note list details plus the guardrail flags the badges show: notes
    /// the engine does not track ("new"), tracked notes that lost their id,
    /// untracked notes carrying an id from elsewhere, Syncthing's conflict
    /// copies (which it never syncs), conflicts and tables.
    fn classify_notes(&mut self) {
        let info = self.vault_info();
        let mode = &info.title_mode;
        let dir = self.vault.folder(&self.current_folder);
        let mut scans = HashMap::new();
        let mut states = HashMap::new();
        let mut details = HashMap::new();
        for name in &self.notes {
            let path = dir.join(name);
            let (modified, size) = fs::metadata(&path).map_or((0, 0), |m| (modified_ms(&m), m.len()));
            let mut scan = self.scans.get(&path).cloned().unwrap_or_default();
            if scan.modified_ms != modified || scan.size != size || &scan.mode != mode {
                let text = read_text(&path, Some(SCAN_LIMIT + 1));
                let huge = utf16_len(&text) > SCAN_LIMIT;
                let file = split_path(name).1;
                let stem = file.rsplit_once('.').map_or(file, |(stem, _)| stem);
                let preview = syncmodel::preview_note(&text, stem, mode);
                scan = NoteScan {
                    modified_ms: modified,
                    size,
                    mode: mode.clone(),
                    title: preview.title,
                    snippet: preview.snippet,
                    id: syncmodel::extract_note_id(&text),
                    conflict: !huge && syncmodel::has_conflict_markers(&text),
                    table: !huge && syncmodel::has_table(&text),
                };
            }
            details.insert(
                name.clone(),
                NoteDetails {
                    title: scan.title.clone(),
                    snippet: scan.snippet.clone(),
                    modified_ms: scan.modified_ms,
                    id: scan.id.clone(),
                },
            );
            let relative = join_path(&self.current_folder, name);
            let mut flags = Vec::new();
            if scan.conflict {
                flags.push("conflict");
            }
            if scan.table {
                flags.push("tables");
            }
            // A Syncthing conflict copy is no note to the engine: never "new"
            // (it will not be created) nor a note with someone else's id.
            if syncmodel::is_sync_conflict_name(name) {
                flags.push("conflict-copy");
            } else if !info.tracked.contains(&relative) {
                flags.push(if scan.id.is_empty() { "new" } else { "foreign-id" });
            } else if scan.id.is_empty() {
                flags.push("missing-id");
            }
            if info.read_only.contains_key(&relative) {
                flags.push("read-only");
            }
            if !flags.is_empty() {
                states.insert(name.clone(), flags);
            }
            scans.insert(path, scan);
        }
        self.scans = scans;
        self.note_states = states;
        self.note_details = details;
    }

    fn load_current_note(&mut self) {
        let path = self.note_absolute_path();
        self.note_content = path.as_deref().map(|p| read_text(p, None)).unwrap_or_default();
        self.note_attachments = path
            .as_deref()
            .map(|p| attachments_for(p, &self.note_content))
            .unwrap_or_default();
        self.read_only_reason = match path {
            Some(_) => {
                let relative = self.current_path();
                self.vault_info().read_only.get(&relative).cloned().unwrap_or_default()
            }
            None => String::new(),
        };
    }

    fn close_note(&mut self) {
        self.current_note.clear();
        self.load_current_note();
    }

    /// Where the note with this apple-note-id lives now, vault-relative.
    /// Only when the open note's file went away, so a full scan is fine.
    fn find_note_by_id(&self, id: &str) -> Option<String> {
        if id.is_empty() {
            return None;
        }
        let mut found = None;
        for_each_note_file(self.vault.path(), &mut |relative, path| {
            // A Syncthing conflict copy carries the id too, and is not the note.
            if !syncmodel::is_sync_conflict_name(relative)
                && syncmodel::extract_note_id(&read_text(path, Some(HEAD_LIMIT))) == id
            {
                found = Some(relative.to_string());
            }
            found.is_none()
        });
        found
    }

    /// Opens the note with this id where it now is. False when there is none.
    fn follow_note(&mut self, id: &str) -> bool {
        if self.following {
            return false;
        }
        let Some(relative) = self.find_note_by_id(id) else {
            return false;
        };
        self.following = true;
        self.lost_note = None;
        if let Some(queued) = &mut self.queued_save {
            queued.note = relative.clone(); // its edits go where the note went
        }
        let (folder, file) = split_path(&relative);
        self.show_note(folder, file); // unsaved edits merge on its load (keep_edits_as_conflict)
        self.following = false;
        true
    }

    /// Makes `name` in `folder` the open note, wherever the backend was.
    fn show_note(&mut self, folder: &str, name: &str) {
        self.current_folder = folder.to_string();
        self.current_note = name.to_string();
        self.rebuild_notes();
        self.load_current_note();
    }

    /// The open note, with `content`, as [`Backend::keep_edits_as_new_note`]
    /// wants it remembered.
    fn lost_copy(&mut self, content: &str) -> LostNote {
        let stem = self
            .current_note
            .rsplit_once('.')
            .map_or("", |(stem, _)| stem)
            .to_string();
        let mode = self.vault_title_mode();
        LostNote {
            folder: self.current_folder.clone(),
            title: syncmodel::preview_note(content, &stem, &mode).title,
            content: content.to_string(),
        }
    }

    /// The open note's file is gone and no file carries its id: close it,
    /// but remember it, so unsaved edits can still be kept.
    fn lose_current_note(&mut self) {
        let content = self.note_content.clone();
        self.lost_note = Some(self.lost_copy(&content));
        self.close_note();
    }

    /// Unsaved edits that cannot go into their note (deleted elsewhere, or
    /// carrying conflict markers a merge would nest): a new note beside
    /// where it was, never with the note's id (that would bring a deleted
    /// one back as a copy). `why` says it, given the note's title and the
    /// new note's.
    fn keep_edits_as_new_note(&mut self, mine: &str, why: fn(&str, &str) -> String) -> bool {
        let Some(lost) = self.lost_note.clone() else {
            return false;
        };
        let folder = if self.vault.folder(&lost.folder).is_dir() {
            lost.folder.as_str()
        } else {
            ""
        };
        let mut title = Self::clean_name(&lost.title);
        if title.is_empty() {
            title = "Untitled".into();
        }
        let dir = self.vault.folder(folder);
        let mut name = format!("{title} (unsaved edits).md");
        let mut n = 2;
        while dir.join(&name).exists() {
            name = format!("{title} (unsaved edits {n}).md");
            n += 1;
        }
        let body = syncmodel::restore_editor_chars(split_envelope(&lost.content).1, mine);
        if !write_text(&dir.join(&name), &body) {
            return false;
        }
        self.lost_note = None;
        self.show_note(folder, &name);
        let message = why(&title, name.strip_suffix(".md").unwrap_or(&name));
        self.append_log(&message);
        self.events.push(Event::EditsKeptAsNote(message));
        self.events.push(Event::VaultChanged);
        true
    }

    /// Shows `folder` (vault-relative, "" for every note); the open note closes.
    pub fn set_current_folder(&mut self, folder: &str) {
        if self.current_folder == folder {
            return;
        }
        self.current_folder = folder.to_string();
        self.close_note();
        self.rebuild_notes();
    }

    /// Reads the vault again: folders, the current folder's notes, the open note.
    pub fn refresh(&mut self) {
        self.rebuild_folders();
        self.rebuild_notes();
        self.load_current_note();
    }

    /// Opens a note of [`Backend::notes`].
    pub fn open_note(&mut self, name: &str) {
        if !self.notes.iter().any(|n| n == name) {
            return;
        }
        // All Notes uses vault-relative paths so identical filenames in
        // different folders remain distinct. Open the note in its actual folder.
        if let Some((folder, file)) = name.rsplit_once('/') {
            let folder = self.vault_relative(folder);
            self.set_current_folder(&folder);
            self.open_note(file);
            return;
        }
        self.lost_note = None;
        if self.current_note != name {
            self.queued_save = None; // a save waiting on a sync was the other note's
            self.current_note = name.to_string();
        }
        self.load_current_note();
    }

    // ---- the open note -------------------------------------------------------------

    /// The whole file.
    pub fn note_content(&self) -> &str {
        &self.note_content
    }

    /// The file without its ID block: what an editor shows.
    pub fn note_body(&self) -> &str {
        split_envelope(&self.note_content).1
    }

    /// The file as it is written for an editor body: the stored envelope
    /// first, untouched, so the id cannot be edited away.
    fn assemble_note(&self, body: &str) -> String {
        format!("{}{body}", split_envelope(&self.note_content).0)
    }

    pub fn note_attachments(&self) -> &[Attachment] {
        &self.note_attachments
    }

    /// Why the open note cannot be changed here, "" when it can.
    pub fn read_only_reason(&self) -> &str {
        &self.read_only_reason
    }

    /// Saves the open note with `body`, an editor's text. `editor_base` is
    /// the text the editor's edits started from (what it last loaded or
    /// saved); the command line has no editor, and saves without one.
    ///
    /// True when the note holds the text (or needs none: read-only,
    /// unchanged). False when nothing was written: the save waits for the
    /// running sync, or the file changed on disk since it was loaded
    /// ([`Event::NoteChangedOnDisk`]), or the write failed.
    pub fn save_note(&mut self, body: &str, editor_base: Option<&str>) -> bool {
        let Some(path) = self.note_absolute_path() else {
            // The note went away under these edits: keep_edits_as_conflict
            // keeps them, which the window asks for once no sync runs. Under
            // one they wait like any save, so that its end still puts them
            // somewhere (write_queued_save).
            if let (true, Some(lost), false) = (self.sync.running, &self.lost_note, body.is_empty()) {
                let base = editor_base
                    .map(str::to_string)
                    .unwrap_or_else(|| syncmodel::editor_form(split_envelope(&lost.content).1));
                self.queued_save = Some(QueuedSave {
                    note: String::new(),
                    base,
                    body: body.to_string(),
                });
            }
            return body.is_empty();
        };
        let text = self.assemble_note(&syncmodel::restore_editor_chars(self.note_body(), body));
        if !self.read_only_reason.is_empty() || text == self.note_content {
            self.queued_save = None; // an edit undone since it was queued is not written later
            return true;
        }
        // Never under a running sync: a pull may be rewriting this very
        // file. Written once it is done and its changes are loaded
        // (write_queued_save). Its base is the text the edits started from:
        // the editor's own, when the caller has one. The note may have been
        // loaded again under the sync while the editor kept its edits, and
        // taking that newer content for the base would write the edits over
        // what changed in between. Without an editor's, a save already
        // waiting keeps the base it has, and a first one takes the note as
        // loaded.
        if self.sync.running {
            let note = self.current_path();
            let waiting = self.queued_save.take().filter(|queued| queued.note == note);
            let base = match (editor_base, waiting) {
                (Some(base), _) => base.to_string(),
                (None, Some(waiting)) => waiting.base,
                (None, None) => syncmodel::editor_form(self.note_body()),
            };
            self.queued_save = Some(QueuedSave {
                note,
                base,
                body: body.to_string(),
            });
            return false;
        }
        // A pull (or another program) rewrote the note since it was loaded:
        // writing now would silently drop that change. The reload that
        // follows hands the edits to keep_edits_as_conflict instead.
        if path.exists() && read_text(&path, None) != self.note_content {
            self.events.push(Event::NoteChangedOnDisk);
            return false;
        }
        if !write_text(&path, &text) {
            return false;
        }
        self.queued_save = None;
        self.load_current_note();
        self.rebuild_notes(); // a save bumps mtime, which reorders the list
        self.events.push(Event::VaultChanged);
        true
    }

    /// Forgets a save still waiting on the running sync: the editor is back
    /// at the note as loaded, so there is nothing to write after it.
    pub fn drop_queued_save(&mut self) {
        self.queued_save = None;
    }

    /// Copies the file `source` into the open note's attachments/ folder,
    /// under a free name. The note itself is not touched: its text must gain
    /// the link for the engine to upload the file.
    pub fn add_attachment(&mut self, source: &str) -> Result<Added, Refusal> {
        let note_path = self
            .note_absolute_path()
            .ok_or_else(|| Refusal::error("no note is open"))?;
        if !self.read_only_reason.is_empty() {
            return Err(Refusal::new(
                "read_only",
                format!(
                    "{} is read-only here: this note {}",
                    self.current_path(),
                    self.read_only_reason
                ),
            )
            .hint("Add the attachment in Apple Notes; it still syncs here."));
        }
        let source_path = Path::new(source);
        let Ok(metadata) = fs::metadata(source_path) else {
            return Err(Refusal::new("not_found", format!("no file \"{source}\"")));
        };
        if !metadata.is_file() {
            return Err(Refusal::error(format!("cannot read {source}: not a regular file")));
        }
        fs::File::open(source_path).map_err(|e| io_refusal(&format!("cannot read {source}"), &e))?;
        if metadata.len() == 0 {
            return Err(Refusal::error(format!("{source} is empty: there is nothing to attach")));
        }
        if metadata.len() > MAX_ATTACHMENT_BYTES {
            return Err(Refusal::error(format!(
                "{source} is larger than 100 MB, the most that can be added as an attachment"
            ))
            .hint("Add it in Apple Notes instead."));
        }
        // One plain file name, as the engine wants an attachment's.
        let name = source_path
            .file_name()
            .unwrap_or_default()
            .to_string_lossy()
            .into_owned();
        if !attachments::is_plain_name(&name) {
            return Err(Refusal::new(
                "usage",
                format!(
                    "\"{name}\" cannot be an attachment's name: rename the file first (no control characters, at most \
                     190 bytes)"
                ),
            ));
        }
        let dir = note_path.parent().unwrap_or(Path::new("")).join("attachments");
        if fs::create_dir_all(&dir).is_err() {
            return Err(Refusal::error(format!("could not create {}", dir.display())));
        }
        // Never over a file that is there: "name 2.ext", as a pull names a
        // second attachment of the same name.
        let (stem, extension) = match name.rfind('.') {
            Some(dot) if dot > 0 => name.split_at(dot),
            _ => (name.as_str(), ""),
        };
        let mut stored = name.clone();
        let mut n = 2;
        while fs::symlink_metadata(dir.join(&stored)).is_ok() {
            stored = format!("{stem} {n}{extension}");
            n += 1;
        }
        let path = dir.join(&stored);
        copy_file(source_path, &path)
            .map_err(|e| io_refusal(&format!("could not copy {source} to {}", path.display()), &e))?;
        if let Ok(copied) = fs::metadata(&path) {
            let _ = fs::set_permissions(&path, fs::Permissions::from_mode(copied.permissions().mode() | 0o600));
        }
        Ok(Added {
            link: attachments::attachment_link(&stored),
            name: stored,
            path,
        })
    }

    /// The attachments the open note has that a save of `body` would no
    /// longer link (and so remove, at the next push).
    pub fn removed_attachments(&self, body: &str) -> Vec<String> {
        let Some(path) = self.note_absolute_path() else {
            return Vec::new();
        };
        if !self.read_only_reason.is_empty() || self.note_attachments.is_empty() {
            return Vec::new();
        }
        let kept = attachments_for(&path, body);
        self.note_attachments
            .iter()
            .filter(|attachment| !kept.iter().any(|k| k.name == attachment.name))
            .map(|attachment| attachment.name.clone())
            .collect()
    }

    /// Merges unsaved editor text into the open note, which changed on disk
    /// since the editor loaded it: `base` is what the editor started from,
    /// `mine` what it holds now. Edits to different lines merge on their own;
    /// lines both sides changed differently become conflict blocks to pick
    /// from. True when the note (or a new note, for edits that cannot go
    /// into theirs) was written.
    pub fn keep_edits_as_conflict(&mut self, base: &str, mine: &str) -> bool {
        // The reloads below reach the window, whose own unsaved text is what
        // is being merged: asked again from there, never a second time.
        if self.merging {
            return false;
        }
        self.merging = true;
        let kept = self.merge_edits(base, mine);
        self.merging = false;
        kept
    }

    fn merge_edits(&mut self, base: &str, mine: &str) -> bool {
        // Its file went away: a pull may have written the new one only after
        // removing the old, so look for its id once more before giving up on it.
        if self.current_note.is_empty()
            && let Some(lost) = &self.lost_note
        {
            let id = syncmodel::extract_note_id(&lost.content);
            if !self.follow_note(&id) {
                return self.keep_edits_as_new_note(mine, |title, new| {
                    format!("\"{title}\" was deleted elsewhere, so your unsaved edits are in a new note, \"{new}\".")
                });
            }
        }
        let Some(path) = self.note_absolute_path() else {
            return false;
        };
        if !self.read_only_reason.is_empty() || !path.exists() {
            return false;
        }
        let disk = read_text(&path, None);
        let (envelope, body) = split_envelope(&disk);
        let theirs = syncmodel::editor_form(body);
        if theirs == base || theirs == mine {
            return false;
        }
        // A conflict still unresolved on either side would nest one block in
        // another, which nothing can read back (see conflict_body). The note
        // is left as it is and the edits become a note of their own.
        if [theirs.as_str(), base, mine]
            .iter()
            .any(|text| syncmodel::has_conflict_markers(text))
        {
            self.lost_note = Some(self.lost_copy(&disk));
            return self.keep_edits_as_new_note(mine, |title, new| {
                format!(
                    "\"{title}\" has an unresolved conflict, so your unsaved edits were not merged into it. They are \
                     in a new note, \"{new}\", and \"{title}\" is unchanged."
                )
            });
        }
        let merged = syncmodel::conflict_body(body, base, mine);
        if !write_text(&path, &format!("{envelope}{merged}")) {
            return false;
        }
        self.queued_save = None; // these edits are in
        self.load_current_note();
        self.rebuild_notes();
        if !syncmodel::has_conflict_markers(&merged) {
            self.events.push(Event::VaultChanged); // a clean merge syncs as any edit
        }
        true
    }

    /// A save refused while the sync ran, now that it is done and its
    /// changes are loaded: a note the sync left alone is saved as asked; one
    /// it changed gets the edits merged in, as any change on disk under edits
    /// does; one it deleted gets them as a new note.
    pub(crate) fn write_queued_save(&mut self) {
        let Some(queued) = self.queued_save.take() else {
            return;
        };
        // Its note is gone (and nobody deleted it here): the edits follow it
        // to where it moved, or become a note of their own.
        if self.current_note.is_empty() {
            if self.lost_note.is_some() && self.keep_edits_as_conflict(&queued.base, &queued.body) {
                self.events.push(Event::QueuedSaveWritten(queued.body));
            }
            return;
        }
        if self.current_path() != queued.note {
            return; // another note is not theirs
        }
        let changed = syncmodel::editor_form(self.note_body()) != queued.base;
        let mut written = changed && self.keep_edits_as_conflict(&queued.base, &queued.body);
        // Saving over the note is only for one the sync left as the edits
        // started, or that already holds them: never over a change a merge
        // could not take (a failed write), which would drop it.
        let theirs = syncmodel::editor_form(self.note_body());
        if !written && (!changed || theirs == queued.base || theirs == queued.body) {
            // The window may hold later typing by now: its reload must not
            // take the text written here for a change from elsewhere and
            // merge the two into a conflict.
            let was_merging = std::mem::replace(&mut self.merging, true);
            written = self.save_note(&queued.body, None);
            self.merging = was_merging;
        }
        if written {
            self.events.push(Event::QueuedSaveWritten(queued.body));
        }
    }

    /// What a save of `body` would do that is worth a warning first: the
    /// guardrails', and the attachments it would remove.
    pub fn save_warning(&self, body: &str) -> String {
        let mut warnings = Vec::new();
        let guardrails = self.guardrail_warning(body);
        if !guardrails.is_empty() {
            warnings.push(guardrails);
        }
        let removed = self.removed_attachments(body);
        match removed.as_slice() {
            [] => {}
            [one] => warnings.push(format!(
                "This removes the attachment {one} from this note on all your devices."
            )),
            many => warnings.push(format!(
                "This removes the attachments {} from this note on all your devices.",
                many.join(", ")
            )),
        }
        warnings.join("\n\n")
    }

    /// What is wrong with the open note as a save of `body` would write it:
    /// conflict markers it did not have, or another note's apple-note-id.
    pub fn guardrail_warning(&self, body: &str) -> String {
        // Checks run against the full file as it would be written.
        let text = self.assemble_note(body);
        let mut warnings = Vec::new();
        if syncmodel::has_conflict_markers(&text) && !syncmodel::has_conflict_markers(&self.note_content) {
            warnings.push(
                "Unresolved conflict markers present. Push will refuse this note until they are resolved.".to_string(),
            );
        }
        let id = syncmodel::extract_note_id(&text);
        let dir = self.vault.folder(&self.current_folder);
        // A Syncthing conflict copy of a note carries its id, but the engine
        // never syncs one: nothing ambiguous about it, whether it is the
        // other file or the one being saved.
        let twin = self.notes.iter().find(|name| {
            !id.is_empty()
                && **name != self.current_note
                && self.scans.get(&dir.join(name)).is_some_and(|scan| scan.id == id)
                && !syncmodel::is_sync_conflict_name(name)
                && !syncmodel::is_sync_conflict_name(&self.current_note)
        });
        if let Some(name) = twin {
            warnings.push(format!(
                "Another note in this folder ({name}) carries the same apple-note-id. Pushing two files with one id \
                 is ambiguous, so keep only one."
            ));
        }
        warnings.join("\n\n")
    }

    // ---- changing notes ------------------------------------------------------------

    /// A note's or folder's name as given, fit for a file name: trimmed, and
    /// without the slash and NUL a name cannot hold.
    pub fn clean_name(name: &str) -> String {
        name.trim().replace(['/', '\0'], "")
    }

    /// Why sync would skip a note or folder of this name, "" when it would not.
    pub fn unsynced_name_reason(name: &str, folder: bool) -> &'static str {
        if name.starts_with('.') {
            "A name cannot start with a dot: those are skipped by sync."
        } else if folder && eq_ignoring_case(name, "attachments") {
            "That folder name holds note attachments: it is skipped by sync."
        } else {
            ""
        }
    }

    fn with_md(name: String) -> String {
        if name.to_lowercase().ends_with(".md") {
            name
        } else {
            name + ".md"
        }
    }

    /// Creates a note in the current folder and opens it.
    pub fn new_note(&mut self, name: &str) {
        let clean = Self::clean_name(name);
        if clean.is_empty() || !Self::unsynced_name_reason(&clean, false).is_empty() {
            return;
        }
        let file = Self::with_md(clean);
        let path = self.vault.folder(&self.current_folder).join(&file);
        // In-body vaults carry the title as the first line; filename vaults
        // carry it in the name and start with an empty body.
        let body = if self.vault_title_mode() == "filename" {
            String::new()
        } else {
            format!("# {}\n", &file[..file.len() - 3])
        };
        if !path.exists() && !write_text(&path, &body) {
            return;
        }
        self.rebuild_notes();
        self.open_note(&file);
    }

    /// Moves the open note to the trash; the next push moves it to Recently
    /// Deleted in iCloud.
    pub fn delete_current_note(&mut self) -> Result<(), Refusal> {
        let path = self
            .note_absolute_path()
            .ok_or_else(|| Refusal::error("No note selected."))?;
        if move_to_trash(&path).is_err() {
            return Err(Refusal::error("Could not move the note to the trash."));
        }
        self.lost_note = None; // deleted here on purpose: its edits go with it
        self.close_note();
        self.rebuild_notes();
        Ok(())
    }

    /// Retitles the open note: its first line, or its file name in a
    /// filename-as-title vault.
    pub fn rename_current_note(&mut self, title: &str) -> Result<(), Refusal> {
        let filename = self.vault_title_mode() == "filename";
        // A file name cannot hold a slash; a title line can ("Q1/Q2 plan").
        let clean = if filename {
            Self::clean_name(title)
        } else {
            title.trim().replace('\0', "")
        };
        let Some(path) = self.note_absolute_path() else {
            return Err(Refusal::error("No note selected."));
        };
        if !self.read_only_reason.is_empty() {
            return Err(Refusal::new(
                "read_only",
                "This note is read-only here. Rename it in Apple Notes.",
            ));
        }
        if clean.is_empty() {
            return Err(Refusal::error("Title is empty."));
        }
        if filename {
            // The file name IS the title: renaming the file retitles the note.
            let target = Self::with_md(clean);
            let reason = Self::unsynced_name_reason(&target, false);
            if !reason.is_empty() {
                return Err(Refusal::new("usage", reason));
            }
            if target == self.current_note {
                return Ok(());
            }
            let target_path = self.vault.folder(&self.current_folder).join(&target);
            if target_path.exists() {
                return Err(Refusal::new("exists", "A note with that name already exists."));
            }
            if !rename_file(&path, &target_path) {
                return Err(Refusal::error("Could not rename the file."));
            }
            self.current_note = target;
        } else {
            // In-body vaults carry the title in the first line: retitle that line.
            if self.note_changed_on_disk() {
                return Err(Refusal::error(CHANGED_ON_DISK));
            }
            let updated = syncmodel::retitle_in_body(&self.note_content, &clean);
            // A title line that holds an attachment's link: replacing it would
            // remove the attachment in iCloud, which nothing does unasked.
            if !self
                .removed_attachments(&syncmodel::editor_form(split_envelope(&updated).1))
                .is_empty()
            {
                return Err(Refusal::error(
                    "The title line holds an attachment. Edit the note's text instead.",
                ));
            }
            if updated != self.note_content && !write_text(&path, &updated) {
                return Err(Refusal::error("Could not write the note."));
            }
        }
        self.rebuild_notes();
        let note = self.current_note.clone();
        self.open_note(&note);
        Ok(())
    }

    /// Moves the open note to another existing folder (vault-relative, ""
    /// for the root), as a mv on disk does: the next push moves it in Notes,
    /// and takes its attachment files along.
    pub fn move_current_note(&mut self, folder: &str) -> Result<(), Refusal> {
        let Some(path) = self.note_absolute_path() else {
            return Err(Refusal::error("No note selected."));
        };
        if !self.read_only_reason.is_empty() {
            return Err(Refusal::new(
                "read_only",
                "This note is read-only here. Move it in Apple Notes.",
            ));
        }
        if !self.folders.iter().any(|f| f == folder) {
            return Err(Refusal::error(format!("No folder \"{folder}\".")));
        }
        if folder == self.current_folder {
            return Ok(());
        }
        let target = self.vault.folder(folder).join(&self.current_note);
        if target.exists() {
            return Err(Refusal::new(
                "exists",
                "A note with that name already exists in that folder.",
            ));
        }
        if !rename_file(&path, &target) {
            return Err(Refusal::error("Could not move the file."));
        }
        self.lost_note = None;
        self.rebuild_folders(); // the counts
        let note = self.current_note.clone();
        self.show_note(folder, &note);
        self.events.push(Event::VaultChanged);
        Ok(())
    }

    /// Only the version on screen is replaced: when the open note's file
    /// holds something else by now (another program, a pull), that is shown
    /// first and the caller refuses with `CHANGED_ON_DISK`.
    fn note_changed_on_disk(&mut self) -> bool {
        let on_disk = self
            .note_absolute_path()
            .map(|p| read_text(&p, None))
            .unwrap_or_default();
        if on_disk == self.note_content {
            return false;
        }
        self.refresh();
        true
    }

    // ---- conflicts -----------------------------------------------------------------

    /// The open note's conflict blocks, each side's lines marked where the
    /// other side lacks them, with a little of the text around.
    pub fn note_conflicts(&self) -> Vec<Conflict> {
        let hunks = syncmodel::parse_conflicts(&self.note_content);
        let lines: Vec<&str> = self.note_content.split('\n').collect();
        let body_start = split_envelope(&self.note_content).0.matches('\n').count();
        let side = |mine: &[String], other: &[String]| -> Vec<ConflictLine> {
            let changed = syncmodel::lines_missing_from(mine, other);
            mine.iter()
                .zip(changed)
                .map(|(text, changed)| ConflictLine {
                    text: text.clone(),
                    changed,
                })
                .collect()
        };
        // Up to two lines of the untouched text around a block, blank edges dropped.
        let context = |from: usize, to: usize, from_end: bool| -> Vec<String> {
            let mut out: &[&str] = if from < to { &lines[from..to] } else { &[] };
            while out.first().is_some_and(|line| line.trim().is_empty()) {
                out = &out[1..];
            }
            while out.last().is_some_and(|line| line.trim().is_empty()) {
                out = &out[..out.len() - 1];
            }
            let kept = if from_end {
                &out[out.len().saturating_sub(2)..]
            } else {
                &out[..out.len().min(2)]
            };
            kept.iter().map(|line| line.to_string()).collect()
        };
        (0..hunks.len())
            .map(|h| {
                let hunk = &hunks[h];
                let previous_end = if h > 0 { hunks[h - 1].last + 1 } else { body_start };
                let next_start = hunks.get(h + 1).map_or(lines.len(), |next| next.first);
                Conflict {
                    local: side(&hunk.local, &hunk.remote),
                    remote: side(&hunk.remote, &hunk.local),
                    before: context(previous_end, hunk.first, true),
                    after: context(hunk.last + 1, next_start, false),
                }
            })
            .collect()
    }

    /// The open note has conflict markers the version picker cannot read.
    pub fn note_conflicts_unreadable(&self) -> bool {
        syncmodel::has_unreadable_conflicts(&self.note_content)
    }

    /// The engine's base copy of the open note: its body as last synced.
    fn synced_copy_path(&mut self) -> Option<PathBuf> {
        let id = syncmodel::extract_note_id(&self.note_content);
        if id.is_empty() {
            return None;
        }
        self.vault_info().base_files.get(&id).cloned()
    }

    pub fn note_has_synced_copy(&mut self) -> bool {
        self.synced_copy_path().is_some_and(|path| path.exists())
    }

    /// Replaces the open note's unreadable conflict markers: `how` is
    /// "strip" (drop only the marker lines) or "synced" (back to the text
    /// last synced). The note is first copied, byte for byte, to the
    /// engine's conflict-backups/; nothing is replaced unless the copy is there.
    pub fn recover_conflicted_note(&mut self, how: &str) -> Result<Recovered, Refusal> {
        let Some(path) = self.note_absolute_path() else {
            return Err(Refusal::error("No note selected."));
        };
        if !self.read_only_reason.is_empty() {
            return Err(Refusal::new(
                "read_only",
                "This note is read-only here. Resolve it in Apple Notes.",
            ));
        }
        if self.sync.running {
            return Err(Refusal::error("Wait for the sync to finish, then try again."));
        }
        if !self.note_conflicts_unreadable() {
            return Err(Refusal::new(
                "no_conflicts",
                "This note has no unreadable conflict markers.",
            ));
        }
        if self.note_changed_on_disk() {
            return Err(Refusal::error(CHANGED_ON_DISK));
        }
        let (envelope, old_body) = split_envelope(&self.note_content);
        let envelope = envelope.to_string();
        let body = match how {
            "strip" => syncmodel::strip_conflict_markers(old_body),
            "synced" => {
                let Some(synced) = self.synced_copy_path().filter(|path| path.exists()) else {
                    return Err(Refusal::new(
                        "no_synced_copy",
                        "There is no last synced version of this note.",
                    ));
                };
                read_text(&synced, None)
            }
            _ => return Err(Refusal::error("Unknown choice.")),
        };
        // A byte-exact copy first; nothing is replaced unless it is there.
        let dir = self.vault.conflict_backup_dir().join(&self.current_folder);
        let stem = format!(
            "{} (conflict backup {}",
            &self.current_note[..self.current_note.len().saturating_sub(3)],
            jiff::Zoned::now().strftime("%Y-%m-%d %H%M%S")
        );
        let mut backup = dir.join(format!("{stem}).md"));
        let mut n = 2;
        while backup.exists() {
            backup = dir.join(format!("{stem} {n}).md"));
            n += 1;
        }
        let copied = fs::create_dir_all(&dir).is_ok()
            && copy_file(&path, &backup).is_ok()
            && read_text(&backup, None) == self.note_content;
        if !copied {
            return Err(Refusal::error("Could not back up the note, so it was left as it is."));
        }
        if !write_text(&path, &format!("{envelope}{body}")) {
            return Err(Refusal::error("Could not write the note. It is unchanged."));
        }
        self.load_current_note();
        self.rebuild_notes();
        if !syncmodel::has_conflict_markers(&self.note_content) {
            self.events.push(Event::VaultChanged);
        }
        let shown = backup
            .strip_prefix(self.vault.path())
            .unwrap_or(&backup)
            .display()
            .to_string();
        self.append_log(&format!(
            "{}: replaced unreadable conflict markers; the note as it was is in {shown}",
            self.current_path()
        ));
        let done = if how == "strip" {
            "Conflict markers removed. Read the note over: both versions' lines are kept. "
        } else {
            "Back to the last synced version. "
        };
        Ok(Recovered {
            message: format!("{done}The note as it was is saved in {shown}."),
            backup,
        })
    }

    /// Settles the open note's conflict blocks: one of "local", "remote",
    /// "both" for each, in order.
    pub fn resolve_conflicts(&mut self, choices: &[String]) -> Result<(), Refusal> {
        let Some(path) = self.note_absolute_path() else {
            return Err(Refusal::error("No note selected."));
        };
        if !self.read_only_reason.is_empty() {
            return Err(Refusal::new(
                "read_only",
                "This note is read-only here. Resolve it in Apple Notes.",
            ));
        }
        if self.note_changed_on_disk() {
            return Err(Refusal::error(CHANGED_ON_DISK));
        }
        let resolved = syncmodel::resolve_conflicts(&self.note_content, choices);
        if resolved == self.note_content {
            return Err(Refusal::error("Choose a version for every change first."));
        }
        if !write_text(&path, &resolved) {
            return Err(Refusal::error("Could not write the note."));
        }
        self.load_current_note();
        self.rebuild_notes();
        self.events.push(Event::VaultChanged);
        Ok(())
    }

    // ---- folders -------------------------------------------------------------------

    /// Creates a folder inside the current one.
    pub fn new_folder(&mut self, name: &str) {
        let clean = Self::clean_name(name);
        if clean.is_empty() || !Self::unsynced_name_reason(&clean, true).is_empty() {
            return;
        }
        let _ = fs::create_dir_all(self.vault.folder(&self.current_folder).join(clean));
        self.rebuild_folders();
    }

    /// Renames the current folder, within the folder it is in.
    pub fn rename_current_folder(&mut self, name: &str) -> Result<(), Refusal> {
        let clean = Self::clean_name(name);
        if self.current_folder.is_empty() {
            return Err(Refusal::error("All Notes cannot be renamed."));
        }
        if clean.is_empty() {
            return Err(Refusal::error("Name is empty."));
        }
        let reason = Self::unsynced_name_reason(&clean, true);
        if !reason.is_empty() {
            return Err(Refusal::new("usage", reason));
        }
        let target = join_path(split_path(&self.current_folder).0, &clean);
        if target == self.current_folder {
            return Ok(());
        }
        if self.vault.folder(&target).is_dir() {
            return Err(Refusal::new("exists", "A folder with that name already exists."));
        }
        if fs::rename(self.vault.folder(&self.current_folder), self.vault.folder(&target)).is_err() {
            return Err(Refusal::error("Could not rename the folder."));
        }
        self.current_folder = target;
        self.refresh();
        Ok(())
    }

    /// Moves the current folder and its notes to the trash.
    pub fn delete_current_folder(&mut self) -> Result<(), Refusal> {
        if self.current_folder.is_empty() {
            return Err(Refusal::error("All Notes cannot be deleted."));
        }
        // its notes go to Recently Deleted on push
        if move_to_trash(&self.vault.folder(&self.current_folder)).is_err() {
            return Err(Refusal::error("Could not move the folder to the trash."));
        }
        self.set_current_folder("");
        self.rebuild_folders();
        Ok(())
    }

    // ---- searching -----------------------------------------------------------------

    /// The notes whose text or title holds `query`, ignoring case: at most
    /// 100, each with the line that matched.
    pub fn search_vault(&mut self, query: &str) -> Vec<SearchHit> {
        self.search(query.trim(), None, false).hits
    }

    /// The window's search, which runs on every key: the text matches of
    /// [`Backend::search_vault`] among the first [`WINDOW_SEARCH_NOTES`]
    /// notes only, and, with `read_all` (Recall will rank them), every one of
    /// those notes as read. A query starting with `~` asks for meaning alone:
    /// its text matches are those of the rest.
    pub fn search_window(&mut self, query: &str, read_all: bool) -> WindowSearch {
        let query = query.trim();
        let query = query.strip_prefix('~').map_or(query, str::trim);
        self.search(query, Some(WINDOW_SEARCH_NOTES), read_all)
    }

    fn search(&mut self, query: &str, most_notes: Option<usize>, read_all: bool) -> WindowSearch {
        let mut found = WindowSearch::default();
        if utf16_len(query) < 2 {
            return found;
        }
        let needle = folded(query);
        let mode = self.vault_title_mode();
        let mut scanned = 0;
        for_each_note_file(self.vault.path(), &mut |relative, path| {
            if found.hits.len() >= SEARCH_HITS && !read_all {
                return false;
            }
            if most_notes == Some(scanned) {
                found.truncated = true;
                return false;
            }
            scanned += 1;
            let text = read_text(path, Some(SEARCH_LIMIT));
            let (folder, file) = split_path(relative);
            let stem = file.rsplit_once('.').map_or(file, |(stem, _)| stem);
            let preview = syncmodel::preview_note(&text, stem, &mode);
            let hit = SearchHit {
                folder: folder.to_string(),
                file: file.to_string(),
                title: preview.title.clone(),
                snippet: preview.snippet.clone(),
            };
            if read_all {
                found.read.push(ReadNote {
                    path: relative.to_string(),
                    text: left_utf16(&text, 4000),
                    hit: hit.clone(),
                });
            }
            if found.hits.len() >= SEARCH_HITS
                || !folded_contains(&folded(&text), &needle) && !folded_contains(&folded(&preview.title), &needle)
            {
                return true;
            }
            let snippet = text
                .split('\n')
                .filter(|line| folded_contains(&folded(line), &needle))
                .map(|line| left_utf16(&syncmodel::strip_markdown_lead(line), 120))
                .find(|snippet| !snippet.is_empty());
            found.hits.push(SearchHit {
                snippet: snippet.unwrap_or(preview.snippet),
                ..hit
            });
            true
        });
        found
    }

    // ---- the PDF -------------------------------------------------------------------

    /// Saves the open note as a PDF next to it, never over an existing one.
    /// `render` lays the Markdown out and writes the file: the part that
    /// needs a toolkit, which this crate has none of. Returns the PDF's path.
    pub fn export_pdf(&mut self, render: &dyn Fn(&str, &Path) -> io::Result<()>) -> Result<PathBuf, Refusal> {
        let Some(path) = self.note_absolute_path() else {
            return Err(Refusal::error("No note selected."));
        };
        let pdf = path.with_extension("pdf");
        if pdf.exists() {
            let name = pdf.file_name().unwrap_or_default().to_string_lossy();
            return Err(Refusal::new(
                "exists",
                format!("Already exists (not overwritten): {name}"),
            ));
        }
        // In-body notes open with their title line; filename notes gain the
        // title as a heading, the way the editor shows it above the body.
        let markdown = if self.vault_title_mode() == "filename" {
            format!(
                "# {}\n\n{}",
                self.note_details(&self.current_note).title,
                self.note_body()
            )
        } else {
            self.note_body().to_string()
        };
        if render(&markdown, &pdf).is_err() || !pdf.exists() {
            return Err(Refusal::error("Could not write the PDF."));
        }
        Ok(pdf)
    }

    /// The open note's file is gone, or holds something else than was loaded.
    pub(crate) fn open_note_changed(&self) -> bool {
        self.note_absolute_path()
            .is_some_and(|path| !path.exists() || read_text(&path, None) != self.note_content)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_copy_never_replaces_or_removes_a_file_that_is_there() {
        let dir = tempfile::tempdir().unwrap();
        let (from, to) = (dir.path().join("from"), dir.path().join("to"));
        fs::write(&from, "new").unwrap();
        fs::write(&to, "someone else's").unwrap();
        assert_eq!(copy_file(&from, &to).unwrap_err().kind(), io::ErrorKind::AlreadyExists);
        assert_eq!(fs::read_to_string(&to).unwrap(), "someone else's");
        // One that fails once its own file is made takes that file back.
        let made = dir.path().join("made");
        assert!(copy_file(dir.path(), &made).is_err());
        assert!(!made.exists());
        copy_file(&from, &made).unwrap();
        assert_eq!(fs::read_to_string(&made).unwrap(), "new");
    }
}
