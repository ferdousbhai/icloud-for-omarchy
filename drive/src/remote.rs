//! iCloud Drive as the sync engine sees it: a tree of [`Node`]s behind the
//! [`Remote`] trait, and [`DriveApi`], the real one, over the web API that
//! icloud.com uses (`drivews` for the tree, `docws` for bytes) through the
//! shared `icloud-session` sign-in. Tests drive the engine with an in-memory
//! `Remote` instead.
//!
//! What the web API does, measured on a real account (2026-11):
//! - A folder's etag changes, at once and all the way up, when something
//!   below it is added, removed or changes size. It does not change for a
//!   rename or for new content of the same size, so etags alone miss edits.
//! - CloudKit's change feed for the `com.apple.clouddocs` container names
//!   every changed document and folder (not what changed, and names are
//!   encrypted there), at once. That feed says which folders to list again.
//! - A listing of an id that no longer exists answers 200 with no items and
//!   `status: "ID_INVALID"`: never an empty folder. Every listing's status
//!   is checked for that reason.
//! - The etag sent with a rename is not enforced (a stale one is accepted),
//!   so etags detect change here and are not relied on as a lock.
//! - `modify_file` replaces a file's content and keeps its id. Sent with
//!   `allow_conflict: false` and an etag that is no longer the file's, it
//!   is refused (409 `OP_LOCK_FAILURE`), so a replace cannot overwrite a
//!   version this computer has not seen.
//! - The `mtime` sent with a new file is kept as its `dateModified`; a
//!   replaced file gets the time of the replacement instead.
//! - The upload URL is on a content host, which takes no cookies.

use std::io::Write;
use std::path::Path;

use serde_json::{Value, json};

/// The top folder's id.
pub const ROOT_ID: &str = "FOLDER::com.apple.CloudDocs::root";

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error(transparent)]
    Session(#[from] icloud_session::Error),
    /// iCloud answered, but refused or flagged the operation.
    #[error("iCloud Drive: {0}")]
    Api(String),
    /// A listing was not `OK` (the folder is gone, or the answer partial).
    #[error("iCloud Drive could not list {id}: {status}")]
    Listing { id: String, status: String },
    /// A package (a Pages document, an app bundle): not a plain file.
    #[error("{0} is a package, which is not synced")]
    Package(String),
    /// A replace refused because the file changed in iCloud since it was
    /// listed (409 on a stale etag).
    #[error("{0} changed in iCloud meanwhile")]
    Stale(String),
    /// The change feed cannot continue from the token it was given.
    #[error("iCloud's change feed starts over")]
    FeedReset,
    #[error("iCloud did not offer the {0} service for this account")]
    NotOffered(String),
    #[error(transparent)]
    Io(#[from] std::io::Error),
}

pub type Result<T> = std::result::Result<T, Error>;

/// A content host's 401 or 403 means its signed URL expired, not that the
/// session did (the session crate leaves that call to the tool): a problem
/// with this one file, tried again next sync.
fn content_host(e: icloud_session::Error) -> Error {
    match e {
        icloud_session::Error::Http { status: 401 | 403, .. } => {
            Error::Api("iCloud's upload or download link expired".into())
        }
        other => Error::Session(other),
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    File,
    Folder,
    /// An app's own folder (Pages, Shortcuts): listed, never synced.
    AppLibrary,
}

/// One file or folder in iCloud Drive.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Node {
    /// `drivewsid`: `FILE::<zone>::<docwsid>` or `FOLDER::…`.
    pub id: String,
    pub docwsid: String,
    pub zone: String,
    /// The parent folder's `drivewsid`.
    pub parent: String,
    /// The whole file name, extension included.
    pub name: String,
    pub kind: Kind,
    pub etag: String,
    /// Bytes; 0 for a folder.
    pub size: u64,
    /// `dateModified` in Unix milliseconds; 0 for a folder.
    pub mtime_ms: i64,
}

/// What changed in iCloud Drive since a token, as far as the feed tells:
/// which items, not how.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct Changes {
    /// `docwsid`s of folders that changed or hold something that did.
    pub folders: Vec<String>,
    /// `docwsid`s of files that changed, moved or went.
    pub files: Vec<String>,
    /// Where the next call continues from.
    pub token: String,
}

