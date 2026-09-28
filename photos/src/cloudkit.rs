//! iCloud Photos over CloudKit web services: the private database
//! `com.apple.photos.cloud`, zone `PrimarySync`.
//!
//! Request and response shapes follow pyicloud's photos service
//! (picklepete/pyicloud `services/photos.py`, timlaing/pyicloud
//! `services/photos_cloudkit/`), which is the only public description of them.
//! Every call is a POST of a JSON body to
//! `{ckdatabasews}/database/1/com.apple.photos.cloud/production/private/<op>`.

use std::collections::HashMap;

use base64::Engine;
use serde_json::{Value, json};

use crate::transport::{Error, Result, Transport};

pub const ZONE: &str = "PrimarySync";
pub const DB_PATH: &str = "/database/1/com.apple.photos.cloud/production/private";
/// All photos, hidden and deleted excluded. Paged `ASCENDING` from rank 0:
/// `DESCENDING` would need the item count first to start from the end.
pub const LIST_ALL: &str = "CPLAssetAndMasterByAssetDateWithoutHiddenOrDeleted";
pub const LIST_ALBUMS: &str = "CPLAlbumByPositionLive";
pub const LIST_ALBUM_MEMBERS: &str = "CPLContainerRelationLiveByAssetDate";
/// CPLAsset + CPLMaster records per page (pyicloud asks for 2x its page size).
pub const PAGE_LIMIT: usize = 200;
const ROOT_FOLDER: &str = "----Root-Folder----";
const ALBUM_TYPE_FOLDER: i64 = 3;

/// Fields we read. CloudKit returns only these when `desiredKeys` is set.
pub const DESIRED_KEYS: &[&str] = &[
    "recordName", "recordType", "recordChangeTag", "masterRef", "isDeleted", "isExpunged", "isHidden",
    "assetDate", "addedDate", "filenameEnc", "itemType",
    "resOriginalRes", "resOriginalFileType", "resOriginalWidth", "resOriginalHeight",
    "resJPEGThumbRes", "resJPEGThumbFileType", "resJPEGThumbWidth", "resJPEGThumbHeight",
    "resJPEGMedRes", "resJPEGMedFileType", "resJPEGMedWidth", "resJPEGMedHeight",
    "resOriginalVidComplRes", "resOriginalVidComplFileType",
    "resVidSmallRes", "resVidSmallFileType", "resVidMedRes", "resVidMedFileType",
    "duration", "albumNameEnc", "albumType", "position", "parentId", "containerId", "itemId",
];

/// One raw CloudKit record (or tombstone, when `record_type` is `None`).
#[derive(Debug, Clone)]
pub struct Record {
    pub name: String,
    pub record_type: Option<String>,
    pub change_tag: Option<String>,
    pub deleted: bool,
    pub fields: serde_json::Map<String, Value>,
    pub created_ms: Option<i64>,
}

impl Record {
    pub fn parse(v: &Value) -> Option<Record> {
        Some(Record {
            name: v.get("recordName")?.as_str()?.to_owned(),
            record_type: v.get("recordType").and_then(Value::as_str).map(str::to_owned),
            change_tag: v.get("recordChangeTag").and_then(Value::as_str).map(str::to_owned),
            deleted: v.get("deleted").and_then(Value::as_bool).unwrap_or(false),
            fields: v.get("fields").and_then(Value::as_object).cloned().unwrap_or_default(),
            created_ms: v.pointer("/created/timestamp").and_then(Value::as_i64),
        })
    }

    fn value(&self, key: &str) -> Option<&Value> {
        self.fields.get(key).and_then(|f| f.get("value"))
    }

    fn int(&self, key: &str) -> Option<i64> {
        let v = self.value(key)?;
        v.as_i64().or_else(|| v.as_f64().map(|f| f as i64))
    }

    fn str(&self, key: &str) -> Option<&str> {
        self.value(key).and_then(Value::as_str)
    }

