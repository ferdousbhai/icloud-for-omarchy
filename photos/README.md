# Photos (icloud-photos)

iCloud Photos for Omarchy: browse your library by month and album, download
originals, upload photos and videos, and move photos to Recently Deleted.

## Requirements

- An Apple ID with **Advanced Data Protection turned off** (off by default)
  and **Access iCloud Data on the Web turned on**, the same as for Notes. On
  iPhone/iPad: Settings → your name → iCloud → Advanced Data Protection.
- The shared iCloud sign-in, `icloud-session` (installed with the app). Sign
  in once, from Notes or from the banner this app shows, and every iCloud app
  on the machine uses that sign-in.

## Install

On Omarchy (or any Arch Linux), one command trusts the package-signing key,
adds the signed `[icloud-session]` and `[icloud-photos]` repositories, and
installs the app:

```bash
curl -fsSL https://ferdousbhai.com/icloud-photos/install.sh | sudo bash
```

Updates arrive with `omarchy update`. The script is [`install.sh`](install.sh)
in this repo; the one-liner runs the copy attached to the latest release.

To build and run from source instead (needs gtk4, libadwaita, Rust, and the
`icloud-session` checkout next to this one, at `../icloud-session`):

```bash
./bin/build
./target/release/icloud-photos
```

## Everyday use

- **Browse**: All Photos and your albums on the left, photos grouped by month
  on the right, newest first. Thumbnails are iCloud's own small previews,
  fetched as you scroll; nothing is decoded or resized on this machine.
- **View**: click a photo. Left/Right (or the arrows, or a swipe) move
  between photos. The viewer shows iCloud's large preview; Live Photos and
  videos are marked.
- **Download** (`Ctrl+S` in the viewer) saves the original to
  `~/Pictures/iCloud/<year>/<month>/`, keeping Apple's file name (a second
  `IMG_0001.HEIC` from the same month becomes `IMG_0001 (2).HEIC`). A Live
  Photo brings its video half along as the matching `.MOV`.
- **Open** (`Ctrl+O`) downloads the original if needed and opens it in your
  default app. Videos play this way.
- **Delete** (`Del`) moves the photo to Recently Deleted in iCloud, on every
  device, recoverable there for about 30 days. The downloaded original, if
  any, stays on disk.
- **Upload** (`Ctrl+U`, the + button, or drop files on the window) sends
  photos and videos to your library, with a progress dialog. A file iCloud
  already has is reported as such, not uploaded twice.
- **Sync** runs on launch, when you come back to the window after a couple of
  minutes, every 10 minutes, and on `Ctrl+R`.
- **Preferences** (`Ctrl+,`): the folder for originals, and whether originals
  download on demand (default) or all of them in the background.
- When Apple ends the web session, a banner offers **Sign In**; syncing
  resumes on its own afterwards.

## Your files

| what | where |
|---|---|
| originals you downloaded | `~/Pictures/iCloud/` (changeable in Preferences) |
| catalog (what is in your library) | `~/.local/share/icloud-photos/catalog.db` |
| thumbnails and viewer previews | `~/.cache/icloud-photos/{thumbs,medium}/` |
| preferences | `~/.config/icloud-photos/settings.json` |

Deleting the cache or the catalog is safe: the next sync fetches them again.

## What has and has not been checked against Apple

Everything here talks to Apple's private web endpoints, which are not
documented. The request and response shapes come from pyicloud
(picklepete/pyicloud and timlaing/pyicloud), not from captures made for this
app; `tests/fixtures/README.md` says where each fixture came from. Not yet
run against a real account:

- **Reading** (`records/query` on `com.apple.photos.cloud`, zone
  `PrimarySync`: albums, the asset list, album members, master lookups):
  pyicloud's long-standing shapes. The most likely to work as is.
- **Incremental sync** (`changes/zone` with a sync token kept in the
  catalog's `meta` table): unverified for PrimarySync from the web. When it
  errors in any way other than a lapsed sign-in, the app falls back to a
  full listing and stores a fresh token, so a wrong guess costs time, not
  data.
- **Delete** (`records/modify`, `isDeleted = 1` on the CPLAsset with its
  current change tag): matches a sanitised browser capture published with
  timlaing/pyicloud.
- **Upload**: the four-step `photosupload` flow (`createUploadUrl`, bytes
  to the reserved content URL, `putAsset`, `uploadStatus`) ported from
  timlaing/pyicloud, whose fixtures say they matched a live account. The
  older single-request `uploadimagews` endpoint used by the icloudpd fork
  has answered 410 Gone since 2026-08-25 and is not used.

## Limitations

- Shared albums, Shared Library, Hidden and Recently Deleted are not shown.
  Hidden photos are left out of All Photos, as on icloud.com.
- Folders of albums are flattened: their albums appear in the list, the
  folders themselves do not.
- No editing, favourites, album management, or moving photos between albums.
- HEIC originals download fine but show in the viewer through iCloud's JPEG
  preview.
- Packages are built for x86_64 only.

## Development

Run the app without an Apple account against the fake CloudKit server,
which serves 120 generated photos in three albums and keeps a change log so
deletes and uploads come back through incremental sync:

```bash
cargo run --example fake_cloudkit -- --port 8765            # add --signed-out to test the banner
ICLOUD_SESSION_MOCK=1 ICLOUD_SESSION_MOCK_URL=http://127.0.0.1:8765 cargo run
```

`ICLOUD_SESSION_MOCK=1` swaps the session for a plain local HTTP client
(`MockTransport` in `src/transport.rs`); every web service resolves to
`ICLOUD_SESSION_MOCK_URL`. Point `HOME` (or the `XDG_*` variables)
somewhere else to keep the dev catalog and downloads out of your own.

Layout:

- `src/cloudkit.rs` CloudKit requests and record parsing
- `src/sync.rs` full listing and incremental `changes/zone`
- `src/catalog.rs` the SQLite catalog
- `src/thumbs.rs` downloads (thumbnails, previews, originals) on a small pool
- `src/upload.rs` upload (unverified, see above)
- `src/transport.rs` the HTTP seam every module above goes through, and the mock
- `src/session.rs` the only file that uses the `icloud-session` crate
- `src/ui/` the GTK 4 / libadwaita app

The `session` cargo feature (on by default) links `../icloud-session`; with
`--no-default-features` the app builds without it and only mock mode works,
which keeps development going while that crate changes. `bin/test` falls
back to that automatically; `bin/release` does not.

`bin/test` runs clippy, the test suite (fixtures in `tests/fixtures/`, plus
an end-to-end run against the fake server), and the installer hash check.
In debug builds, `ICLOUD_PHOTOS_SCREENSHOT=out.png` (optionally with
`ICLOUD_PHOTOS_SCREENSHOT_VIEW=viewer` or `prefs`) renders the window to a
PNG after the first sync and quits, so the UI can be checked headless with
`GDK_BACKEND=broadway`.

## Releasing

Same as icloud-notes: `bin/release 0.1.0` runs the tests, tags, builds the
package from `pkgbuild/PKGBUILD` with `makepkg`, signs it and the repository
database with the key `install.sh` pins, publishes the GitHub release, and
has `bin/verify-release` install it with the public one-liner in a clean
Arch container, rolling the release back if that fails. The package-signing
key and its backup are described in icloud-notes' README.

The `add_signed_repo` function in `install.sh` is shared verbatim with the
other installers (icloud-notes, icloud-findmy, ghost); `tests/add_signed_repo.sha256`
pins its hash here as in each twin, so change them all together.