/// What the engine needs from iCloud Drive.
pub trait Remote {
    /// The changes since `token`; with none, only a token to start from.
    /// [`Error::FeedReset`] when the token is no longer accepted.
    fn changes(&self, token: Option<&str>) -> Result<Changes>;
    /// A folder and its direct children. Fails unless the listing is `OK`.
    fn list(&self, folder_id: &str) -> Result<(Node, Vec<Node>)>;
    fn create_folder(&self, parent_id: &str, name: &str) -> Result<Node>;
    /// Uploads `path` as a new file called `name` in `parent`.
    fn upload_new(&self, parent: &Node, name: &str, path: &Path, mtime_ms: i64) -> Result<Node>;
    /// Replaces `node`'s content with `path`; the node keeps its id.
    fn replace(&self, node: &Node, path: &Path, mtime_ms: i64) -> Result<Node>;
    fn rename(&self, node: &Node, name: &str) -> Result<Node>;
    fn move_to(&self, node: &Node, parent_id: &str) -> Result<Node>;
    /// Moves to Recently Deleted. Nothing here deletes for good.
    fn trash(&self, node: &Node) -> Result<()>;
    /// Writes the file's bytes to `dest` (a temp name the caller renames).
    fn download(&self, node: &Node, dest: &Path) -> Result<()>;
}

// ------------------------------------------------------------------ parsing

/// `2026-11-11T02:25:58Z` in Unix milliseconds; 0 when unreadable.
pub fn parse_date(text: &str) -> i64 {
    let b = text.as_bytes();
    if b.len() < 19 || b[4] != b'-' || b[7] != b'-' || b[10] != b'T' || b[13] != b':' || b[16] != b':' {
        return 0;
    }
    let num = |from: usize, to: usize| text.get(from..to).and_then(|s| s.parse::<i64>().ok());
    let (Some(y), Some(mo), Some(d), Some(h), Some(mi), Some(s)) =
        (num(0, 4), num(5, 7), num(8, 10), num(11, 13), num(14, 16), num(17, 19))
    else {
        return 0;
    };
    let days = icloud_session::time::days_from_civil(y, mo as u32, d as u32);
    (days * 86_400 + h * 3600 + mi * 60 + s) * 1000
}

fn text(v: &Value, key: &str) -> String {
    v[key].as_str().unwrap_or_default().to_string()
}

/// A `drivews` item (a listing's folder or one of its `items`).
pub fn parse_item(v: &Value) -> Result<Node> {
    let id = text(v, "drivewsid");
    if id.is_empty() {
        return Err(Error::Api(format!("an item without a drivewsid: {v}")));
    }
    let kind = match v["type"].as_str() {
        Some("FILE") => Kind::File,
        Some("FOLDER") => Kind::Folder,
        // APP_LIBRARY, APP_CONTAINER and anything newer: listed, not synced.
        _ => Kind::AppLibrary,
    };
    let mut name = text(v, "name");
    if kind == Kind::File
        && let Some(ext) = v["extension"].as_str().filter(|e| !e.is_empty())
    {
        name = format!("{name}.{ext}");
    }
    Ok(Node {
        id,
        docwsid: text(v, "docwsid"),
        zone: text(v, "zone"),
        parent: text(v, "parentId"),
        name,
        kind,
        etag: text(v, "etag"),
        size: v["size"].as_u64().unwrap_or(0),
        mtime_ms: v["dateModified"].as_str().map(parse_date).unwrap_or(0),
    })
}

/// A whole `retrieveItemDetailsInFolders` answer for one folder.
pub fn parse_listing(folder_id: &str, answer: &Value) -> Result<(Node, Vec<Node>)> {
    let folder = &answer[0];
    let status = folder["status"].as_str().unwrap_or("missing");
    if status != "OK" {
        return Err(Error::Listing {
            id: folder_id.to_string(),
            status: status.to_string(),
        });
    }
    let items = folder["items"].as_array().ok_or_else(|| Error::Listing {
        id: folder_id.to_string(),
        status: "no items".to_string(),
    })?;
    // `numberOfItems` is what Apple says the folder holds: a shorter list
    // is a partial answer, which must not read as deletions.
    if let Some(n) = folder["numberOfItems"].as_u64()
        && n as usize != items.len()
    {
        return Err(Error::Listing {
            id: folder_id.to_string(),
            status: format!("{} of {n} items", items.len()),
        });
    }
    let children = items.iter().map(parse_item).collect::<Result<Vec<_>>>()?;
    Ok((parse_item(folder)?, children))
}

