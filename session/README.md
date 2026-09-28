# icloud-session

One iCloud web sign-in for every app on the machine. A small D-Bus user
service owns the Apple account; Notes (through icloud-md), Find My and
Photos use it instead of signing in themselves.

| crate | binary | role |
|---|---|---|
| `icloud-session` (repo root) | – | client library the apps link |
| `icloud-sessiond` (`sessiond/`) | `icloud-sessiond` | D-Bus user service, the only owner of the session |
| | `icloud-session-signin` | GTK4 + WebKitGTK 6 sign-in window the daemon runs |
| | `icloud-session` | CLI for scripts and debugging |

Requirements: an Apple ID with Advanced Data Protection off and "Access
iCloud Data on the Web" on. Runtime: `gtk4`, `webkitgtk-6.0`, a D-Bus
session bus. No systemd unit, no Python, no Node.

## Design

`icloud-sessiond` is D-Bus activated
(`/usr/share/dbus-1/services/io.github.ferdousbhai.ICloudSession.service`)
and exits after 5 minutes with no connected clients and no sign-in in
progress. It is the only process that holds the cookie jar, calls
`setup.icloud.com/setup/ws/1/validate`, and writes session state. With one
owner there are no file locks, no validate cache file, no sign-in marker,
no truncated-read retries and no status polling.

