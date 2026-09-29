//! A fake iCloud Photos backend for development and end-to-end tests: the
//! CloudKit endpoints icloud-photos calls, answering in the fixture shapes,
//! with generated JPEGs as thumbnails, medium renditions and originals.
//!
//! Point the app at it with `ICLOUD_SESSION_MOCK=1 ICLOUD_SESSION_MOCK_URL=<url>`.
//! It keeps a change log, so deletes and uploads show up through
//! `changes/zone` exactly as the incremental sync expects.
#![allow(dead_code)]

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use base64::Engine;
use serde_json::{Value, json};

const DB: &str = "/database/1/com.apple.photos.cloud/production/private/";
const PAGE_PAIRS: usize = 50;
/// 2026-09-20T12:00:00Z.
const NEWEST_MS: i64 = 1_789_905_600_000;

#[derive(Clone)]
struct FakeAsset {
    id: String,
    master: String,
    filename: String,
    uti: &'static str,
    date_ms: i64,
    w: i64,
    h: i64,
    hue: f32,
    live: bool,
    video: bool,
    deleted: bool,
    tag: u32,
    /// Bytes of an uploaded file, served for every rendition.
    uploaded: Option<Arc<Vec<u8>>>,
}

struct State {
    base: String,
    assets: Vec<FakeAsset>,
    albums: Vec<(String, String)>,
    members: Vec<(String, String)>,
    /// Changed records, in order; the sync token is "tok-<len>".
    log: Vec<Value>,
    signed_out: bool,
    uploads: HashMap<String, Arc<Vec<u8>>>,
    jpegs: HashMap<(usize, &'static str), Arc<Vec<u8>>>,
    next_id: usize,
}

pub struct FakeServer {
    pub url: String,
    server: Arc<tiny_http::Server>,
    state: Arc<Mutex<State>>,
}

impl Drop for FakeServer {
    fn drop(&mut self) {
        self.server.unblock();
    }
}

impl FakeServer {
    /// `port` 0 picks a free one. `count` assets, newest on 2026-09-20.
    pub fn start(port: u16, count: usize) -> FakeServer {
        let server = Arc::new(tiny_http::Server::http(("127.0.0.1", port)).expect("bind fake server"));
        let addr = server.server_addr().to_ip().expect("ip address");
        let url = format!("http://{addr}");
        let state = Arc::new(Mutex::new(State::new(&url, count)));
        for _ in 0..4 {
            let (server, state) = (server.clone(), state.clone());
            std::thread::spawn(move || {
                while let Ok(mut req) = server.recv() {
                    let mut body = Vec::new();
                    let _ = req.as_reader().read_to_end(&mut body);
                    let (status, ctype, bytes) = handle(&state, req.method().as_str(), req.url(), &body);
                    let header = tiny_http::Header::from_bytes("Content-Type", ctype).expect("header");
                    let _ = req.respond(
                        tiny_http::Response::from_data(bytes)
                            .with_status_code(status)
                            .with_header(header),
                    );
                }
            });
        }
        FakeServer { url, server, state }
    }

    /// Answer 421 to every Apple call until `/mock/reauthenticate`.
    pub fn sign_out(&self) {
        self.state.lock().unwrap().signed_out = true;
    }

    /// Simulate an edit made on another device: the asset's change tag moves on.
    pub fn edit_elsewhere(&self, asset_id: &str) {
        let mut s = self.state.lock().unwrap();
        if let Some(i) = s.assets.iter().position(|a| a.id == asset_id) {
            s.assets[i].tag += 1;
            let rec = s.asset_record(i);
            s.log.push(rec);
        }
    }

    /// Simulate a change made on another device: delete an asset.
    pub fn delete_elsewhere(&self, asset_id: &str) {
        let mut s = self.state.lock().unwrap();
        if let Some(i) = s.assets.iter().position(|a| a.id == asset_id) {
            s.assets[i].deleted = true;
            s.assets[i].tag += 1;
            let rec = s.asset_record(i);
            s.log.push(rec);
        }
    }