/// The `document` of an `update/documents` answer (`add_file`,
/// `modify_file`), as a node in `zone`.
pub fn parse_document(answer: &Value, zone: &str) -> Result<Node> {
    let result = &answer["results"][0];
    let code = result["status"]["status_code"].as_i64().unwrap_or(-1);
    if code != 0 {
        return Err(Error::Api(format!(
            "upload refused: {}",
            result["status"]["error_message"]
                .as_str()
                .or(answer["status"]["error_message"].as_str())
                .unwrap_or("no reason given")
        )));
    }
    let doc = &result["document"];
    let docwsid = text(doc, "document_id");
    if docwsid.is_empty() {
        return Err(Error::Api("upload answered without a document".into()));
    }
    Ok(Node {
        id: format!("FILE::{zone}::{docwsid}"),
        parent: format!("FOLDER::{zone}::{}", text(doc, "parent_id")),
        docwsid,
        zone: zone.to_string(),
        name: text(doc, "name"),
        kind: Kind::File,
        etag: text(doc, "etag"),
        size: doc["size"].as_u64().unwrap_or(0),
        mtime_ms: doc["mtime"].as_i64().unwrap_or(0),
    })
}

/// Adds one page of the change feed's records to `changes`.
pub fn parse_changes(zone: &Value, changes: &mut Changes) -> Result<()> {
    if let Some(code) = zone["serverErrorCode"].as_str() {
        return Err(match code {
            "CHANGE_TOKEN_EXPIRED" | "ZONE_NOT_FOUND" | "BAD_REQUEST" => Error::FeedReset,
            other => Error::Api(format!("the change feed answered {other}")),
        });
    }
    let folder_of = |name: &str| name.strip_prefix("directory/").map(str::to_string);
    for record in zone["records"].as_array().into_iter().flatten() {
        let name = record["recordName"].as_str().unwrap_or_default();
        let parent = record["fields"]["parent"]["value"]["recordName"]
            .as_str()
            .and_then(folder_of);
        if let Some(folder) = folder_of(name) {
            changes.folders.push(folder);
        } else if let Some((_, id)) = name.split_once('/') {
            changes.files.push(id.to_string());
        }
        changes.folders.extend(parent);
    }
    match zone["syncToken"].as_str() {
        Some(token) => {
            changes.token = token.to_string();
            Ok(())
        }
        None => Err(Error::Api("the change feed gave no token".into())),
    }
}

/// The one item of a `renameItems` / `moveItems` / `createFolders` answer,
/// which must say `OK`.
fn parse_changed(answer: &Value, list: &str, what: &str) -> Result<Node> {
    let item = &answer[list][0];
    match item["status"].as_str() {
        Some("OK") => parse_item(item),
        other => Err(Error::Api(format!("{what} refused: {}", other.unwrap_or("no status")))),
    }
}

// ----------------------------------------------------------------- the API

/// iCloud Drive over the shared sign-in.
pub struct DriveApi {
    session: icloud_session::Session,
}

impl DriveApi {
    pub fn connect() -> Result<Self> {
        // Upload forms a crash left behind. One sync runs at a time, so
        // none of them is in use.
        if let Ok(entries) = std::fs::read_dir(upload_dir()) {
            for entry in entries.flatten() {
                if entry.file_name().to_string_lossy().starts_with("upload-") {
                    let _ = std::fs::remove_file(entry.path());
                }
            }
        }
        Ok(Self {
            session: icloud_session::Session::connect()?,
        })
    }

    /// The signed-in account's id: state is kept per account.
    pub fn dsid(&self) -> &str {
        self.session.dsid()
    }

    fn service(&self, key: &str) -> Result<String> {
        self.session
            .webservices()?
            .url(key)
            .map(str::to_owned)
            .ok_or_else(|| Error::NotOffered(key.to_owned()))
    }

    fn drivews(&self, call: &str, body: &Value) -> Result<Value> {
        let url = format!("{}/{call}", self.service("drivews")?);
        Ok(self.session.post_json(&url, body)?.json()?)
    }

