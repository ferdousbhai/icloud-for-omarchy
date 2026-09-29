//! Upload to iCloud Photos.
//!
//! UNVERIFIED AGAINST APPLE BY THIS PROJECT. No request/response has been
//! captured from icloud.com for this app yet; the flow below is ported from
//! timlaing/pyicloud `services/photos_cloudkit/upload.py` (MIT), whose
//! fixtures say they were confirmed against a live account. The older
//! single-POST `uploadimagews` endpoint that the icloudpd fork used has
//! answered HTTP 410 Gone since 2026-08-25, so it is not implemented.
//!
//! Four requests over two hosts, all through the session (which attaches
//! the cookie jar only on the iCloud service hosts, not the upload URL):
//!
//! 1. `POST {photosupload}/photosupload/createUploadUrl`
//!    `{"zoneName":"PrimarySync","assets":{<client uuid>: <bytes>}}`
//!    → `{"uploadUrls":{<uuid>: <https content-host URL with its own token>}}`
//! 2. `POST <that URL>` with the raw file → `{"singleFile":{...receipt...}}`
//! 3. `POST {photosupload}/photosupload/putAsset`
//!    `{"zoneName","files":[{fileName,lastModDate(ms),timeZoneOffset(min, JS sign),
//!      singleFileUploadRequest:<receipt verbatim>}],"localTimeZoneId","importGroup"}`
//!    → `[{"uploadJobId","cplMaster","cplAsset","response":{"status":200|409,...}}]`
//!    409 = iCloud already has this file; the record names are the existing asset.
//! 4. `POST {photosupload}/photosupload/uploadStatus` `{"uploadJobIds":[..]}`
//!    → `{<jobId>: {"progress": 0..100} | {"errorCode": 404}}`
//!
//! The new asset is not queryable for ~15-20 s after step 3; the caller runs
//! an incremental sync after `wait_for_ingest`.

use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use serde_json::{Value, json};

use crate::cloudkit::ZONE;
use crate::transport::{Error, Result, Transport};

pub const CREATE_UPLOAD_URL: &str = "/photosupload/createUploadUrl";
pub const PUT_ASSET: &str = "/photosupload/putAsset";
pub const UPLOAD_STATUS: &str = "/photosupload/uploadStatus";
const DUPLICATE: i64 = 409;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Step {
    Reserving,
    Sending { bytes: u64 },
    Registering,
    Ingesting { progress: i64 },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Uploaded {
    pub asset_id: String,
    pub master_id: String,
    pub job_id: Option<String>,
    /// iCloud already had this file; the ids are the existing asset's.
    pub duplicate: bool,
}

/// Local time zone as the web client sends it: an IANA id and the offset in
/// JavaScript's `getTimezoneOffset()` sign (UTC+2 → -120).
pub fn local_time_zone() -> (String, i64) {
    let now = gtk::glib::DateTime::now_local().ok();
    let minutes = now.as_ref().map_or(0, |d| -(d.utc_offset().as_minutes()));
    let id = std::env::var("TZ")
        .ok()
        .map(|tz| tz.trim_start_matches(':').to_owned())
        .filter(|tz| tz.contains('/'))
        .or_else(|| {
            std::fs::read_link("/etc/localtime").ok().and_then(|p| {
                let s = p.to_string_lossy().into_owned();
                s.split_once("zoneinfo/").map(|(_, z)| z.to_owned())
            })
        })
        .unwrap_or_else(|| "UTC".into());
    (id, minutes)
}

pub struct Uploader<'t> {
    t: &'t dyn Transport,
    base: String,
}

impl<'t> Uploader<'t> {
    /// Resolves the `photosupload` web service.
    pub fn connect(t: &'t dyn Transport) -> Result<Self> {
        let base = t.service_url("photosupload")?;
        Ok(Self::with_base(t, &base))
    }

    pub fn with_base(t: &'t dyn Transport, base: &str) -> Self {
        Self {
            t,
            base: base.trim_end_matches('/').to_owned(),
        }
    }

    fn url(&self, path: &str) -> String {
        format!("{}{path}", self.base)
    }