    pub fn asset_ids(&self) -> Vec<String> {
        self.state
            .lock()
            .unwrap()
            .assets
            .iter()
            .filter(|a| !a.deleted)
            .map(|a| a.id.clone())
            .collect()
    }
}

impl State {
    fn new(base: &str, count: usize) -> State {
        let mut assets = Vec::new();
        for i in 0..count {
            let video = i % 10 == 7;
            let live = i % 10 == 2;
            let (uti, ext) = if video {
                ("com.apple.quicktime-movie", "MOV")
            } else if live {
                ("public.heic", "HEIC")
            } else {
                ("public.jpeg", "JPG")
            };
            let portrait = i % 4 == 1;
            // Newest first in time, spread over ~8 months.
            let date_ms = NEWEST_MS - (i as i64) * 2 * 86_400_000 - (i as i64 % 5) * 3_600_000;
            assets.push(FakeAsset {
                id: format!("{:08X}-FAKE-4000-8000-{:012X}", 0xA55E7 + i, i),
                master: format!("AX/fake+{i:05}"),
                filename: format!("IMG_{:04}.{ext}", 1000 + count - i),
                uti,
                date_ms,
                w: if portrait { 3024 } else { 4032 },
                h: if portrait { 4032 } else { 3024 },
                hue: (i as f32 * 37.0) % 360.0,
                live,
                video,
                deleted: false,
                tag: 1,
                uploaded: None,
            });
        }
        let albums = vec![
            ("ALBUM-TRIPS".to_owned(), "Trips".to_owned()),
            ("ALBUM-FAMILY".to_owned(), "Family".to_owned()),
            ("ALBUM-EMPTY".to_owned(), "Empty album".to_owned()),
        ];
        let mut members = Vec::new();
        for (i, a) in assets.iter().enumerate() {
            if i % 3 == 0 {
                members.push(("ALBUM-TRIPS".to_owned(), a.id.clone()));
            }
            if i % 5 == 0 {
                members.push(("ALBUM-FAMILY".to_owned(), a.id.clone()));
            }
        }
        State {
            base: base.to_owned(),
            assets,
            albums,
            members,
            log: Vec::new(),
            signed_out: false,
            uploads: HashMap::new(),
            jpegs: HashMap::new(),
            next_id: count,
        }
    }

    fn token(&self) -> String {
        format!("tok-{}", self.log.len())
    }

    fn zone() -> Value {
        json!({ "zoneName": "PrimarySync", "ownerRecordName": "_fake_owner", "zoneType": "REGULAR_CUSTOM_ZONE" })
    }

    fn res(&self, kind: &str, i: usize, size: i64) -> Value {
        json!({ "value": { "size": size, "fileChecksum": "AQ", "wrappingKey": "wk==", "referenceChecksum": "AR",
                           "downloadURL": format!("{}/content/{kind}/{i}", self.base) }, "type": "ASSETID" })
    }

    fn asset_record(&self, i: usize) -> Value {
        let a = &self.assets[i];
        json!({
            "recordName": a.id, "recordType": "CPLAsset", "recordChangeTag": format!("tag-{}", a.tag),
            "fields": {
                "assetDate": { "value": a.date_ms, "type": "TIMESTAMP" },
                "addedDate": { "value": a.date_ms + 60_000, "type": "TIMESTAMP" },
                "masterRef": { "value": { "recordName": a.master, "action": "DELETE_SELF", "zoneID": Self::zone() }, "type": "REFERENCE" },
                "isDeleted": { "value": a.deleted as i64, "type": "INT64" },
            },
            "pluginFields": {}, "deleted": false, "zoneID": Self::zone(),
        })
    }