    /// Steps one and two of an upload: ask where, send the bytes. Returns
    /// the new document id and the `data` object the last step names.
    fn send_content(&self, zone: &str, name: &str, path: &Path) -> Result<(String, Value)> {
        let docws = self.service("docws")?;
        // The content host wants a multipart form. The session crate
        // streams a file as a request body, so the form is written to a
        // temp file in the cache folder, not held in memory. It is written
        // before the upload URL is asked for, so a large file's copy does
        // not use up the signed URL's lifetime.
        let boundary = format!("----icloud-drive-{}", uuid::Uuid::new_v4().simple());
        let mut form = tempfile_in_tmp()?;
        let safe_name = name.replace(['"', '\r', '\n'], "_");
        write!(
            form.file,
            "--{boundary}\r\nContent-Disposition: form-data; name=\"files\"; filename=\"{safe_name}\"\r\nContent-Type: application/octet-stream\r\n\r\n"
        )?;
        let size = std::io::copy(&mut std::fs::File::open(path)?, &mut form.file)?;
        write!(form.file, "\r\n--{boundary}--\r\n")?;
        form.file.flush()?;
        let slot: Value = self
            .session
            .post_json(
                &format!("{docws}/ws/{zone}/upload/web"),
                &json!({"filename": name, "type": "FILE", "content_type": "application/octet-stream", "size": size}),
            )?
            .json()?;
        let (Some(document_id), Some(url)) = (slot[0]["document_id"].as_str(), slot[0]["url"].as_str()) else {
            return Err(Error::Api(format!("no upload URL for {name}")));
        };
        let sent: Value = self
            .session
            .post_file(url, &format!("multipart/form-data; boundary={boundary}"), &form.path)
            .map_err(content_host)?
            .json()?;
        let file = &sent["singleFile"];
        if file["fileChecksum"].is_null() {
            return Err(Error::Api(format!("the upload of {name} was not accepted")));
        }
        let mut data = json!({
            "signature": file["fileChecksum"],
            "wrapping_key": file["wrappingKey"],
            "reference_signature": file["referenceChecksum"],
            "size": file["size"],
        });
        // An empty file has no receipt.
        if !file["receipt"].is_null() {
            data["receipt"] = file["receipt"].clone();
        }
        Ok((document_id.to_string(), data))
    }

    fn update_document(&self, zone: &str, body: &Value) -> Result<Node> {
        let docws = self.service("docws")?;
        let answer: Value = self
            .session
            .post_json(&format!("{docws}/ws/{zone}/update/documents"), body)?
            .json()?;
        parse_document(&answer, zone)
    }
}

/// A temp file removed when dropped.
struct TempFile {
    path: std::path::PathBuf,
    file: std::fs::File,
}

/// On disk, not in `/run` or `/tmp`, which are memory on most systems: the
/// form is as large as the file it carries.
fn upload_dir() -> std::path::PathBuf {
    let cache = std::env::var_os("XDG_CACHE_HOME")
        .map(std::path::PathBuf::from)
        .filter(|p| p.is_absolute())
        .or_else(|| std::env::var_os("HOME").map(|home| std::path::PathBuf::from(home).join(".cache")))
        .unwrap_or_else(std::env::temp_dir);
    cache.join("icloud-drive")
}

fn tempfile_in_tmp() -> std::io::Result<TempFile> {
    let dir = upload_dir();
    std::fs::create_dir_all(&dir)?;
    let path = dir.join(format!("upload-{}", uuid::Uuid::new_v4().simple()));
    let file = std::fs::OpenOptions::new().write(true).create_new(true).open(&path)?;
    Ok(TempFile { path, file })
}

impl Drop for TempFile {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.path);
    }
}

const FILE_FLAGS: &str = r#"{"is_writable":true,"is_executable":false,"is_hidden":false}"#;

impl Remote for DriveApi {
    fn changes(&self, token: Option<&str>) -> Result<Changes> {
        let url = format!(
            "{}/database/1/com.apple.clouddocs/production/private/changes/zone",
            self.service("ckdatabasews")?
        );
        let mut changes = Changes::default();
        let mut token = token.map(str::to_string);
        // With no token the feed replays the zone's whole history: only its
        // last token is wanted, so the records are dropped page by page.
        let from_start = token.is_none();
        loop {
            let mut zone = json!({
                "zoneID": {"zoneName": "com.apple.CloudDocs", "zoneType": "REGULAR_CUSTOM_ZONE"},
                "resultsLimit": 200,
                "desiredKeys": ["parent"],
            });
            if let Some(token) = &token {
                zone["syncToken"] = json!(token);
            }
            let answer: Value = self.session.post_json(&url, &json!({"zones": [zone]}))?.json()?;
            let page = &answer["zones"][0];
            parse_changes(page, &mut changes)?;
            if from_start {
                changes.folders.clear();
                changes.files.clear();
            }
            if page["moreComing"] != true {
                return Ok(changes);
            }
            token = Some(changes.token.clone());
        }
    }