    /// Upload one file. `client_id` is the per-file UUID the reservation is
    /// keyed by (random in the app, fixed in tests).
    pub fn upload(&self, path: &Path, client_id: &str, progress: &dyn Fn(Step)) -> Result<Uploaded> {
        // Streamed from disk by post_file: a multi-GB video is never in memory.
        let size = std::fs::metadata(path)?.len();
        let file_name = path
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_else(|| "photo".into());
        let modified_ms = std::fs::metadata(path)
            .and_then(|m| m.modified())
            .ok()
            .and_then(|t| t.duration_since(UNIX_EPOCH).ok())
            .unwrap_or_else(|| SystemTime::now().duration_since(UNIX_EPOCH).unwrap_or_default())
            .as_millis() as i64;

        progress(Step::Reserving);
        let reserved = self.t.post_json(
            &self.url(CREATE_UPLOAD_URL),
            &json!({ "zoneName": ZONE, "assets": { client_id: size } }),
        )?;
        let target = reserved
            .pointer(&format!(
                "/uploadUrls/{}",
                client_id.replace('~', "~0").replace('/', "~1")
            ))
            .and_then(Value::as_str)
            .ok_or_else(|| Error::Other("createUploadUrl did not return an upload URL".into()))?
            .to_owned();
        self.check_target(&target)?;

        progress(Step::Sending { bytes: size });
        // The bare body to the bare URL, as pyicloud does: icloud-session
        // sends neither the cookie jar nor its client params to hosts outside
        // the iCloud service list, so the signed upload URL gets neither.
        let receipt = self.t.post_file(&target, "application/octet-stream", path)?;
        let single = receipt
            .get("singleFile")
            .cloned()
            .ok_or_else(|| Error::Other("upload host returned no receipt".into()))?;

        progress(Step::Registering);
        let (zone_id, offset) = local_time_zone();
        let body = json!({
            "zoneName": ZONE,
            "files": [{
                "fileName": file_name,
                "lastModDate": modified_ms,
                "timeZoneOffset": offset,
                "singleFileUploadRequest": single,
            }],
            "localTimeZoneId": zone_id,
            "importGroup": client_id,
        });
        let results = self.t.post_json(&self.url(PUT_ASSET), &body)?;
        let results = results
            .as_array()
            .ok_or_else(|| Error::Other("putAsset returned an unexpected payload".into()))?;
        let [result] = results.as_slice() else {
            return Err(Error::Other(format!(
                "putAsset returned {} results for one file",
                results.len()
            )));
        };
        let status = result
            .pointer("/response/status")
            .and_then(Value::as_i64)
            .unwrap_or(200);
        let ids = (
            result.get("cplAsset").and_then(Value::as_str).map(str::to_owned),
            result.get("cplMaster").and_then(Value::as_str).map(str::to_owned),
        );
        let (Some(asset_id), Some(master_id)) = ids else {
            return Err(Error::Other(format!("putAsset gave no record names (status {status})")));
        };
        if status >= 400 && status != DUPLICATE {
            let msg = result
                .pointer("/response/errorMessage")
                .and_then(Value::as_str)
                .unwrap_or("rejected");
            return Err(Error::Http {
                status: status as u16,
                body: format!("putAsset: {msg}"),
            });
        }
        Ok(Uploaded {
            asset_id,
            master_id,
            job_id: result.get("uploadJobId").and_then(Value::as_str).map(str::to_owned),
            duplicate: status == DUPLICATE,
        })
    }

    /// The reserved URL carries a token; never send it or the file in the
    /// clear. Plain http is allowed only to loopback under the mock transport.
    fn check_target(&self, url: &str) -> Result<()> {
        let loopback = url.starts_with("http://127.0.0.1") || url.starts_with("http://localhost");
        if url.starts_with("https://") || (self.t.is_mock() && loopback) {
            Ok(())
        } else {
            Err(Error::Other(
                "upload URL is not HTTPS; refusing to send the file".into(),
            ))
        }
    }

    /// Progress (0..=100) per job; `None` for a job Apple does not know.
    pub fn status(&self, job_ids: &[String]) -> Result<Vec<Option<i64>>> {
        let v = self
            .t
            .post_json(&self.url(UPLOAD_STATUS), &json!({ "uploadJobIds": job_ids }))?;
        Ok(job_ids
            .iter()
            .map(|id| {
                let entry = v.get(id)?;
                if entry.get("errorCode").is_some() {
                    None
                } else {
                    entry.get("progress").and_then(Value::as_i64)
                }
            })
            .collect())
    }

