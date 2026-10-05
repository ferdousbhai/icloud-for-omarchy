//! Upload against pyicloud's fixtures. These pin the request shapes we send;
//! they do not prove Apple accepts them (see src/upload.rs).

mod support;

use std::sync::Mutex;

use icloud_photos::transport::Error;
use icloud_photos::upload::{Step, Uploader, local_time_zone};
use serde_json::{Value, json};
use support::{FixtureTransport, UPLOAD_ROOT, fixture, temp_dir};

const CLIENT: &str = "11111111-2222-3333-4444-555555555555";

fn apple(put_asset: &'static str) -> FixtureTransport {
    FixtureTransport::new(move |call| match call.op.as_str() {
        "/photosupload/createUploadUrl" => Ok(fixture("write/create_upload_url.json")),
        "/photosupload/putAsset" => Ok(fixture(put_asset)),
        "/photosupload/uploadStatus" => Ok(fixture("write/upload_status.json")),
        url if url.contains("/singleFileUpload") => Ok(fixture("write/single_file_upload.json")),
        other => Err(Error::Http {
            status: 404,
            body: other.into(),
        }),
    })
}

/// A photo to upload, in a directory removed when it drops.
struct Photo {
    path: std::path::PathBuf,
    _dir: tempfile::TempDir,
}

impl std::ops::Deref for Photo {
    type Target = std::path::Path;

    fn deref(&self) -> &std::path::Path {
        &self.path
    }
}

fn photo(name: &str) -> Photo {
    let dir = temp_dir(name);
    let path = dir.path().join("IMG_9000.JPG");
    std::fs::write(&path, vec![0xFFu8; 3105]).unwrap();
    Photo { path, _dir: dir }
}

#[test]
fn uploads_in_three_requests_with_pyicloud_shapes() {
    let t = apple("write/put_asset.json");
    let steps = Mutex::new(Vec::new());
    let up = Uploader::connect(&t).unwrap();
    let out = up
        .upload(&photo("upload"), CLIENT, &|s| steps.lock().unwrap().push(s))
        .unwrap();

    assert_eq!(out.asset_id, "6D6CB701-C0BD-490D-92D4-181E47C67C7A");
    assert_eq!(out.master_id, "AX/92+r9B5N+sKNFEfAYZX0FjsNr");
    assert!(!out.duplicate);
    assert_eq!(
        out.job_id.as_deref(),
        Some("AX/92+r9B5N+sKNFEfAYZX0FjsNr#PrimarySync:6D6CB701-C0BD-490D-92D4-181E47C67C7A")
    );
    assert_eq!(
        *steps.lock().unwrap(),
        vec![Step::Reserving, Step::Sending { bytes: 3105 }, Step::Registering]
    );

    let calls = t.calls();
    assert_eq!(calls.len(), 3);
    assert_eq!(calls[0].url, format!("{UPLOAD_ROOT}/photosupload/createUploadUrl"));
    assert_eq!(
        calls[0].body,
        json!({ "zoneName": "PrimarySync", "assets": { CLIENT: 3105 } })
    );
    // Bytes go to the reserved URL, verbatim.
    let reserved = fixture("write/create_upload_url.json")["uploadUrls"][CLIENT]
        .as_str()
        .unwrap()
        .to_owned();
    assert_eq!(calls[1].url, reserved);
    assert_eq!(calls[1].bytes.as_ref().unwrap().len(), 3105);
    // putAsset echoes the receipt verbatim.
    let put = &calls[2].body;
    assert_eq!(put["zoneName"], "PrimarySync");
    assert_eq!(put["importGroup"], CLIENT);
    assert_eq!(put["files"][0]["fileName"], "IMG_9000.JPG");
    assert_eq!(
        put["files"][0]["singleFileUploadRequest"],
        fixture("write/single_file_upload.json")["singleFile"]
    );
    assert!(
        put["files"][0]["lastModDate"].as_i64().unwrap() > 1_600_000_000_000,
        "milliseconds"
    );
    let (zone, offset) = local_time_zone();
    assert_eq!(put["localTimeZoneId"], zone.as_str());
    assert_eq!(put["files"][0]["timeZoneOffset"], offset);
}