    fn list(&self, folder_id: &str) -> Result<(Node, Vec<Node>)> {
        let answer = self.drivews(
            "retrieveItemDetailsInFolders",
            &json!([{"drivewsid": folder_id, "partialData": false, "includeHierarchy": false}]),
        )?;
        parse_listing(folder_id, &answer)
    }

    fn create_folder(&self, parent_id: &str, name: &str) -> Result<Node> {
        let client_id = format!("FOLDER::UNKNOWN_ZONE::TempId-{}", uuid::Uuid::new_v4());
        let answer = self.drivews(
            "createFolders",
            &json!({"destinationDrivewsId": parent_id, "folders": [{"clientId": client_id, "name": name}]}),
        )?;
        parse_changed(&answer, "folders", "creating the folder")
    }

    fn upload_new(&self, parent: &Node, name: &str, path: &Path, mtime_ms: i64) -> Result<Node> {
        let (document_id, data) = self.send_content(&parent.zone, name, path)?;
        let flags: Value = serde_json::from_str(FILE_FLAGS).expect("constant JSON");
        self.update_document(
            &parent.zone,
            &json!({
                "command": "add_file",
                "document_id": document_id,
                "create_short_guid": true,
                "allow_conflict": true,
                "path": {"starting_document_id": parent.docwsid, "path": name},
                "data": data,
                "file_flags": flags,
                "mtime": mtime_ms,
                "btime": mtime_ms,
            }),
        )
    }

    fn replace(&self, node: &Node, path: &Path, mtime_ms: i64) -> Result<Node> {
        let (_, data) = self.send_content(&node.zone, &node.name, path)?;
        let flags: Value = serde_json::from_str(FILE_FLAGS).expect("constant JSON");
        // `allow_conflict: false` makes iCloud check the etag: a file that
        // changed there since it was listed is refused, not overwritten.
        let mut replaced = self
            .update_document(
                &node.zone,
                &json!({
                    "command": "modify_file",
                    "document_id": node.docwsid,
                    "etag": node.etag,
                    "allow_conflict": false,
                    "data": data,
                    "file_flags": flags,
                    "mtime": mtime_ms,
                }),
            )
            .map_err(|e| match e {
                Error::Session(icloud_session::Error::Http { status: 409, .. }) => Error::Stale(node.name.clone()),
                other => other,
            })?;
        // The answer's parent is the same folder; keep the id we know.
        replaced.parent = node.parent.clone();
        Ok(replaced)
    }

    fn rename(&self, node: &Node, name: &str) -> Result<Node> {
        let answer = self.drivews(
            "renameItems",
            &json!({"items": [{"drivewsid": node.id, "etag": node.etag, "name": name}]}),
        )?;
        parse_changed(&answer, "items", "the rename")
    }

    fn move_to(&self, node: &Node, parent_id: &str) -> Result<Node> {
        let answer = self.drivews(
            "moveItems",
            &json!({"destinationDrivewsId": parent_id, "items": [{"drivewsid": node.id, "etag": node.etag, "clientId": node.id}]}),
        )?;
        parse_changed(&answer, "items", "the move")
    }

    fn trash(&self, node: &Node) -> Result<()> {
        let answer = self.drivews(
            "moveItemsToTrash",
            &json!({"items": [{"drivewsid": node.id, "etag": node.etag, "clientId": node.id}]}),
        )?;
        match answer["items"][0]["status"].as_str() {
            Some("OK") => Ok(()),
            other => Err(Error::Api(format!(
                "moving {} to Recently Deleted refused: {}",
                node.name,
                other.unwrap_or("no status")
            ))),
        }
    }

