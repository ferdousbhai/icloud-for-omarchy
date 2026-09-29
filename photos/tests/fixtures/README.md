# Fixtures

None of these were captured from icloud.com by this project yet (brief, phase
2 and 4: "capture real CloudKit fixtures"). They are hand-built in the shapes
pyicloud documents, with placeholder ids and URLs:

- Read side (`*.json` here): CloudKit `records/query`, `records/lookup`,
  `zones/list` and `changes/zone` responses for the private database
  `com.apple.photos.cloud`, zone `PrimarySync`, shaped after
  picklepete/pyicloud `services/photos.py` and timlaing/pyicloud
  `tests/fixtures/photos_*`. `filenameEnc` / `albumNameEnc` are base64 UTF-8.
- `write/delete_*`: the records/modify request and response for moving an
  asset to Recently Deleted, adapted from timlaing/pyicloud
  `photos_browser_mutations/photo_delete_*` (sanitised browser captures).
  `delete_conflict_response.json` is synthesised from CloudKit's documented
  per-record error shape.
- `write/create_upload_url.json`, `single_file_upload.json`, `put_asset*.json`,
  `upload_status.json`: copied from timlaing/pyicloud
  `tests/fixtures/photos_upload/` (MIT), which states they matched a live
  account.

When real captures exist, replace these files; the tests say what changed.