    fn master_record(&self, i: usize) -> Value {
        let a = &self.assets[i];
        let b64 = base64::engine::general_purpose::STANDARD.encode(a.filename.as_bytes());
        let mut fields = json!({
            "filenameEnc": { "value": b64, "type": "ENCRYPTED_BYTES" },
            "itemType": { "value": a.uti, "type": "STRING" },
            "resOriginalRes": self.res("orig", i, 2_000_000 + i as i64 * 1000),
            "resOriginalFileType": { "value": a.uti, "type": "STRING" },
            "resOriginalWidth": { "value": a.w, "type": "INT64" },
            "resOriginalHeight": { "value": a.h, "type": "INT64" },
            "resJPEGThumbRes": self.res("thumb", i, 20_000),
            "resJPEGThumbFileType": { "value": "public.jpeg", "type": "STRING" },
            "resJPEGMedRes": self.res("med", i, 200_000),
            "resJPEGMedFileType": { "value": "public.jpeg", "type": "STRING" },
        });
        if a.live {
            fields["resOriginalVidComplRes"] = self.res("live", i, 400_000);
            fields["resOriginalVidComplFileType"] = json!({ "value": "com.apple.quicktime-movie", "type": "STRING" });
        }
        if a.video {
            fields["resVidSmallRes"] = self.res("vsmall", i, 90_000);
            fields["resVidSmallFileType"] = json!({ "value": "com.apple.quicktime-movie", "type": "STRING" });
        }
        json!({ "recordName": a.master, "recordType": "CPLMaster", "recordChangeTag": "m1", "fields": fields,
                "pluginFields": {}, "deleted": false, "zoneID": Self::zone() })
    }

    fn album_record(&self, id: &str, name: &str, pos: usize) -> Value {
        let b64 = base64::engine::general_purpose::STANDARD.encode(name.as_bytes());
        json!({ "recordName": id, "recordType": "CPLAlbum", "recordChangeTag": "a1",
                "fields": { "albumNameEnc": { "value": b64, "type": "ENCRYPTED_BYTES" }, "albumType": { "value": 0, "type": "INT64" },
                            "position": { "value": pos, "type": "INT64" }, "isDeleted": { "value": 0, "type": "INT64" } },
                "deleted": false, "zoneID": Self::zone() })
    }

    /// Visible asset indexes, oldest first (ASCENDING by asset date).
    fn ranked(&self, album: Option<&str>) -> Vec<usize> {
        let mut idx: Vec<usize> = (0..self.assets.len())
            .filter(|&i| !self.assets[i].deleted)
            .filter(|&i| {
                album.is_none_or(|al| {
                    self.members
                        .iter()
                        .any(|(m_al, m_as)| m_al == al && *m_as == self.assets[i].id)
                })
            })
            .collect();
        idx.sort_by_key(|&i| self.assets[i].date_ms);
        idx
    }

    fn query(&self, body: &Value) -> Value {
        let rtype = body.pointer("/query/recordType").and_then(Value::as_str).unwrap_or("");
        let filter = |name: &str| {
            body.pointer("/query/filterBy")
                .and_then(Value::as_array)
                .and_then(|fs| fs.iter().find(|f| f["fieldName"] == name))
                .map(|f| f["fieldValue"]["value"].clone())
        };
        let rank = filter("startRank").and_then(|v| v.as_u64()).unwrap_or(0) as usize;
        let parent = filter("parentId").and_then(|v| v.as_str().map(str::to_owned));
        let records: Vec<Value> = match rtype {
            "CheckIndexingState" => vec![json!({ "recordName": "_indexing", "recordType": "CheckIndexingState",
                                                 "fields": { "state": { "value": "FINISHED", "type": "STRING" } } })],
            "CPLAlbumByPositionLive" if parent.is_none() => self
                .albums
                .iter()
                .enumerate()
                .map(|(i, (id, name))| self.album_record(id, name, i + 1))
                .collect(),
            "CPLAssetAndMasterByAssetDateWithoutHiddenOrDeleted" | "CPLContainerRelationLiveByAssetDate" => {
                let album = if rtype.starts_with("CPLContainer") {
                    parent.as_deref().or(Some(""))
                } else {
                    None
                };
                self.ranked(album)
                    .into_iter()
                    .skip(rank)
                    .take(PAGE_PAIRS)
                    .flat_map(|i| [self.asset_record(i), self.master_record(i)])
                    .collect()
            }
            _ => Vec::new(),
        };
        json!({ "records": records, "syncToken": self.token() })
    }