    fn download(&self, node: &Node, dest: &Path) -> Result<()> {
        if node.size == 0 {
            // Nothing to fetch for an empty file.
            std::fs::write(dest, b"")?;
            return Ok(());
        }
        let docws = self.service("docws")?;
        let token: Value = self
            .session
            .get(&format!(
                "{docws}/ws/{}/download/by_id?document_id={}",
                node.zone, node.docwsid
            ))?
            .json()?;
        if !token["package_token"].is_null() {
            return Err(Error::Package(node.name.clone()));
        }
        let url = token["data_token"]["url"]
            .as_str()
            .ok_or_else(|| Error::Api(format!("no download URL for {}", node.name)))?;
        let written = self.session.download(url, dest).map_err(content_host)?;
        if written != node.size {
            let _ = std::fs::remove_file(dest);
            return Err(Error::Api(format!(
                "{} arrived as {written} bytes, not the {} iCloud lists",
                node.name, node.size
            )));
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dates_parse_to_unix_milliseconds() {
        assert_eq!(parse_date("1970-01-01T00:00:00Z"), 0);
        assert_eq!(parse_date("2023-11-14T22:13:20Z"), 1_700_000_000_000);
        assert_eq!(parse_date("nonsense"), 0);
    }

    #[test]
    fn a_file_item_joins_name_and_extension() {
        let item = json!({"dateModified": "2023-11-14T22:13:20Z", "docwsid": "D1", "drivewsid": "FILE::com.apple.CloudDocs::D1",
            "etag": "a::b", "extension": "txt", "name": "probe", "parentId": "FOLDER::com.apple.CloudDocs::P", "size": 21,
            "type": "FILE", "zone": "com.apple.CloudDocs"});
        let node = parse_item(&item).unwrap();
        assert_eq!(node.name, "probe.txt");
        assert_eq!(
            (node.size, node.mtime_ms, node.kind),
            (21, 1_700_000_000_000, Kind::File)
        );
    }

    #[test]
    fn app_libraries_are_their_own_kind() {
        let item = json!({"drivewsid": "FOLDER::com.apple.CloudDocs::appDocuments_com.apple.Pages", "name": "Pages", "type": "APP_LIBRARY"});
        assert_eq!(parse_item(&item).unwrap().kind, Kind::AppLibrary);
    }

    #[test]
    fn a_vanished_folder_is_an_error_not_an_empty_folder() {
        let answer = json!([{"items": [], "numberOfItems": 0, "status": "ID_INVALID"}]);
        assert!(matches!(parse_listing("X", &answer), Err(Error::Listing { .. })));
    }

    #[test]
    fn a_short_listing_is_an_error() {
        let answer =
            json!([{"drivewsid": "FOLDER::z::r", "type": "FOLDER", "items": [], "numberOfItems": 3, "status": "OK"}]);
        assert!(matches!(parse_listing("X", &answer), Err(Error::Listing { .. })));
    }

    #[test]
    fn the_feed_names_changed_files_and_the_folders_to_list_again() {
        let page = json!({"moreComing": false, "syncToken": "T2", "records": [
            {"recordName": "documentContent/DOC1", "deleted": false},
            {"recordName": "documentStructure/DOC1", "fields": {"parent": {"value": {"recordName": "directory/DIR1"}}}},
            {"recordName": "directory/DIR2", "fields": {"parent": {"value": {"recordName": "directory/root"}}}},
        ]});
        let mut changes = Changes::default();
        parse_changes(&page, &mut changes).unwrap();
        assert_eq!(changes.files, ["DOC1", "DOC1"]);
        assert_eq!(changes.folders, ["DIR1", "DIR2", "root"]);
        assert_eq!(changes.token, "T2");
        let expired = json!({"serverErrorCode": "CHANGE_TOKEN_EXPIRED"});
        assert!(matches!(parse_changes(&expired, &mut changes), Err(Error::FeedReset)));
    }

    #[test]
    fn an_upload_answer_becomes_a_node() {
        let answer = json!({"results": [{"document": {"document_id": "D1", "etag": "gvg::gvf", "mtime": 1794363951000i64,
            "name": "probe.txt", "parent_id": "P1", "size": 21, "zone": "com.apple.CloudDocs"},
            "status": {"error_message": "OK", "status_code": 0}}]});
        let node = parse_document(&answer, "com.apple.CloudDocs").unwrap();
        assert_eq!(node.id, "FILE::com.apple.CloudDocs::D1");
        assert_eq!(node.parent, "FOLDER::com.apple.CloudDocs::P1");
        let refused = json!({"results": [{"status": {"error_message": "nope", "status_code": 12}}]});
        assert!(parse_document(&refused, "z").is_err());
    }
}
