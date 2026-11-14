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
curl -fsSL https://ferdousbhai.com/icloud-photos/install.sh | sudo bash
```

Updates then arrive with `omarchy update`. The one-liner runs
`install-photos.sh` from the latest release: [`install.sh`](../install.sh)
at the root of this repository, set to install just this app. It also
installs an Omarchy `pre-refresh-pacman` hook so `omarchy refresh pacman`
keeps the repository. See the root [README](../README.md#install) for
uninstalling.

To build and run from source instead (needs gtk4, libadwaita and Rust, and
the icloud-session daemon installed, as [session/](../session/README.md)
describes), from the repository root:

```bash
bin/build icloud-photos
target/release/icloud-photos       # or target/release/icloud-photos-app
```

## Everyday use

- **Browse**: All Photos and your albums on the left, photos grouped by month
  on the right, newest first. Thumbnails are iCloud's own small previews,
  fetched as you scroll; nothing is decoded or resized on this machine.
- **View**: click a photo. Left/Right (or the arrows, or a swipe) move
  between photos. The viewer shows iCloud's large preview; Live Photos and
  videos are marked.
- **Download** (`Ctrl+S` in the viewer) saves the original to
  `~/Pictures/<year>/<month>/`, keeping Apple's file name (a second
  `IMG_0001.HEIC` from the same month becomes `IMG_0001 (2).HEIC`). A Live
  Photo brings its video half along as the matching `.MOV`. A download never
  replaces a file already there, and the app never deletes, renames or
  uploads anything in the folder that it did not download (your screenshots,
  for one).
- **Open** (`Ctrl+O`) downloads the original if needed and opens it in your
  default app. Videos play this way.
- **Delete** (`Del`) moves the photo to Recently Deleted in iCloud, on every
  device, recoverable there for about 30 days. The downloaded original, if
  any, stays on disk. **Undo** on the toast that follows brings it back.
- **Copy Image** (`Ctrl+C`, or the ⋮ menu) puts the photo on the clipboard,
  so it pastes as an image anywhere; a HEIC photo is copied from iCloud's
  JPEG rendition.
- **Set as Wallpaper** (the ⋮ menu, on Omarchy) makes the photo your
  background with `omarchy-theme-bg-set`. A photo you haven't downloaded is
  kept in `~/.local/share/icloud-photos/wallpaper/`, which holds only the
  current one.
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

`icloud-photos` is also the command-line tool: with no arguments it opens
the app (`icloud-photos-app`, which the desktop entry starts directly);
`icloud-photos <command>` runs without GTK or a display, over the
same sign-in, catalog, cache and library folder as the app.

```console
$ icloud-photos status
$ icloud-photos sync
$ icloud-photos albums
$ icloud-photos list --album ID --since 2026-08-01 --limit 20
$ icloud-photos download ID... --out ~/Downloads/holiday
$ icloud-photos upload IMG_0001.HEIC --album ID
$ icloud-photos delete ID --yes          # to Recently Deleted
$ icloud-photos restore ID
```

`icloud-photos COMMAND --help` describes one command and its JSON. Every
command, the output modes (text on a terminal, JSON when piped), the exit
and error codes and the JSON shapes are in the
[command-line reference](https://github.com/ferdousbhai/icloud-for-omarchy/blob/main/docs/CLI.md).

## Your files

| what | where |
|---|---|
| originals you downloaded | `~/Pictures/<year>/<month>/` (the folder is changeable in Preferences) |
| catalog (what is in your library) | `~/.local/share/icloud-photos/catalog.db` |
| thumbnails and viewer previews | `~/.cache/icloud-photos/{thumbs,medium}/` |
| preferences | `~/.config/icloud-photos/settings.json` |

Deleting the cache or the catalog is safe: the next sync fetches them again.
Originals already in the folder are not downloaded a second time: a file
with the photo's name and exactly the size iCloud reports is taken to be
its download and recorded again. A file of that name with any other size is
left alone, and the download goes beside it as `IMG_0001 (2).HEIC`.
A download that was cut off (the app quit, the machine went down) can leave
a hidden `.icloud-photos-….part` file in a `<year>/<month>` folder; these
are removed, a day old, the next time downloads start.

**Upgrading**: originals used to go to
`~/Pictures/icloud-photos/`. The first start after the update (app or
command line) moves what is there up into `~/Pictures/`, by renaming only
(nothing is copied or replaced), and updates the catalog to match. Where a
folder already exists in `~/Pictures` (a year folder, say) the two are
merged; a file whose name is already taken stays in
`~/Pictures/icloud-photos/` and is listed on stderr. The old folder is
removed once empty. A library folder you chose yourself in Preferences is
left where it is. After that first start, `~/Pictures/icloud-photos/` is a
folder like any other and can be chosen again.

## What has and has not been checked against Apple

Everything here talks to Apple's private web endpoints, which are not
documented. The request and response shapes come from pyicloud
(picklepete/pyicloud and timlaing/pyicloud), not from captures made for this
app; `tests/fixtures/README.md` says where each fixture came from. Unless an
item says it was verified live, it has not been run against a real account:

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
  `<asset>-IN-<album>`, the shape the album listing returns. Verified live:
  on 2026-09-30 an upload with `--album` landed in that album on a real
  account. (No capture of icloud.com doing it has been made.)
- **Upload**: the four-step `photosupload` flow (`createUploadUrl`, bytes
  to the reserved content URL, `putAsset`, `uploadStatus`) ported from
  timlaing/pyicloud, whose fixtures say they matched a live account. The
  older single-request `uploadimagews` endpoint used by the icloudpd fork
  has answered 410 Gone since 2026-08-25 and is not used. Verified live on
  2026-09-30 (`icloud-photos upload --album`).

## Limitations

- Shared albums, Shared Library, Hidden and Recently Deleted are not shown.
  Hidden photos are left out of All Photos, as on icloud.com.
- Folders of albums are flattened: their albums appear in the list, the
  folders themselves do not.
- No editing, favourites, album management, or moving photos between albums
  (the command line can only add new uploads to an existing album).
- HEIC originals download fine but show in the viewer through iCloud's JPEG
  preview.

## Development

Run the app without an Apple account against the fake CloudKit server,
which serves 120 generated photos in three albums and keeps a change log so
deletes and uploads come back through incremental sync:

```bash
cargo run --example fake_cloudkit -- --port 8765            # add --signed-out to test the banner
ICLOUD_SESSION_MOCK=1 ICLOUD_SESSION_MOCK_URL=http://127.0.0.1:8765 cargo run --bin icloud-photos-app
```

The command line works the same way, and `--data-dir` keeps it away from
your own files:

```bash
export ICLOUD_SESSION_MOCK=1 ICLOUD_SESSION_MOCK_URL=http://127.0.0.1:8765
cargo run -- --data-dir /tmp/photos-dev sync
cargo run -- --data-dir /tmp/photos-dev list --limit 3 --quiet
```

`ICLOUD_SESSION_MOCK=1` is icloud-session's mock mode: no D-Bus, and every
web service resolves to `ICLOUD_SESSION_MOCK_URL`. The banner's Sign In
(and `icloud-photos sign-in`) posts to the fake's `/mock/reauthenticate`. Point `HOME` (or the `XDG_*` variables)
somewhere else to keep the dev catalog and downloads out of your own.

Layout:

- `src/cloudkit.rs` CloudKit requests and record parsing
- `src/sync.rs` full listing and incremental `changes/zone`
- `src/catalog.rs` the SQLite catalog
- `src/thumbs.rs` downloads (thumbnails, previews, originals) on a small pool
- `src/upload.rs` upload (verified live, see above)
- `src/transport.rs` the HTTP seam every module above goes through
- `src/session.rs` the only file that uses the `icloud-session` client
- `src/ui/` the GTK 4 / libadwaita app, started by `src/app.rs` (`icloud-photos-app`)
- `src/cli.rs` the command line (no GTK), over the same library code, started
  by `src/main.rs` (`icloud-photos`, which runs `icloud-photos-app` when given
  no arguments; `ICLOUD_PHOTOS_APP=path` points it at another build)

`cargo test -p icloud-photos` runs the test suite (fixtures in `tests/fixtures/`, plus
an end-to-end run against the fake server, and `tests/cli.rs`, which runs
the built binary against it for every command); the root `bin/check` runs
it with clippy and every other crate's tests.
In debug builds, `ICLOUD_PHOTOS_SCREENSHOT=out.png` (optionally with
`ICLOUD_PHOTOS_SCREENSHOT_VIEW=viewer` or `prefs`) renders the window to a
PNG after the first sync and quits, so the UI can be checked headless with
`GDK_BACKEND=broadway`.

## Releasing

Released with the other packages from the repository root; see the root
[README](../README.md#releasing). Releases are tagged `v<version>`; the older
`photos-v<version>` tags are historical.