- **Sign-in.** `SignIn()` runs `icloud-session-signin`, a window showing
  `https://www.icloud.com/`: Apple's own page, so every 2FA flavour and
  "Trust this browser" work. Its WebKit profile persists, so a later
  sign-in usually needs no 2FA. When the page's own `/accountLogin` or
  `/validate` call answers with `dsInfo` and no pending
  `hsaChallengeRequired` (icloud-md's rule), the window prints the
  icloud.com cookies (with expiry) and the client params from that
  request as JSON and exits. The daemon validates the capture and stores it.
- **Heartbeat.** `/validate` once on start, then whenever the last one is
  older than 10 minutes and a client called within the last 15 (the
  browser's own heartbeat is 14). `Session()` also revalidates first when
  the last validate is older than 10 minutes. Rotated cookies merge by name.
  If Apple cannot be reached (10 s to connect, 20 s in all), callers get
  the session as it is, and `/validate` is not tried again for a minute;
  callers that queued behind a failed attempt share its answer.
- **Expiry** is the captured X-APPLE-WEBAUTH-TOKEN cookie's own expiry,
  updated when Apple rotates it.
- **Signing out.** 421/401 on `/validate` signs out. A client's 421/401
  on a data endpoint only triggers `ReportSignInRequired()`, which runs a
  `/validate` to confirm first, so one stray 401 does not sign every app out.
  Reports that arrive while a `/validate` is running share its answer.
- **icloud-md mirror.** After every sign-in and rotation the daemon writes
  `~/.config/icloud-md/accounts/<dsid>/{session.local.json,meta.json}`
  atomically, so `icloud-md clone --account <dsid>` syncs Notes with no
  browser of its own. It watches that file (inotify); when icloud-md writes
  a jar the daemon did not write (its own `/validate` rotation), the daemon
  adopts it, also on start for writes made while it was not running.

## Library

```toml
[dependencies]
icloud-session = "=0.2.0"
```

```rust
use icloud_session::{Error, Session};

let s = Session::connect()?;           // D-Bus; SignInRequired when signed out
let ws = s.webservices()?;             // from Session(); cached in-process ≤ 60 s
let findme = ws.url("findme").unwrap();
let r = s.post_json(&format!("{findme}/fmipservice/client/web/refreshClient"), &body)?;
let devices: serde_json::Value = r.json()?;

match s.get(url) {
    Err(Error::SignInRequired) => { /* banner whose button calls icloud_session::sign_in() */ }
    other => { /* ... */ }
}

icloud_session::sign_in()?;            // SignIn(), returns at once
icloud_session::status()?;             // Status { signed_in, apple_id, dsid, expires_at, signing_in }
for status in icloud_session::watch()? { /* on its own thread: one Status per change */ }
```

| item | what it does |
|---|---|
| `Session::connect()` | Reads the daemon's properties (D-Bus activates it) and fetches `Session()`. `SignInRequired` when signed out. |
| `s.webservices()` | The `webservices` map (`ckdatabasews`, `findme`, ...) from the daemon's last `/validate`. |
| `s.get(url)`, `post_json(url, &value)`, `post_bytes(url, content_type, bytes)`, `post_file(url, content_type, &path)` | Straight to Apple with the cookie header from `Session()`, `Origin`/`Referer: https://www.icloud.com`, and `clientBuildNumber`, `clientMasteringNumber`, `clientId`, `dsid` appended to the query (a parameter already in the URL is left alone). `post_file` streams the file with its `Content-Length` instead of reading it into memory. |
| `s.download(url, dest)` | Streams to a temp file beside `dest` (parent directories created), renamed on success. Cookies attached, no client params. |
| `s.apple_id()`, `s.dsid()` | The account the session belongs to. If the daemon later holds another account, the session's calls return `SignInRequired`; connect again. |
| `sign_in()`, `sign_out()` | `SignIn()` / `SignOut()`; both return at once. |
| `status()` | `Status { signed_in, apple_id, dsid, expires_at, signing_in }`, one `GetAll`. `expires_at` is unix seconds or `None`. |
| `watch()` | Blocking iterator yielding the new `Status` after each `PropertiesChanged`, and after the daemon dies or restarts (re-read from the new instance). |
| `Session::connect_on(&conn)`, `status_on`, `watch_on`, `sign_in_on`, `sign_out_on` | The same on a given `zbus::blocking::Connection` (tests, tools). |
| `Session::mock(base_url)` | What mock mode gives `Session::connect()`. |

Every `Set-Cookie` Apple sends a request goes back to the daemon through
`MergeCookies`, and the in-process `Session()` cache is dropped so the next
request uses the merged jar. On 421/401 from icloud.com or a service host
(a content host's 401 is a plain `Http` error) the client calls
`ReportSignInRequired()`: if the daemon is still signed in it retries the
request once with the fresh jar (a 421/401 again is `Http`), otherwise it
returns `SignInRequired`.

Errors: `SignInRequired`, `Http { status, body }` (any other non-2xx),
`Network`, `Service` (the daemon could not be reached or failed), `Io`.

Every call is blocking: ureq for HTTP, zbus's blocking API with its own
small executor thread for D-Bus, no tokio. GTK apps run calls on
`gio::spawn_blocking` or a thread pool. `Session` is cheap to clone and
safe to share between threads.

## D-Bus interface

Session bus, bus name and interface `io.github.ferdousbhai.ICloudSession`,
object `/io/github/ferdousbhai/ICloudSession`.

| member | kind | signature / notes |
|---|---|---|
| `SignedIn` | property | `b` |
| `AppleId` | property | `s`, empty when signed out |
| `Dsid` | property | `s`, empty when signed out |
| `ExpiresAt` | property | `t` unix seconds, 0 = session-only or unknown |
| `SigningIn` | property | `b`, the sign-in window is open |
| `Session()` | method | `→ (s cookie_header, a{ss} client_params, a{ss} webservices)`; error `io.github.ferdousbhai.ICloudSession.Error.SignInRequired` when signed out; revalidates first if the last validate is older than 10 minutes (if Apple is unreachable it answers with what it has) |
| `MergeCookies(as)` | method | raw `Set-Cookie` header values a client received |
| `ReportSignInRequired()` | method | `→ b still_signed_in`. A client got 421/401. The daemon runs `/validate`: on 2xx it rewrites the icloud-md mirror with the fresh jar and answers true (retry once); on 421/401 it signs out and answers false |
| `SignIn()` | method | opens the sign-in window unless it is open; returns at once, the outcome arrives as property changes |
| `SignOut()` | method | forgets the account, the WebKit profile and the mirrored `session.local.json` |

Property changes are announced with the standard
`org.freedesktop.DBus.Properties.PropertiesChanged` signal, one signal per
state change carrying every property that changed, so Qt (QtDBus) and GTK
apps watch one signal for their sign-in banner. A newly started daemon
announces every property once.

```console
$ busctl --user introspect io.github.ferdousbhai.ICloudSession /io/github/ferdousbhai/ICloudSession
$ gdbus call --session -d io.github.ferdousbhai.ICloudSession -o /io/github/ferdousbhai/ICloudSession \
    -m io.github.ferdousbhai.ICloudSession.SignIn
```

## Command line

JSON on stdout, errors on stderr.

```console
$ icloud-session status
{"signed_in":true,"apple_id":"you@example.com","dsid":"1234567890","expires_at":1793000000,"signing_in":false}
$ icloud-session sign-in     # opens the window, waits for it to close, prints status
$ icloud-session validate    # {"dsid":…,"apple_id":…,"webservices":{…}}
$ icloud-session sign-out
```

Exit codes: 0 ok, 1 error, 2 sign-in required (or sign-in not completed), 64 usage.

## Files

| path | written by | contents |
|---|---|---|
| `$XDG_STATE_HOME/icloud-session/account.json` (0600) | daemon | `apple_id`, `dsid`, `cookies` (name, value, domain, path, expires), `client_params` (clientId, clientBuildNumber, clientMasteringNumber), `webservices`, `validated_at`, `captured_at`. One that cannot be read is moved to `account.json.bad` and the daemon starts signed out. |
| `$XDG_DATA_HOME/icloud-session/webkit/` | sign-in window | its WebKit profile (cookies.sqlite, storage): device trust for later sign-ins |
| `$XDG_CACHE_HOME/icloud-session/webkit/` | sign-in window | WebKit cache |
| `~/.config/icloud-md/accounts/<dsid>/session.local.json` (0600) | daemon, icloud-md | `cookie`, `clientId`, `clientBuildNumber`, `clientMasteringNumber`, `capturedAt` (fields icloud-md adds are kept) |
| `~/.config/icloud-md/accounts/<dsid>/meta.json` (0600) | daemon | `appleId`, `dsid` |

`$XDG_STATE_HOME` defaults to `~/.local/state`, `$XDG_DATA_HOME` to
`~/.local/share`, `$XDG_CACHE_HOME` to `~/.cache`. The icloud-md path
follows icloud-md itself (`os.homedir()/.config/icloud-md`). Every daemon
write is atomic: temp file in the same directory, mode 0600, fsync, rename.
One account at a time; signing in with another Apple ID replaces it.
The earlier layout, where this crate read icloud-md's accounts directory
as its source of truth, is gone: after upgrading, sign in once more.

## Mock mode

`ICLOUD_SESSION_MOCK=1` makes the client library use no D-Bus at all: a
signed-in fake session (dsid `mock`, Apple ID `mock@example.com`) whose
every `webservices` URL is `ICLOUD_SESSION_MOCK_URL` (default
`http://127.0.0.1:8765`). A request to any other host is sent there with its
path and query unchanged, so an app's mock server can serve fixtures for
CloudKit, Find My and download URLs alike. `sign_in`/`sign_out` do nothing
and `watch` never yields.

## Environment

| variable | used by | default |
|---|---|---|
| `ICLOUD_SESSION_MOCK`, `ICLOUD_SESSION_MOCK_URL` | client library | off, `http://127.0.0.1:8765` |
| `ICLOUD_SESSION_SIGNIN_BIN` | daemon | `icloud-session-signin` beside `icloud-sessiond`, else on `PATH` |
| `ICLOUD_SESSION_SIGNIN_UA` | sign-in window | WebKitGTK's own user agent; `safari` for a macOS Safari one, anything else verbatim |
| `ICLOUD_SESSION_SETUP_URL` | daemon (tests) | `https://setup.icloud.com` |
| `ICLOUD_SESSIOND_IDLE_SECS`, `ICLOUD_SESSIOND_VALIDATE_SECS`, `ICLOUD_SESSIOND_RETRY_SECS` | daemon (tests) | 300, 600, 60 |

## The sign-in spike

The one part the tests cannot cover is a real Apple sign-in through
WebKitGTK. Try it before relying on the rest:

```bash
cargo run -p icloud-sessiond --bin icloud-session-signin
```

Sign in (password, 2FA, "Trust this browser"). When the page has signed in
the window closes by itself and the captured session is printed as JSON:
cookies for `.icloud.com` including `X-APPLE-WEBAUTH-TOKEN` with an
`expires`, plus `clientId`, `clientBuildNumber`, `clientMasteringNumber`
(`null` when the page's setup call did not carry them; the daemon then uses
icloud-md's defaults and a fresh clientId). Progress goes to stderr. It uses
the real profile directory, `~/.local/share/icloud-session/webkit/`; point
`XDG_DATA_HOME`/`XDG_CACHE_HOME` elsewhere for a throwaway one. If Apple
refuses the browser, retry with `ICLOUD_SESSION_SIGNIN_UA=safari`.

To check that `/validate` accepts the capture, run the whole flow:
`icloud-session sign-in`, then `icloud-session validate` (installed
package, or the dev install below).

## Install

The iCloud apps' installers add the signed `[icloud-session]` pacman
repository (served from this repository's GitHub releases) and pull the
package in as a dependency. On its own:

```bash
curl -fsSL https://ferdousbhai.com/icloud-session/install.sh | sudo bash
```

or build the package from this checkout: `cd pkgbuild && makepkg -si`.
Either installs the three binaries and the D-Bus activation file. Nothing
to enable; the bus starts the daemon on first use. Updates arrive through
`omarchy update`.

For development, without the package:

```bash
bin/dev-install     # release build into ~/.local/bin, D-Bus file into ~/.local/share/dbus-1/services
bin/dev-uninstall   # removes both; an installed package takes over again
```

`bin/dev-install` writes
`~/.local/share/dbus-1/services/io.github.ferdousbhai.ICloudSession.service`
with `Exec=~/.local/bin/icloud-sessiond` (expanded). The user services
directory is searched before `/usr/share`, so the dev build wins over an
installed package until `bin/dev-uninstall`. Both reload the bus's
configuration; a daemon already running keeps its old binary until it
idles out (or `pkill -x icloud-sessiond`). Neither touches the signed-in
session.

## Development

```bash
bin/test                                 # workspace tests + the installer's shared-function hash
cargo test                               # whole workspace, sign-in window included
cargo clippy --all-targets -- -D warnings
cargo test --no-default-features         # without webkitgtk-6.0 installed
```

The sign-in window is the `signin` feature of `icloud-sessiond` (on by
default); everything else builds and tests without it. The daemon tests run
`icloud-sessiond` through D-Bus activation on a private `dbus-daemon`, with
HOME and the XDG directories in a temp dir, a local HTTP server standing in
for Apple, and a shell script standing in for the sign-in window
(`ICLOUD_SESSION_SIGNIN_BIN`). They never touch the real session bus,
`~/.config`, `~/.local` or Apple. Covered: `Session()` and revalidation,
`MergeCookies`, `ReportSignInRequired` both ways, sign-in (captured,
closed, without params), sign-out, the icloud-md mirror and adoption of
icloud-md's writes (live and on start), the client library against the
daemon (requests, rotation, retry, sign-out), the CLI, idle exit, a second
daemon refusing to start, and mock mode without D-Bus.

## Releasing

`bin/release <major.minor.patch>` runs `bin/test` and a `cargo publish
--dry-run`, sets the version in both crates, `pkgbuild/PKGBUILD` and the
dependency snippet above, commits and tags, builds the package with
`makepkg --sign`, makes a signed one-package repository with `repo-add
--sign`, and publishes it with the signing key and `install.sh` as the
tag's GitHub release. `bin/verify-release` then installs it in a clean Arch
container through the public one-liner and rolls the release back if that
fails. Only after that does it `cargo publish` the client crate. It needs
the package-signing key pinned in `install.sh`, `gh`, docker, and a
crates.io token.

## License

MIT, see [LICENSE](LICENSE).