    /// `ENCRYPTED_BYTES` fields such as `filenameEnc` carry base64 UTF-8.
    fn decoded(&self, key: &str) -> Option<String> {
        let raw = self.str(key)?;
        match base64::engine::general_purpose::STANDARD.decode(raw) {
            Ok(bytes) => String::from_utf8(bytes).ok(),
            Err(_) => Some(raw.to_owned()),
        }
    }

    fn reference(&self, key: &str) -> Option<&str> {
        self.value(key)?.get("recordName")?.as_str()
    }

    fn resource(&self, prefix: &str) -> Option<Resource> {
        let token = self.value(&format!("{prefix}Res"))?;
        Some(Resource {
            url: token.get("downloadURL")?.as_str()?.to_owned(),
            size: token.get("size").and_then(Value::as_i64).unwrap_or(0),
            file_type: self.str(&format!("{prefix}FileType")).map(str::to_owned),
        })
    }

    pub fn is_type(&self, t: &str) -> bool {
        self.record_type.as_deref() == Some(t)
    }

    /// Soft-deleted (Recently Deleted) or tombstoned.
    pub fn is_deleted(&self) -> bool {
        self.deleted || self.int("isDeleted") == Some(1) || self.int("isExpunged") == Some(1)
    }

    /// In the Hidden album. `changes/zone` reports these like any other
    /// asset (only the listing index leaves them out).
    pub fn is_hidden(&self) -> bool {
        self.int("isHidden") == Some(1)
    }
}

/// A signed download URL for one rendition. URLs expire; see `lookup_masters`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Resource {
    pub url: String,
    pub size: i64,
    pub file_type: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    Photo,
    Video,
}

impl Kind {
    pub fn as_str(self) -> &'static str {
        match self {
            Kind::Photo => "photo",
            Kind::Video => "video",
        }
    }

    pub fn parse(s: &str) -> Kind {
        if s == "video" { Kind::Video } else { Kind::Photo }
    }
}

/// The master-side (file) facts of an asset: name, size, renditions.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MasterInfo {
    pub master_id: String,
    pub filename: String,
    pub size: i64,
    pub width: i64,
    pub height: i64,
    pub kind: Kind,
    pub original: Option<Resource>,
    pub thumb: Option<Resource>,
    pub medium: Option<Resource>,
    /// The video half of a Live Photo.
    pub live: Option<Resource>,
}

impl MasterInfo {
    pub fn from_record(m: &Record) -> MasterInfo {
        let original = m.resource("resOriginal");
        let item_type = m.str("itemType").or(original.as_ref().and_then(|r| r.file_type.as_deref())).unwrap_or("");
        // A Live Photo's master carries the resVid* renditions of its video
        // half too, so only the UTI decides; a paired video means a photo.
        let live = m.resource("resOriginalVidCompl");
        let is_video = live.is_none() && is_video_uti(item_type);
        let filename = m.decoded("filenameEnc").unwrap_or_else(|| format!("{}{}", sanitize(&m.name), extension_for(item_type)));
        MasterInfo {
            master_id: m.name.clone(),
            filename,
            size: original.as_ref().map_or(0, |r| r.size),
            width: m.int("resOriginalWidth").unwrap_or(0),
            height: m.int("resOriginalHeight").unwrap_or(0),
            kind: if is_video { Kind::Video } else { Kind::Photo },
            thumb: m.resource("resJPEGThumb"),
            medium: m.resource("resJPEGMed"),
            live,
            original,
        }
    }
}

/// One photo or video: a CPLAsset (date, deleted flag) joined to its CPLMaster.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Asset {
    /// CPLAsset recordName; what delete and album membership refer to.
    pub id: String,
    pub change_tag: Option<String>,
    /// Unix seconds, from `assetDate` (falls back to `addedDate`).
    pub created: i64,
    pub deleted: bool,
    pub master: MasterInfo,
}

impl Asset {
    pub fn is_live(&self) -> bool {
        self.master.live.is_some()
    }
}

/// The asset-side facts, for when a change arrives without its master.
#[derive(Debug, Clone)]
pub struct AssetPart {
    pub id: String,
    pub master_id: String,
    pub change_tag: Option<String>,
    pub created: i64,
    pub deleted: bool,
}