    /// Poll `uploadStatus` until every job reports 100 (or is unknown), up to
    /// `timeout`. Progress is informative; indexing is what the next sync waits on.
    pub fn wait_for_ingest(&self, job_ids: &[String], timeout: Duration, progress: &dyn Fn(Step)) -> Result<()> {
        if job_ids.is_empty() {
            return Ok(());
        }
        let start = std::time::Instant::now();
        let mut interval = Duration::from_millis(500);
        loop {
            let states = self.status(job_ids)?;
            let min = states.iter().map(|s| s.unwrap_or(100)).min().unwrap_or(100);
            progress(Step::Ingesting { progress: min });
            if min >= 100 || start.elapsed() >= timeout {
                return Ok(());
            }
            std::thread::sleep(interval);
            interval = (interval * 2).min(Duration::from_secs(4));
        }
    }
}

/// What [`upload_batch`] reports as it goes.
#[derive(Debug)]
pub enum BatchEvent {
    /// A step of file `index` (0-based) of `total`. The final wait for
    /// iCloud to ingest the batch comes as `index == total` with no name.
    Step {
        index: usize,
        total: usize,
        name: String,
        step: Step,
    },
    /// File `index` is done. When the upload service cannot be reached at
    /// all, one event with an empty `name` carries that error.
    FileDone {
        index: usize,
        name: String,
        result: Result<Uploaded>,
    },
}

/// Counts for a whole batch.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct BatchSummary {
    pub uploaded: usize,
    /// Files iCloud already had.
    pub duplicates: usize,
    pub failed: usize,
    /// `should_stop` answered true before every file was sent.
    pub stopped: bool,
}

/// Upload `paths` one after another (each with a fresh client UUID), then
/// wait up to `ingest_timeout` for iCloud to ingest the new ones. A lapsed
/// sign-in fails the rest of the batch. `should_stop` is asked before each
/// file. The app's upload dialog and `icloud-photos upload` both run this.
pub fn upload_batch(
    t: &dyn Transport,
    paths: &[PathBuf],
    ingest_timeout: Duration,
    should_stop: &dyn Fn() -> bool,
    on: &dyn Fn(BatchEvent),
) -> BatchSummary {
    let total = paths.len();
    let mut summary = BatchSummary::default();
    let up = match Uploader::connect(t) {
        Ok(u) => u,
        Err(e) => {
            on(BatchEvent::FileDone {
                index: 0,
                name: String::new(),
                result: Err(e),
            });
            summary.failed = total;
            return summary;
        }
    };
    let mut jobs = Vec::new();
    for (index, path) in paths.iter().enumerate() {
        if should_stop() {
            summary.stopped = true;
            break;
        }
        let name = path
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_default();
        let client_id = uuid::Uuid::new_v4().to_string();
        let result = up.upload(path, &client_id, &|step| {
            on(BatchEvent::Step {
                index,
                total,
                name: name.clone(),
                step,
            })
        });
        let sign_in = matches!(&result, Err(e) if e.is_sign_in());
        match &result {
            Ok(u) if u.duplicate => summary.duplicates += 1,
            Ok(u) => {
                summary.uploaded += 1;
                jobs.extend(u.job_id.clone());
            }
            Err(_) if sign_in => summary.failed += total - index,
            Err(_) => summary.failed += 1,
        }
        on(BatchEvent::FileDone { index, name, result });
        if sign_in {
            break;
        }
    }
    if !jobs.is_empty() {
        let _ = up.wait_for_ingest(&jobs, ingest_timeout, &|step| {
            on(BatchEvent::Step {
                index: total,
                total,
                name: String::new(),
                step,
            })
        });
    }
    summary
}

/// Is this a file iCloud Photos takes?
pub fn is_supported(path: &Path) -> bool {
    path.extension()
        .and_then(|e| e.to_str())
        .map(|e| e.to_ascii_lowercase())
        .is_some_and(|e| {
            matches!(
                e.as_str(),
                "jpg" | "jpeg" | "heic" | "heif" | "png" | "gif" | "tif" | "tiff" | "mov" | "mp4" | "m4v" | "dng"
            )
        })
}