    fn changes(&self, body: &Value) -> Value {
        let token = body.pointer("/zones/0/syncToken").and_then(Value::as_str);
        let from = match token {
            None => 0,
            Some(t) => match t
                .strip_prefix("tok-")
                .and_then(|n| n.parse::<usize>().ok())
                .filter(|&n| n <= self.log.len())
            {
                Some(n) => n,
                None => {
                    return json!({ "zones": [{ "zoneID": Self::zone(), "serverErrorCode": "CHANGE_TOKEN_EXPIRED", "reason": "unknown token" }] });
                }
            },
        };
        let end = (from + 200).min(self.log.len());
        json!({ "zones": [{ "zoneID": Self::zone(), "syncToken": format!("tok-{end}"), "moreComing": end < self.log.len(),
                            "records": self.log[from..end] }] })
    }

    fn lookup(&self, body: &Value) -> Value {
        let names: Vec<&str> = body["records"]
            .as_array()
            .map(|a| a.iter().filter_map(|r| r["recordName"].as_str()).collect())
            .unwrap_or_default();
        let records: Vec<Value> = names
            .iter()
            .map(
                |n| match self.assets.iter().position(|a| a.master == *n || a.id == *n) {
                    Some(i) if self.assets[i].master == *n => self.master_record(i),
                    Some(i) => self.asset_record(i),
                    None => json!({ "recordName": n, "serverErrorCode": "NOT_FOUND", "reason": "Record not found" }),
                },
            )
            .collect();
        json!({ "records": records })
    }

    fn modify(&mut self, body: &Value) -> Value {
        let mut out = Vec::new();
        for op in body["operations"].as_array().cloned().unwrap_or_default() {
            let rec = &op["record"];
            let name = rec["recordName"].as_str().unwrap_or("");
            if op["operationType"] == "create" && rec["recordType"] == "CPLContainerRelation" {
                let album = rec
                    .pointer("/fields/containerId/value")
                    .and_then(Value::as_str)
                    .unwrap_or("")
                    .to_owned();
                let asset = rec
                    .pointer("/fields/itemId/value")
                    .and_then(Value::as_str)
                    .unwrap_or("")
                    .to_owned();
                if !self.albums.iter().any(|(id, _)| *id == album)
                    || !self.assets.iter().any(|a| a.id == asset && !a.deleted)
                {
                    out.push(json!({ "recordName": name, "serverErrorCode": "NOT_FOUND", "reason": "no such album or asset" }));
                    continue;
                }
                if !self.members.iter().any(|(al, a)| *al == album && *a == asset) {
                    self.members.push((album.clone(), asset.clone()));
                }
                let r = json!({ "recordName": name, "recordType": "CPLContainerRelation", "recordChangeTag": "r1",
                                "fields": { "containerId": { "value": album, "type": "STRING" }, "itemId": { "value": asset, "type": "STRING" } },
                                "deleted": false, "zoneID": Self::zone() });
                self.log.push(r.clone());
                out.push(r);
                continue;
            }
            let Some(i) = self.assets.iter().position(|a| a.id == name) else {
                out.push(json!({ "recordName": name, "serverErrorCode": "NOT_FOUND", "reason": "Record not found" }));
                continue;
            };
            let current = format!("tag-{}", self.assets[i].tag);
            if rec["recordChangeTag"].as_str() != Some(current.as_str()) {
                out.push(json!({ "recordName": name, "serverErrorCode": "CONFLICT", "reason": "client oplock error updating record" }));
                continue;
            }
            if rec.pointer("/fields/isDeleted/value").and_then(Value::as_i64) == Some(1) {
                self.assets[i].deleted = true;
            }
            self.assets[i].tag += 1;
            let r = self.asset_record(i);
            self.log.push(r.clone());
            out.push(r);
        }
        json!({ "records": out, "syncToken": self.token() })
    }