impl AssetPart {
    pub fn from_record(a: &Record) -> Option<AssetPart> {
        let ms = a.int("assetDate").or_else(|| a.int("addedDate")).or(a.created_ms).unwrap_or(0);
        Some(AssetPart {
            id: a.name.clone(),
            master_id: a.reference("masterRef")?.to_owned(),
            change_tag: a.change_tag.clone(),
            created: ms.div_euclid(1000),
            // Hidden assets leave All Photos and albums exactly like deleted
            // ones (downloaded files stay); unhiding brings them back.
            deleted: a.is_deleted() || a.is_hidden(),
        })
    }

    pub fn with_master(self, master: MasterInfo) -> Asset {
        Asset { id: self.id, change_tag: self.change_tag, created: self.created, deleted: self.deleted, master }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Album {
    pub id: String,
    pub name: String,
    pub position: i64,
    pub is_folder: bool,
    pub deleted: bool,
}

impl Album {
    pub fn from_record(r: &Record) -> Option<Album> {
        if r.name == ROOT_FOLDER {
            return None;
        }
        Some(Album {
            id: r.name.clone(),
            name: r.decoded("albumNameEnc")?,
            position: r.int("position").unwrap_or(0),
            is_folder: r.int("albumType") == Some(ALBUM_TYPE_FOLDER),
            deleted: r.is_deleted(),
        })
    }
}

/// Membership of an asset in an album (CPLContainerRelation).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Relation {
    pub id: String,
    pub album_id: String,
    pub asset_id: String,
    pub deleted: bool,
}

impl Relation {
    pub fn from_record(r: &Record) -> Option<Relation> {
        Some(Relation {
            id: r.name.clone(),
            album_id: r.str("containerId")?.to_owned(),
            asset_id: r.str("itemId")?.to_owned(),
            deleted: r.is_deleted(),
        })
    }
}

/// CPLAsset records joined to CPLMaster records from one response.
#[derive(Debug, Default)]
pub struct Paired {
    pub assets: Vec<Asset>,
    /// Assets whose master was not in the response.
    pub orphans: Vec<AssetPart>,
    /// Masters no asset in the response referred to.
    pub lone_masters: Vec<MasterInfo>,
}

pub fn pair_assets(records: &[Record]) -> Paired {
    let masters: HashMap<&str, &Record> = records
        .iter()
        .filter(|r| r.is_type("CPLMaster") && !r.is_deleted())
        .map(|r| (r.name.as_str(), r))
        .collect();
    let mut out = Paired::default();
    let mut used = std::collections::HashSet::new();
    for rec in records.iter().filter(|r| r.is_type("CPLAsset")) {
        let Some(part) = AssetPart::from_record(rec) else { continue };
        match masters.get(part.master_id.as_str()) {
            Some(m) => {
                used.insert(m.name.as_str());
                out.assets.push(part.with_master(MasterInfo::from_record(m)));
            }
            None => out.orphans.push(part),
        }
    }
    out.lone_masters = records
        .iter()
        .filter(|r| r.is_type("CPLMaster") && !r.is_deleted() && !used.contains(r.name.as_str()))
        .map(MasterInfo::from_record)
        .collect();
    out
}

/// One page of a records/query.
#[derive(Debug, Default)]
pub struct QueryPage {
    pub records: Vec<Record>,
    pub continuation: Option<String>,
    pub sync_token: Option<String>,
}

/// One page of changes/zone.
#[derive(Debug, Default)]
pub struct ZoneChanges {
    pub records: Vec<Record>,
    pub sync_token: String,
    pub more_coming: bool,
}

/// Outcome of a successful records/modify on one record.
#[derive(Debug, Clone)]
pub struct Modified {
    pub name: String,
    pub change_tag: Option<String>,
}

pub struct CloudKit<'t> {
    t: &'t dyn Transport,
    base: String,
}

impl<'t> CloudKit<'t> {
    /// Resolves `ckdatabasews` through the session's webservices map.
    pub fn connect(t: &'t dyn Transport) -> Result<Self> {
        let root = t.service_url("ckdatabasews")?;
        Ok(Self::with_root(t, &root))
    }

