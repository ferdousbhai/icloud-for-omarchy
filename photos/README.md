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

On Omarchy (or any Arch Linux), one command trusts the package-signing
key, adds the signed `[icloud-for-omarchy]` repository (all the iCloud
apps and what they share), and installs the app:

```bash
curl -fsSL https://ferdousbhai.com/icloud-photos/install.sh | sudo bash -s -- icloud-photos
```

Updates then arrive with `omarchy update`. The script is
[`install.sh`](../install.sh) at the root of this repository, attached to
every release; the one-liner runs the copy from the latest release. It
also installs an Omarchy `pre-refresh-pacman` hook so
`omarchy refresh pacman` keeps the repository. See the root
[README](../README.md#install) for uninstalling.

To build and run from source instead (needs gtk4, libadwaita and Rust, and
the icloud-session daemon installed, as [session/](../session/README.md)
describes), from the repository root:

```bash
bin/build icloud-photos
target/release/icloud-photos
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
- When you are signed out (here or from any other iCloud app), a banner
  offers **Sign In**, which opens Apple's sign-in page in a window of its
  own; syncing resumes on its own once you are signed in.

## Command line

The same binary is a command-line tool: `icloud-photos` with no arguments
opens the app; `icloud-photos <command>` runs without GTK or a display, over
the same sign-in, catalog, cache and library folder as the app.

| command | what it does |
|---|---|
| `status` | signed in?, catalog path, item/album/downloaded counts, last sync, library and cache folders |
| `sync [--full]` | incremental sync (falling back to a full listing), or `--full`; prints what changed |
| `albums` | id, name, count |
| `list [--album ID] [--since DATE] [--limit N] [--kind photo\|video\|live]` | id, date, kind, size, file name, local path if downloaded; newest first |
| `info ID` | everything the catalog knows about one item, including its albums |
| `thumb ID [--out PATH]` | fetches iCloud's thumbnail into the cache (and copies it to `PATH`) |
| `download ID... \| --all [--medium] [--out DIR]` | originals (plus a Live Photo's video) into the library folder, or under `DIR`; `--medium` fetches the viewer's preview instead; prints the saved paths |
| `upload FILE... [--album ID] [--no-sync]` | uploads, progress on stderr, prints the new ids, then syncs until they are in the catalog |
| `delete ID... [--yes]` | moves to Recently Deleted; asks on a terminal, refuses without `--yes` otherwise |
| `prune-cache` | drops cached previews of deleted items, trims the preview cache |
| `sign-in` | opens the iCloud sign-in window |
| `config [--library-dir DIR] [--download on-demand\|all]` | shows or changes the preferences |

- `--json` prints JSON on stdout (errors as a JSON object on stderr);
  otherwise output is plain text, tab-separated for lists.
- Exit codes: 0 ok, 1 error, 2 sign-in required (`status` also exits 2
  when signed out), 64 usage.
- `--data-dir DIR` keeps the catalog, cache, preferences and library all
  under `DIR` (`DIR/data`, `DIR/cache`, `DIR/config`, `DIR/library`), so a
  test run never touches your own.
- `DATE` is `YYYY-MM-DD`, `YYYY-MM-DDTHH:MM:SS` (UTC) or Unix seconds; dates
  print as UTC.
- A delete that conflicts with a change made on another device syncs and
  tries once more, as the app does.
- `upload --album` adds the new items to an album with a CloudKit request
  that has not been checked against Apple (see below).

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
- **Adding to an album** (`upload --album`, command line only):
  `records/modify` creating a `CPLContainerRelation` named
  `<asset>-IN-<album>`, the shape the album listing returns; no capture of
  icloud.com doing it has been made.
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
- No editing, favourites, album management, or moving photos between albums
  (the command line can only add new uploads to an existing album).
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

The command line works the same way, and `--data-dir` keeps it away from
your own files:

```bash
export ICLOUD_SESSION_MOCK=1 ICLOUD_SESSION_MOCK_URL=http://127.0.0.1:8765
cargo run -- --data-dir /tmp/photos-dev sync
cargo run -- --data-dir /tmp/photos-dev --json list --limit 3
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
- `src/cli.rs` the command line (no GTK), over the same library code

`cargo test -p icloud-photos` runs the test suite (fixtures in `tests/fixtures/`, plus
an end-to-end run against the fake server, and `tests/cli.rs`, which runs
the built binary against it for every command); the root `bin/test` runs
it with clippy and every other crate's tests.
In debug builds, `ICLOUD_PHOTOS_SCREENSHOT=out.png` (optionally with
`ICLOUD_PHOTOS_SCREENSHOT_VIEW=viewer` or `prefs`) renders the window to a
PNG after the first sync and quits, so the UI can be checked headless with
`GDK_BACKEND=broadway`.

## Releasing

Released with the other packages from the repository root; see the root
[README](../README.md#releasing). Its tags are `photos-v<version>`.