    fn put_asset(&mut self, body: &Value) -> Value {
        let mut out = Vec::new();
        for f in body["files"].as_array().cloned().unwrap_or_default() {
            let receipt = f
                .pointer("/singleFileUploadRequest/receipt")
                .and_then(Value::as_str)
                .unwrap_or("");
            let Some(bytes) = self.uploads.get(receipt).cloned() else {
                out.push(
                    json!({ "response": { "status": 400, "isRetryable": false, "errorMessage": "unknown receipt" } }),
                );
                continue;
            };
            if let Some(dup) = self
                .assets
                .iter()
                .find(|a| !a.deleted && a.uploaded.as_ref().is_some_and(|b| **b == *bytes))
            {
                out.push(json!({ "cplMaster": dup.master, "cplAsset": dup.id, "response": { "status": 409, "errorMessage": "duplicate asset" } }));
                continue;
            }
            let n = self.next_id;
            self.next_id += 1;
            let name = f["fileName"].as_str().unwrap_or("upload.jpg").to_owned();
            let video = name.to_ascii_lowercase().ends_with(".mov") || name.to_ascii_lowercase().ends_with(".mp4");
            let now = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_millis() as i64)
                .unwrap_or(0);
            self.assets.push(FakeAsset {
                id: format!("{:08X}-UPLD-4000-8000-{:012X}", 0xB0B0 + n, n),
                master: format!("AX/upld+{n:05}"),
                filename: name,
                uti: if video {
                    "com.apple.quicktime-movie"
                } else {
                    "public.jpeg"
                },
                date_ms: f["lastModDate"].as_i64().unwrap_or(now),
                w: 1600,
                h: 1200,
                hue: 200.0,
                live: false,
                video,
                deleted: false,
                tag: 1,
                uploaded: Some(bytes),
            });
            let i = self.assets.len() - 1;
            let (a, m) = (self.asset_record(i), self.master_record(i));
            self.log.push(m);
            self.log.push(a);
            let asset = &self.assets[i];
            out.push(
                json!({ "uploadJobId": format!("{}#PrimarySync:{}", asset.master, asset.id), "cplMaster": asset.master,
                             "cplAsset": asset.id, "response": { "status": 200, "isRetryable": false } }),
            );
        }
        Value::Array(out)
    }

    fn content(&mut self, kind: &str, i: usize) -> Option<Arc<Vec<u8>>> {
        let a = self.assets.get(i)?;
        if let Some(b) = &a.uploaded {
            return Some(b.clone());
        }
        let (kind, w, h): (&'static str, usize, usize) = match kind {
            "thumb" => ("thumb", 320, 240),
            "med" => ("med", 1024, 768),
            "orig" => ("orig", 1600, 1200),
            "live" | "vsmall" => return Some(Arc::new(b"not really a movie".to_vec())),
            _ => return None,
        };
        let (w, h) = if a.h > a.w { (h, w) } else { (w, h) };
        let hue = a.hue;
        Some(
            self.jpegs
                .entry((i, kind))
                .or_insert_with(|| Arc::new(jpeg(w, h, hue, i)))
                .clone(),
        )
    }
}