    pub fn with_root(t: &'t dyn Transport, ckdatabasews: &str) -> Self {
        Self { t, base: format!("{}{}", ckdatabasews.trim_end_matches('/'), DB_PATH) }
    }

    pub fn transport(&self) -> &'t dyn Transport {
        self.t
    }

    /// The session appends the client params (clientBuildNumber, dsid, ...);
    /// these two are the Photos-specific ones pyicloud adds.
    fn url(&self, op: &str) -> String {
        format!("{}/{op}?remapEnums=true&getCurrentSyncToken=true", self.base)
    }

    fn zone() -> Value {
        json!({ "zoneName": ZONE })
    }

    fn post(&self, op: &str, body: &Value) -> Result<Value> {
        let v = self.t.post_json(&self.url(op), body)?;
        if let Some(code) = v.get("serverErrorCode").and_then(Value::as_str) {
            let reason = v.get("reason").and_then(Value::as_str).unwrap_or("").to_owned();
            return Err(Error::CloudKit { code: code.to_owned(), reason });
        }
        Ok(v)
    }

    pub fn query(&self, record_type: &str, filters: Vec<Value>, continuation: Option<&str>) -> Result<QueryPage> {
        let mut query = json!({ "recordType": record_type });
        if !filters.is_empty() {
            query["filterBy"] = Value::Array(filters);
        }
        let mut body = json!({
            "query": query,
            "zoneID": Self::zone(),
            "resultsLimit": PAGE_LIMIT,
            "desiredKeys": DESIRED_KEYS,
        });
        if let Some(c) = continuation {
            body["continuationMarker"] = json!(c);
        }
        let v = self.post("records/query", &body)?;
        Ok(QueryPage {
            records: records_of(&v),
            continuation: v.get("continuationMarker").and_then(Value::as_str).map(str::to_owned),
            sync_token: v.get("syncToken").and_then(Value::as_str).map(str::to_owned),
        })
    }

    /// Apple indexes a library before the web can list it.
    pub fn indexing_finished(&self) -> Result<bool> {
        let v = self.post("records/query", &json!({ "query": { "recordType": "CheckIndexingState" }, "zoneID": Self::zone() }))?;
        let state = v.pointer("/records/0/fields/state/value").and_then(Value::as_str);
        Ok(state.is_none_or(|s| s == "FINISHED"))
    }

    /// User albums, folders flattened (folders themselves are dropped).
    pub fn albums(&self) -> Result<Vec<Album>> {
        let mut out = Vec::new();
        self.albums_under(None, &mut out, 0)?;
        Ok(out)
    }

    fn albums_under(&self, parent: Option<&str>, out: &mut Vec<Album>, depth: usize) -> Result<()> {
        let filters = parent.map(|p| vec![string_filter("parentId", p)]).unwrap_or_default();
        let mut records = Vec::new();
        let mut cont: Option<String> = None;
        loop {
            let page = self.query(LIST_ALBUMS, filters.clone(), cont.as_deref())?;
            records.extend(page.records);
            match page.continuation {
                Some(c) if Some(&c) != cont.as_ref() => cont = Some(c),
                _ => break,
            }
        }
        for album in records.iter().filter_map(Album::from_record).filter(|a| !a.deleted) {
            if album.is_folder {
                if depth < 8 {
                    self.albums_under(Some(&album.id), out, depth + 1)?;
                }
            } else if !out.iter().any(|a: &Album| a.id == album.id) {
                out.push(album);
            }
        }
        Ok(())
    }

    /// Page through an asset list index, calling `each` per page.
    /// Follows `continuationMarker` when Apple sends one, else advances
    /// `startRank` by the number of masters, as pyicloud does.
    pub fn list_assets(
        &self,
        list_type: &str,
        extra_filters: &[Value],
        mut each: impl FnMut(&[Record]) -> Result<()>,
    ) -> Result<()> {
        // `rank` is what the query sends; a continuation re-sends the same
        // query, so it only moves (to every master seen) once a marker ends.
        let mut rank: i64 = 0;
        let mut seen: i64 = 0;
        let mut cont: Option<String> = None;
        let mut previous_first: Option<String> = None;
        for _ in 0..100_000 {
            let mut filters = vec![int_filter("startRank", rank), string_filter("direction", "ASCENDING")];
            filters.extend_from_slice(extra_filters);
            let page = self.query(list_type, filters, cont.as_deref())?;
            let masters = page.records.iter().filter(|r| r.is_type("CPLMaster")).count() as i64;
            let first = page.records.first().map(|r| r.name.clone());
            if first.is_some() && first == previous_first {
                return Err(Error::Other("iCloud returned the same page twice".into()));
            }
            previous_first = first;
            each(&page.records)?;
            seen += masters;
            if let Some(c) = page.continuation.filter(|c| Some(c) != cont.as_ref()) {
                cont = Some(c);
                continue;
            }
            if masters == 0 {
                return Ok(());
            }
            cont = None;
            rank = seen;
        }
        Err(Error::Other("asset listing did not end".into()))
    }

    /// Asset ids (CPLAsset recordNames) in one album, plus relation ids if Apple
    /// returned the CPLContainerRelation records.
    pub fn album_members(&self, album_id: &str) -> Result<Vec<Relation>> {
        let mut out: Vec<Relation> = Vec::new();
        self.list_assets(LIST_ALBUM_MEMBERS, &[string_filter("parentId", album_id)], |records| {
            for r in records {
                if r.is_type("CPLContainerRelation") {
                    if let Some(rel) = Relation::from_record(r).filter(|rel| !rel.deleted) {
                        out.retain(|o| o.asset_id != rel.asset_id);
                        out.push(rel);
                    }
                } else if r.is_type("CPLAsset") && !out.iter().any(|o| o.asset_id == r.name) {
                    out.push(Relation { id: String::new(), album_id: album_id.to_owned(), asset_id: r.name.clone(), deleted: false });
                }
            }
            Ok(())
        })?;
        Ok(out)
    }

    /// The zone's current change token (zones/list).
    pub fn zone_sync_token(&self) -> Result<Option<String>> {
        let v = self.post("zones/list", &json!({}))?;
        Ok(v.get("zones").and_then(Value::as_array).and_then(|zones| {
            zones
                .iter()
                .find(|z| z.pointer("/zoneID/zoneName").and_then(Value::as_str) == Some(ZONE))
                .and_then(|z| z.get("syncToken")?.as_str().map(str::to_owned))
        }))
    }

    /// One page of changes/zone since `token`. Errors (HTTP or a zone-level
    /// `serverErrorCode` such as an expired token) are returned so the caller
    /// can fall back to a full query.
    pub fn zone_changes(&self, token: Option<&str>) -> Result<ZoneChanges> {
        let mut zone = json!({ "zoneID": Self::zone(), "desiredKeys": DESIRED_KEYS, "reverse": false });
        if let Some(t) = token {
            zone["syncToken"] = json!(t);
        }
        let v = self.post("changes/zone", &json!({ "zones": [zone], "resultsLimit": PAGE_LIMIT }))?;
        let z = v
            .get("zones")
            .and_then(Value::as_array)
            .and_then(|zs| zs.first())
            .ok_or_else(|| Error::Other("changes/zone returned no zone".into()))?;
        if let Some(code) = z.get("serverErrorCode").and_then(Value::as_str) {
            let reason = z.get("reason").and_then(Value::as_str).unwrap_or("").to_owned();
            return Err(Error::CloudKit { code: code.to_owned(), reason });
        }
        Ok(ZoneChanges {
            records: z.get("records").and_then(Value::as_array).map(|a| a.iter().filter_map(Record::parse).collect()).unwrap_or_default(),
            sync_token: z
                .get("syncToken")
                .and_then(Value::as_str)
                .ok_or_else(|| Error::Other("changes/zone returned no syncToken".into()))?
                .to_owned(),
            more_coming: z.get("moreComing").and_then(Value::as_bool).unwrap_or(false),
        })
    }

    /// Fresh CPLMaster records (download URLs expire) by recordName.
    pub fn lookup_masters(&self, master_ids: &[&str]) -> Result<Vec<MasterInfo>> {
        let records: Vec<Value> = master_ids.iter().map(|id| json!({ "recordName": id })).collect();
        let v = self.post("records/lookup", &json!({ "records": records, "zoneID": Self::zone(), "desiredKeys": DESIRED_KEYS }))?;
        Ok(records_of(&v).iter().filter(|r| r.is_type("CPLMaster")).map(MasterInfo::from_record).collect())
    }

    /// Move an asset to Recently Deleted: update the CPLAsset with isDeleted=1.
    /// The change tag must be the asset record's current one, or CloudKit
    /// answers a per-record CONFLICT. Shape from a browser capture of
    /// icloud.com (timlaing/pyicloud `photos_browser_mutations/photo_delete_*`).
    pub fn delete_asset(&self, asset_id: &str, change_tag: Option<&str>) -> Result<Modified> {
        let mut record = json!({
            "recordName": asset_id,
            "recordType": "CPLAsset",
            "fields": { "isDeleted": { "value": 1 } },
        });
        if let Some(tag) = change_tag {
            record["recordChangeTag"] = json!(tag);
        }
        let body = json!({
            "atomic": true,
            "operations": [{ "operationType": "update", "record": record }],
            "zoneID": Self::zone(),
        });
        let v = self.post("records/modify", &body)?;
        let rec = v
            .get("records")
            .and_then(Value::as_array)
            .and_then(|a| a.first())
            .ok_or_else(|| Error::Other("records/modify returned no record".into()))?;
        if let Some(code) = rec.get("serverErrorCode").and_then(Value::as_str) {
            let reason = rec.get("reason").and_then(Value::as_str).unwrap_or("").to_owned();
            return Err(Error::CloudKit { code: code.to_owned(), reason });
        }
        Ok(Modified {
            name: rec.get("recordName").and_then(Value::as_str).unwrap_or(asset_id).to_owned(),
            change_tag: rec.get("recordChangeTag").and_then(Value::as_str).map(str::to_owned),
        })
    }
}

