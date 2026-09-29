# iCloud apps for Omarchy

Four repos, one Apple sign-in, no Python. Each app repo has the same shape as
`icloud-notes` (README, `bin/build`, `bin/release`, `bin/verify-release`,
`install.sh`, `pkgbuild/`, signed package repo, `omarchy pkg` install).

| repo | what | stack | status |
|---|---|---|---|
| `icloud-notes` | Apple Notes | Qt/QML, syncs via `icloud-md` | shipped |
| `icloud-session` | account owner: daemon, sign-in window, client crate, CLI | Rust + WebKitGTK 6 + zbus | new |
| `icloud-findmy` | Find My devices | Rust + GTK4/libadwaita + libshumate | new |
| `icloud-photos` | iCloud Photos, two-way | Rust + GTK4/libadwaita | new |

Dependency line: `icloud-session` (Rust: D-Bus daemon + WebKitGTK sign-in +
client crate) owns the Apple account → findmy and photos link the client crate;
Notes talks to the daemon over D-Bus and syncs through `icloud-md` (npm, third
party), which the daemon feeds with the session. (Since 2026-09-29 Notes syncs
through `icloud-notes-sync`, the Rust port, which gets the session over D-Bus
like the other apps; see "icloud-md was a consumer" below.)

## What changed from the first brief

- **No Python anywhere.** No sidecars, no venvs, no pip, no JSON-lines protocol,
  no `sidecar.rs`. Apps call Apple's endpoints directly through the
  `icloud-session` crate. pyicloud and the icloudpd fork are read as the spec
  for the endpoints, not run.
- **Find My before Photos.** Four JSON endpoints make it the smallest real
  consumer of the crate, so it proves the session code before the big port.
- **Photos split into read and write.** Browse/download ships first; upload and
  delete (the part only the icloudpd fork knows) come after.
- **Validate once for everyone.** A shared validate cache (below) means three
  apps plus icloud-md do not each rotate the token on their own schedule. This
  is the main fix for the rotation race, before any lock matters.
- **Thumbnails from Apple, not generated.** CloudKit asset records carry
  `resJPEGThumbRes` / `resJPEGMedRes` derivatives; fetch those instead of
  decoding originals. No image-processing crate, no originals needed to browse.
- **Incremental Photos sync via CloudKit change tokens** (`changes/zone` on
  PrimarySync) instead of re-querying `assets(since)`. Needs a check that the
  web endpoint accepts it for PrimarySync; fall back to the query if not.
- **Runtime deps shrink to gtk4, libadwaita (+ libshumate), webkitgtk-6.0**
  (through icloud-session). Only Notes needs nodejs/npm, for icloud-md. Rust is
  `makedepends` only; users install prebuilt binaries.
- **AirTag items (FindMy.py) are dropped from scope**, not deferred into a
  Python sidecar. Revisit only if a Rust implementation exists or you decide a
  Python dependency is worth it for that one feature.

## Sign-in and session: icloud-session owns the account

Redesigned 2026-09-28 from scratch. Sign-in no longer belongs to a Notes sync
tool that every other app borrows. `icloud-session` owns the Apple account;
Notes (through icloud-md), Find My and Photos all consume it. No support for
the earlier icloud-md-owned layout: after the switch you sign in once more.

### Parts (one repo, Cargo workspace)

| crate | binary | role |
|---|---|---|
| `icloud-session` | – | client library the apps link |
| `icloud-sessiond` | `icloud-sessiond` | D-Bus user service, the only owner of the session |
| `icloud-sessiond` | `icloud-session-signin` | WebKitGTK sign-in window the daemon spawns |
| `icloud-sessiond` | `icloud-session` | CLI for scripts and debugging |

### The daemon

`icloud-sessiond` is D-Bus activated (`/usr/share/dbus-1/services/
io.github.ferdousbhai.ICloudSession.service`) and exits after 5 minutes with
no clients and no sign-in in progress. It is the only process that holds the
cookie jar, calls `setup.icloud.com/setup/ws/1/validate`, and writes session
state. Because there is one owner, there are no file locks, no validate cache
file, no sign-in marker file, no truncated-read retries, and no status polling.