#[test]
fn the_time_zone_offset_matches_the_system_clock() {
    // date(1) works the offset out from the same TZ / /etc/localtime.
    let out = std::process::Command::new("date").arg("+%z").output().unwrap();
    let z = String::from_utf8(out.stdout).unwrap();
    let z = z.trim();
    let (sign, digits) = z.split_at(1);
    let minutes = digits[..2].parse::<i64>().unwrap() * 60 + digits[2..4].parse::<i64>().unwrap();
    let east = if sign == "-" { -minutes } else { minutes };
    // JavaScript's getTimezoneOffset() sign: UTC+2 is -120.
    assert_eq!(local_time_zone().1, -east, "date +%z said {z}");
}

#[test]
fn a_duplicate_returns_the_existing_asset() {
    let t = apple("write/put_asset_duplicate.json");
    let out = Uploader::connect(&t)
        .unwrap()
        .upload(&photo("dup"), CLIENT, &|_| {})
        .unwrap();
    assert!(out.duplicate);
    assert_eq!(out.asset_id, "6D6CB701-C0BD-490D-92D4-181E47C67C7A");
    assert!(out.job_id.is_none());
}

#[test]
fn a_rejected_registration_is_an_error() {
    let t = FixtureTransport::new(|call| match call.op.as_str() {
        "/photosupload/createUploadUrl" => Ok(fixture("write/create_upload_url.json")),
        "/photosupload/putAsset" => {
            Ok(json!([{ "cplMaster": "M", "cplAsset": "A", "response": { "status": 500, "isRetryable": true } }]))
        }
        _ => Ok(fixture("write/single_file_upload.json")),
    });
    let err = Uploader::connect(&t)
        .unwrap()
        .upload(&photo("rejected"), CLIENT, &|_| {})
        .unwrap_err();
    assert!(matches!(err, Error::Http { status: 500, .. }), "{err:?}");
}

#[test]
fn refuses_a_plain_http_upload_target() {
    let t = FixtureTransport::new(|call| match call.op.as_str() {
        "/photosupload/createUploadUrl" => {
            Ok(json!({ "uploadUrls": { CLIENT: "http://evil.example/upload?tk=secret" } }))
        }
        _ => panic!("nothing else may be called"),
    });
    let err = Uploader::connect(&t)
        .unwrap()
        .upload(&photo("http"), CLIENT, &|_| {})
        .unwrap_err();
    assert!(err.to_string().contains("not HTTPS"));
    assert_eq!(t.calls().len(), 1);
}

#[test]
fn status_maps_progress_and_unknown_jobs() {
    let job = "AX/92+r9B5N+sKNFEfAYZX0FjsNr#PrimarySync:6D6CB701-C0BD-490D-92D4-181E47C67C7A".to_owned();
    let t = FixtureTransport::new(|call| {
        let mut v = fixture("write/upload_status.json");
        for id in call.body["uploadJobIds"].as_array().unwrap() {
            let id = id.as_str().unwrap();
            if v.get(id).is_none() {
                v[id] = json!({ "errorCode": 404 });
            }
        }
        Ok(v)
    });
    let up = Uploader::connect(&t).unwrap();
    assert_eq!(up.status(&[job.clone(), "nope".into()]).unwrap(), vec![Some(95), None]);
    assert_eq!(t.calls()[0].body, json!({ "uploadJobIds": [job, "nope"] }));
}

#[test]
fn a_missing_reservation_is_an_error() {
    let t = FixtureTransport::new(|_| Ok(json!({ "uploadUrls": {} })));
    let err = Uploader::connect(&t)
        .unwrap()
        .upload(&photo("noslot"), CLIENT, &|_| {})
        .unwrap_err();
    assert!(err.to_string().contains("upload URL"));
    let _ = Value::Null;
}