fn records_of(v: &Value) -> Vec<Record> {
    v.get("records").and_then(Value::as_array).map(|a| a.iter().filter_map(Record::parse).collect()).unwrap_or_default()
}

pub fn string_filter(field: &str, value: &str) -> Value {
    json!({ "fieldName": field, "comparator": "EQUALS", "fieldValue": { "type": "STRING", "value": value } })
}

pub fn int_filter(field: &str, value: i64) -> Value {
    json!({ "fieldName": field, "comparator": "EQUALS", "fieldValue": { "type": "INT64", "value": value } })
}

/// Record names can contain `/` and `+` (master ids are base64-ish).
pub fn sanitize(name: &str) -> String {
    name.chars().map(|c| if c.is_ascii_alphanumeric() || c == '-' || c == '_' { c } else { '_' }).collect()
}

/// Movie UTIs (`itemType` / `resOriginalFileType`); images, HEIC and JPEG
/// included, are photos.
pub fn is_video_uti(uti: &str) -> bool {
    matches!(
        uti,
        "com.apple.quicktime-movie" | "public.mpeg-4" | "public.movie" | "public.video" | "public.avi" | "public.3gpp"
            | "public.3gpp2" | "com.apple.m4v-video" | "public.mpeg" | "public.mpeg-2-video"
    ) || uti.ends_with("-movie")
        || uti.ends_with("-video")
}

/// File extension for a UTI, for naming Live Photo halves and nameless files.
pub fn extension_for(uti: &str) -> &'static str {
    match uti {
        "public.jpeg" => ".JPG",
        "public.heic" => ".HEIC",
        "public.png" => ".PNG",
        "com.apple.quicktime-movie" => ".MOV",
        "public.mpeg-4" => ".MP4",
        "com.compuserve.gif" => ".GIF",
        "public.tiff" => ".TIFF",
        "com.adobe.raw-image" | "com.canon.cr2-raw-image" => ".RAW",
        _ => "",
    }
}