- State: `$XDG_STATE_HOME/icloud-session/account.json` (0600): apple_id,
  dsid, cookies with per-cookie expiry, client params (clientId,
  clientBuildNumber, clientMasteringNumber), webservices, validated_at.
  One account. Written atomically by the daemon only.
- Heartbeat: `/validate` on start, then every 10 minutes while any client
  has called in the last 15 minutes (the browser's own heartbeat is 14).
  Rotated cookies merge by name.
- Expiry comes from the X-APPLE-WEBAUTH-TOKEN cookie's own expiry as
  captured, not from a browser cookie database.
- 421/401 on `/validate` → signed out. A client's `ReportSignInRequired`
  triggers a `/validate` to confirm before flipping, so one stray 401 on a
  data endpoint does not sign every app out.

D-Bus API, bus name and interface `io.github.ferdousbhai.ICloudSession`,
object `/io/github/ferdousbhai/ICloudSession`, session bus:

| member | kind | signature / notes |
|---|---|---|
| `SignedIn` | property | `b` |
| `AppleId` | property | `s`, empty when signed out |
| `Dsid` | property | `s`, empty when signed out |
| `ExpiresAt` | property | `t` unix seconds, 0 = session-only or unknown |
| `SigningIn` | property | `b`, the sign-in window is open (also for `AuthorizeFindMy()`) |
| `FindMyAuthorized` | property | `b`, a Find My jar is held and has `X-APPLE-WEBAUTH-FMIP` |
| `Session()` | method | `→ (s cookie_header, a{ss} client_params, a{ss} webservices)`; error `io.github.ferdousbhai.ICloudSession.Error.SignInRequired` when signed out; revalidates first if the last validate is older than 10 minutes |
| `MergeCookies(as)` | method | raw `Set-Cookie` header values a client received |
| `ReportSignInRequired()` | method | `→ b still_signed_in`: a client got 421/401; the daemon validates, and on success keeps the fresh jar and returns true so the caller fetches `Session()` and retries once; on 421/401 it signs out and returns false |
| `SignIn()` | method | opens the sign-in window if not already open; returns at once, the outcome arrives as property changes |
| `SignOut()` | method | forgets the account and the WebKit profile |
| `AuthorizeFindMy()` | method | opens the sign-in window with `--find` on `www.icloud.com/find`, where Apple asks for the password before Find My stops answering 450; returns at once like `SignIn()`. That is a one-factor sign-in `/validate` refuses, so the captured jar (session-only cookies incl. `X-APPLE-WEBAUTH-FMIP`, plus client params) is kept unvalidated as a separate Find My jar in account.json; main jar and generation untouched; dsid checked only if the window reports it (or X-APPLE-WEBAUTH-USER names it) |
| `FindMySession()` | method | `→ (s cookie_header, a{ss} client_params)` of the Find My jar; error `…Error.FindMyAuthRequired` when there is none |
| `MergeFindMyCookies(as)` | method | `Set-Cookie`s a client got from the `findme` host, merged into the Find My jar |
| `ReportFindMyAuthRequired()` | method | `→ b reauthorized`: a client got HTTP 450 from Find My: the Find My jar is deleted (SignOut deletes it too); with a stored password the daemon signs in to Find My again and answers true (client retries once) |
| `FindMyPasswordStored` | property | `b`, the Apple ID password is in the keyring (opt-in) |
| `SetPassword(s)` / `ForgetPassword()` | method | verify with a Find My sign-in, then store in / remove from the Secret Service |

Property changes are announced with the standard `PropertiesChanged` signal,
so Qt (QtDBus) and GTK apps watch one signal for their sign-in banner.

### Sign-in window

`icloud-session-signin` is a small GTK4 + WebKitGTK 6 (`webkit6` crate) window
loading `https://www.icloud.com/`, Apple's real sign-in, so every 2FA flavour
and "Trust this browser" work and Apple's UI changes are not ours to track.
Its WebKit NetworkSession keeps a persistent data dir,
`$XDG_DATA_HOME/icloud-session/webkit/`, so a later sign-in usually needs no
2FA. When the page has signed in (X-APPLE-WEBAUTH-TOKEN present in the
cookie manager for `.icloud.com`), it reads the icloud.com cookies, prints one
JSON object to stdout (cookies with expiry, plus the client params captured
from the page's `/validate` request or a fresh clientId), and exits; the
daemon validates and stores it. Closing the window exits non-zero. It sets a
Safari user agent if Apple rejects WebKitGTK's default. First milestone: sign in
with a real account (spike before the rest depends on it).

### Client crate

```rust
let s = Session::connect()?;           // D-Bus; SignInRequired when signed out
let ws = s.webservices()?;             // from Session(); cached in-process ≤ 60 s
s.get(url)?; s.post_json(url, &v)?; s.post_bytes(url, ct, body)?; s.download(url, dest)?;
s.apple_id(); s.dsid();
icloud_session::sign_in()?;            // SignIn(), returns at once
icloud_session::authorize_find_my()?;  // AuthorizeFindMy(), returns at once
icloud_session::status()?;             // Status { signed_in, apple_id, dsid, expires_at, signing_in, find_my_authorized }
icloud_session::watch()?;              // blocking iterator of Status, one per change (run on a thread)
```

Requests go straight to Apple with the cookie header from `Session()` (no
downloads through D-Bus). Any `Set-Cookie` goes back through `MergeCookies`;
421/401 → `ReportSignInRequired` and `Error::SignInRequired`. Requests to
the `findme` host use `FindMySession()` instead of `Session()` and
`MergeFindMyCookies`; no Find My jar, or a 450 from it →
`ReportFindMyAuthRequired` and `Error::FindMyAuthRequired`, retried once
only if the Find My jar changed meanwhile or the report answered true.

Automatic Find My re-authorization (opt-in, 2026-09-28): `icloud-session
set-password [--from-bitwarden|--from-1password [ITEM]]` stores the Apple
ID password in GNOME Keyring (Secret Service, `application=icloud-session`,
`apple-id=…`) after one verifying sign-in (stored unless Apple refuses it;
another failure is reported but the password kept). Without ITEM the
manager options pick the login saved for apple.com/icloud.com (preferring
the Apple ID's; asks on a TTY if several). On a 450, or `FindMySession()`
with no jar, the daemon runs `icloud-session-signin --find --autofill`
hidden, writes "<apple id>\n<password>\n" to its stdin, and the window
fills Apple's own sign-in form like a password manager (a plain
`accountLogin` POST now answers 421); it shows itself after 40 s if Apple
wants 2FA. Its stdout capture (FMIP jar, dsid, client params) becomes the
Find My jar; the main jar is never touched. Exit 3 = password refused (or
a jar Find My refuses at once) stops it until the password changes; any
other failure → 60 s back-off; then the manual `AuthorizeFindMy()` path.
`ICLOUD_SESSION_SIGNIN_TRACE=1` traces the page's form (never values). Trade-off: any process running as the
user can read unlocked keyring items.
`ICLOUD_SESSION_MOCK=1`: no D-Bus, fake signed-in status, every request sent
to `$ICLOUD_SESSION_MOCK_URL` (default `http://127.0.0.1:8765`) keeping path
and query.

CLI: `icloud-session status | sign-in | authorize-find-my | set-password | forget-password | sign-out | validate`, JSON on stdout.

### icloud-md was a consumer (retired 2026-09-29)

Retired on 2026-09-29, when Notes moved to icloud-notes-sync, which gets the
session from the daemon over D-Bus. The daemon (icloud-sessiond 0.2.1 on) no
longer writes, watches or deletes anything under `~/.config/icloud-md`, and
`ReportSignInRequired()` only confirms with `/validate`. The design as it was:

icloud-md can already sync from a stored session alone: `icloud-md clone
--account <dsid>` reuses `accounts/<dsid>/session.local.json` plus
`meta.json` without a browser (`bindKnownAccount` → `reuseStoredSession`).
So the daemon mirrors the session there:

- After every sign-in and rotation it writes
  `~/.config/icloud-md/accounts/<dsid>/{session.local.json,meta.json}`
  atomically.
- It watches that file (inotify). When icloud-md writes a cookie jar the
  daemon did not write (its own `/validate` rotation), the daemon adopts it.
- icloud-md's own browser re-login never runs (there is no icloud-md browser
  profile); on 421 it fails, Notes reports it, and the user signs in through
  `SignIn()`.
- Upstream PR #31 (atomic write) makes the mirror safe to read; the lock
  Discussion is no longer needed.

## Notes

- First run: `SignIn()`, wait for `SignedIn`, then `icloud-md clone
  --account <Dsid> <vault>`.
- Banner and days-left from the D-Bus properties via QtDBus
  (`PropertiesChanged`); no `icloud-session status` process, no
  `.icloud-notes-signin-expired` flag file, no session-file watching.
- An icloud-md failure that says to reauthenticate → `ReportSignInRequired()`: true → retry the sync once (the mirror was refreshed; since 2026-09-29 icloud-notes-sync fetches `Session()` again), false → paused;
  the "Sign in" button → `SignIn()`. Notes never runs `icloud-md
  reauthenticate`.
- PKGBUILD depends on `icloud-session`; `webkitgtk-6.0` comes with it.


## icloud-findmy

- Endpoints on the `findme` web service, as in pyicloud's
  `FindMyiPhoneServiceManager`: `initClient`, `refreshClient`, `playSound`,
  `lostDevice`. `src/findme.rs`, direct calls through the crate.
- `~/.local/share/icloud-findmy/history.db`: `(device_id, ts, lat, lon,
  accuracy, battery)`, one row per refresh when the position moved.
- UI: libshumate map with OSM tiles, device list with battery and last seen,
  click to center, history trail, actions popover (play sound, lost mode),
  sign-in banner. A Find My 450 (`Error::FindMyAuthRequired`) shows its own
  banner state, "Find My needs your Apple password", whose button calls
  `authorize_find_my()`; no 450 retry loop.

```
icloud-findmy/
  README.md  install.sh  bin/{build,release,verify-release}  pkgbuild/PKGBUILD
  src/  main.rs findme.rs history.rs ui/{window,banner,map,devices}.rs
  data/ icloud-findmy.desktop, icon
  tests/ fixtures/ (recorded initClient/refreshClient JSON)
```

## icloud-photos

Read first, then write.

- Apple side, `src/cloudkit.rs`: `records/query` on the private db, zone
  PrimarySync (`CPLAlbumByPositionLive`, `CPLAssetAndMasterByAssetDateWithoutHiddenOrDeleted`
  and friends, as pyicloud uses), asset download via the signed `downloadURL`
  on each resource, Live Photo pairs (`resOriginalVidComplRes`), derivatives
  for thumbs. Incremental via zone change token stored in `meta`.
- Write side, `src/upload.rs`: port icloudpd fork's `upload_file` and delete
  (move to Recently Deleted = set `isDeleted` on the asset record). Record real
  request/response fixtures from icloud.com in the browser before porting.
- Local: `~/.local/share/icloud-photos/catalog.db`, originals on demand in
  `~/Pictures/iCloud/`, thumbs `~/.cache/icloud-photos/thumbs/`.

```sql
assets(id TEXT PK, filename, created INT, size INT, w INT, h INT, kind TEXT,
       is_live INT, local_path TEXT NULL, thumb_path TEXT NULL, deleted INT DEFAULT 0)
albums(id TEXT PK, name)
album_assets(album_id, asset_id)
meta(key TEXT PK, value)   -- includes zone change token
```

UI: AdwNavigationSplitView, albums sidebar, GtkGridView with month headers,
viewer (prev/next, download, delete, open), upload chooser with progress,
sign-in banner, preferences (library dir, download originals: on demand/all).

```
icloud-photos/
  README.md  install.sh  bin/{build,release,verify-release}  pkgbuild/PKGBUILD
  src/  main.rs cloudkit.rs upload.rs catalog.rs sync.rs thumbs.rs
        ui/{window,banner,grid,albums,viewer,upload}.rs
  data/ icloud-photos.desktop, icon
  tests/ fixtures/
```

Small shared bits (sign-in banner, status polling) are copied between the two
apps. They become a crate only if a third GTK app appears.

## Packaging

- One signed pacman repo per app, served from that repo's GitHub releases,
  as `[icloud-notes]` is today: `[icloud-session]`, `[icloud-findmy]`,
  `[icloud-photos]`, same key. Each app's install.sh adds `[icloud-session]`
  too. The one-liners are `ferdousbhai.com/<app>/install.sh`, 302s in the
  site's `public/_redirects` to the latest release asset.
- PKGBUILDs: `makedepends=(rust cargo git)`; `depends=(gtk4 libadwaita icloud-session)`,
  findmy adds `libshumate`; icloud-session depends on `webkitgtk-6.0 dbus`.
  Only Notes' install.sh installs icloud-md (npm).
- `install.sh` per app: shared `add_signed_repo`, byte-identical and pinned by
  `tests/add_signed_repo.sha256`, then `pacman -S <app>`.
- `bin/release` and `bin/verify-release` copied from icloud-notes, app name
  parameterised; icloud-session's release also runs `cargo publish`.

## Order of work

### 0. icloud-session
- [ ] sign-in window spike: real account signs in through WebKitGTK, cookies captured, `/validate` accepts them
- [ ] daemon: state file, D-Bus API, heartbeat, confirm-before-sign-out, idle exit, activation file
- [x] ~~icloud-md mirror + inotify adoption~~ (built, then retired 2026-09-29)
- [ ] client crate on D-Bus, mock mode, CLI
- [ ] tests against a private dbus-daemon and a fake Apple server
- [x] upstream PR to icloud-md: atomic write (#31)

### 0b. icloud-notes
- [ ] QtDBus properties for banner/expiry, `SignIn()`, `ReportSignInRequired()`, first-run clone `--account`
- [ ] remove the `icloud-session status` process, flag file, `icloud-md reauthenticate`

### 1. icloud-findmy core (dev container)
- [ ] findme.rs against fixtures, history.rs, tests

### 2. icloud-photos read (dev container)
- [ ] capture real CloudKit fixtures
- [ ] cloudkit.rs query + change token, catalog.rs, sync.rs, thumbs.rs, download incl. Live pairs

### 3. GTK UIs (on Omarchy)
- [ ] findmy: map, devices, actions, history trail, banner
- [ ] photos: window, grid, albums, viewer, banner, prefs

### 4. icloud-photos write
- [ ] capture upload/delete fixtures, upload.rs, delete, upload UI with progress

### 5. Packaging
- [ ] PKGBUILDs, .desktop, install.sh, release scripts for both apps

### 6. Verify on Omarchy
- [ ] Sign in once (from any app), open the others with no prompt
- [ ] All three apps open for a day, no logouts from rotation (check `validated_at` cadence)
- [ ] Photos: sync a few thousand assets, upload, delete to Recently Deleted
- [ ] Find My: locate a phone, play sound, history after a walk
- [ ] Expire the session, banner in all apps at once, sign in once, all resume
- [ ] Clean Arch container: both one liners install and launch

## Risks

- Apple may refuse sign-in in WebKitGTK. The spike settles it first; a Safari
  user agent is the first fix. If it still fails, fall back to native SRP login
  (as icloudpd does) behind the same `SignIn()`.
- ~~icloud-md rotates the mirrored session on its own `/validate`.~~ Gone
  with the mirror (2026-09-29): every client now rotates through the daemon.
- Photos write is the least documented part. Only the icloudpd fork shows it
  working; capture fixtures from icloud.com first, and ship read-only if it
  stalls.
- Unofficial endpoints break on Apple changes, and there is no upstream Python
  library to wait for a fix from. Fixture tests show what changed; pin
  `clientBuildNumber` from the session file, as icloud-md does.
- Advanced Data Protection off and web access on, same as Notes.
- Apple ends sessions without warning; every app must survive `SignInRequired`.

## Next step

Phase 0: the sign-in window spike with a real account, then the daemon and
client crate. Find My and Photos move to `Session::connect()` / `sign_in()`
once the client crate lands.