fn handle(state: &Mutex<State>, method: &str, url: &str, body: &[u8]) -> (u16, &'static str, Vec<u8>) {
    let path = url.split('?').next().unwrap_or(url).to_owned();
    let json_ok = |v: Value| (200, "application/json", v.to_string().into_bytes());
    let mut s = state.lock().unwrap();
    if path == "/mock/reauthenticate" {
        s.signed_out = false;
        return json_ok(json!({ "ok": true }));
    }
    if path.starts_with("/content/") {
        let mut parts = path.trim_start_matches("/content/").split('/');
        let (kind, i) = (
            parts.next().unwrap_or(""),
            parts.next().and_then(|n| n.parse().ok()).unwrap_or(usize::MAX),
        );
        return match s.content(kind, i) {
            Some(b) => (200, "image/jpeg", b.to_vec()),
            None => (404, "text/plain", b"no such content".to_vec()),
        };
    }
    if s.signed_out {
        return (421, "application/json", br#"{"error":"Misdirected Request"}"#.to_vec());
    }
    if let Some(uuid) = path.strip_prefix("/content-upload/") {
        s.uploads.insert(uuid.to_owned(), Arc::new(body.to_vec()));
        return json_ok(
            json!({ "singleFile": { "referenceChecksum": "AZref", "size": body.len(), "fileChecksum": "AZfile",
                                               "wrappingKey": "wk==", "receipt": uuid } }),
        );
    }
    let body: Value = serde_json::from_slice(body).unwrap_or(Value::Null);
    if method != "POST" {
        return (405, "text/plain", b"POST only".to_vec());
    }
    match path.as_str() {
        "/photosupload/createUploadUrl" => {
            let urls: serde_json::Map<String, Value> = body["assets"]
                .as_object()
                .map(|m| {
                    m.keys()
                        .map(|k| (k.clone(), json!(format!("{}/content-upload/{k}", s.base))))
                        .collect()
                })
                .unwrap_or_default();
            json_ok(json!({ "uploadUrls": urls }))
        }
        "/photosupload/putAsset" => json_ok(s.put_asset(&body)),
        "/photosupload/uploadStatus" => {
            let m: serde_json::Map<String, Value> = body["uploadJobIds"]
                .as_array()
                .map(|a| {
                    a.iter()
                        .filter_map(Value::as_str)
                        .map(|id| (id.to_owned(), json!({ "progress": 100 })))
                        .collect()
                })
                .unwrap_or_default();
            json_ok(Value::Object(m))
        }
        p if p.starts_with(DB) => match &p[DB.len()..] {
            "records/query" => json_ok(s.query(&body)),
            "zones/list" => {
                json_ok(json!({ "zones": [{ "zoneID": State::zone(), "syncToken": s.token(), "deleted": false }] }))
            }
            "changes/zone" => json_ok(s.changes(&body)),
            "records/lookup" => json_ok(s.lookup(&body)),
            "records/modify" => json_ok(s.modify(&body)),
            _ => (404, "application/json", br#"{"serverErrorCode":"NOT_FOUND"}"#.to_vec()),
        },
        _ => (404, "text/plain", b"not found".to_vec()),
    }
}

/// A soft diagonal gradient in one hue with a lighter disc, so photos are
/// told apart at a glance.
fn jpeg(w: usize, h: usize, hue: f32, seed: usize) -> Vec<u8> {
    let mut px = Vec::with_capacity(w * h * 3);
    let (cx, cy, r) = (
        (w as f32) * (0.3 + (seed % 5) as f32 * 0.1),
        (h as f32) * 0.4,
        (w.min(h) as f32) * 0.22,
    );
    for y in 0..h {
        for x in 0..w {
            let t = (x as f32 / w as f32 + y as f32 / h as f32) / 2.0;
            let d = ((x as f32 - cx).powi(2) + (y as f32 - cy).powi(2)).sqrt();
            let light = if d < r { 0.85 } else { 0.35 + 0.35 * t };
            let (rr, gg, bb) = hsl(hue + t * 40.0, 0.55, light);
            px.extend_from_slice(&[rr, gg, bb]);
        }
    }
    let mut out = Vec::new();
    jpeg_encoder::Encoder::new(&mut out, 80)
        .encode(&px, w as u16, h as u16, jpeg_encoder::ColorType::Rgb)
        .expect("encode jpeg");
    out
}

fn hsl(h: f32, s: f32, l: f32) -> (u8, u8, u8) {
    let c = (1.0 - (2.0 * l - 1.0).abs()) * s;
    let hp = (h.rem_euclid(360.0)) / 60.0;
    let x = c * (1.0 - (hp % 2.0 - 1.0).abs());
    let (r, g, b) = match hp as u32 {
        0 => (c, x, 0.0),
        1 => (x, c, 0.0),
        2 => (0.0, c, x),
        3 => (0.0, x, c),
        4 => (x, 0.0, c),
        _ => (c, 0.0, x),
    };
    let m = l - c / 2.0;
    let to = |v: f32| ((v + m) * 255.0).round().clamp(0.0, 255.0) as u8;
    (to(r), to(g), to(b))
}
