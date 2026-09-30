# icloud-session

One iCloud web sign-in for every app on the machine. A small D-Bus user
service owns the Apple account; Notes (through icloud-notes-sync), Find My and
Photos use it instead of signing in themselves.

| crate | binary | role |
|---|---|---|
| `icloud-session` (`session/`) | – | client library the apps link |
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
  sign-in usually needs no 2FA. Once the jar holds a session token, the
  window makes its own `/validate` call from the page (the default build
  numbers and a clientId it generates); when that answers with `dsInfo`
  and no pending `hsaChallengeRequired` (icloud-md's rule), the window
  prints the icloud.com cookies (with expiry) and those client params as
  JSON and exits. The daemon validates the capture and stores it.
  `ICLOUD_SESSION_SIGNIN_TRACE=1` logs the page's form fields (never
  values) and requests to stderr.
- **Find My.** Apple's `findme` service answers HTTP 450 (pyicloud's
  `FIND_MY_REAUTH_REQUIRED`, empty body) to a normal signed-in session:
  icloud.com asks for the password again on `www.icloud.com/find` first.
  `AuthorizeFindMy()` runs `icloud-session-signin --find`, which loads that
  page, validates once in-page, then asks Find My's `initClient` every 5 s;
  once it answers 2xx the window prints the jar like a sign-in, plus the
  `dsid` its `/validate` named. With a stored password the daemon runs it
  hidden as `--find --autofill` instead (see [Automatic Find My
  re-authorization](#automatic-find-my-re-authorization)). That
  password step is a one-factor sign-in (pyicloud's
  `canLaunchWithOneFactor`): good for Find My, refused by `/validate`. So
  the daemon never validates it and leaves the main jar alone: it keeps
  it as a separate Find My jar in `account.json`
  (`find_my`: cookies, session-only ones such as `X-APPLE-WEBAUTH-FMIP`
  included, and client params), after checking the dsid the window
  reports (or the jar's X-APPLE-WEBAUTH-USER names), if any, is the
  account's. How long Apple honours it is unknown. Clients get it from
  `FindMySession()` for the `findme` host only. `FindMyAuthorized` is true
  while that jar exists and holds `X-APPLE-WEBAUTH-FMIP`; a client's 450
  (`ReportFindMyAuthRequired()`), a sign-out or another account forgets it.
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

## Library

```toml
[dependencies]
icloud-session = "=0.3.0"
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
    Err(Error::FindMyAuthRequired) => { /* banner whose button calls icloud_session::authorize_find_my() */ }
    other => { /* ... */ }
}

icloud_session::sign_in()?;            // SignIn(), returns at once
icloud_session::authorize_find_my()?;  // AuthorizeFindMy(), returns at once
icloud_session::status()?;             // Status { signed_in, apple_id, dsid, expires_at, signing_in, find_my_authorized, find_my_password_stored }
for status in icloud_session::watch()? { /* on its own thread: one Status per change */ }
icloud_session::watch_forever(|status| { /* ... */ true }); // the same, reconnecting; false stops it
```

| item | what it does |
|---|---|
| `Session::connect()` | Reads the daemon's properties (D-Bus activates it) and fetches `Session()`. `SignInRequired` when signed out. |
| `s.webservices()` | The `webservices` map (`ckdatabasews`, `findme`, ...) from the daemon's last `/validate`. |
| `s.get(url)`, `post_json(url, &value)`, `post_file(url, content_type, &path)` | Straight to Apple with the cookie header from `Session()`, `Origin`/`Referer: https://www.icloud.com`, and `clientBuildNumber`, `clientMasteringNumber`, `clientId`, `dsid` appended to the query (a parameter already in the URL is left alone). `post_file` streams the file with its `Content-Length` instead of reading it into memory. |
| `s.download(url, dest)` | Streams to a temp file beside `dest` (parent directories created), renamed on success. Cookies attached, no client params. |
| `s.apple_id()`, `s.dsid()` | The account the session belongs to. If the daemon later holds another account, the session's calls return `SignInRequired`; connect again. |
| `sign_in()`, `sign_out()` | `SignIn()` / `SignOut()`; both return at once. |
| `authorize_find_my()` | `AuthorizeFindMy()`, returns at once: `signing_in` while its window is open, then `find_my_authorized`. No-op in mock mode. |
| `status()` | `Status { signed_in, apple_id, dsid, expires_at, signing_in, find_my_authorized, find_my_password_stored }`, one `GetAll`. `expires_at` is unix seconds or `None`; `find_my_authorized` is false from a daemon without the property. Mock mode reports it true. |
| `watch()` | Blocking iterator yielding the new `Status` after each `PropertiesChanged`, and after the daemon dies or restarts (re-read from the new instance). |
| `watch_forever(f)` | What a sign-in banner wants: calls `f` with the current `Status` on every (re)connect and after each change, until `f` returns false. When the watch ends (the daemon idle-exited or restarted) or cannot start, it reconnects after 2 s, doubling up to 60 s; an outage is reported once on stderr. Returns at once in mock mode. |
| `Session::connect_on(&conn)`, `status_on`, `watch_on`, `sign_in_on`, `authorize_find_my_on`, `sign_out_on` | The same on a given `zbus::blocking::Connection` (tests, tools). |
| `Session::mock(base_url)` | What mock mode gives `Session::connect()`. |

Every `Set-Cookie` Apple sends a request goes back to the daemon through
`MergeCookies`, and the in-process `Session()` cache is dropped so the next
request uses the merged jar. On 421/401 from icloud.com or a service host
(a content host's 401 is a plain `Http` error) the client calls
`ReportSignInRequired()`: if the daemon is still signed in it retries the
request once with the fresh jar (a 421/401 again is `Http`), otherwise it
returns `SignInRequired`. Requests to the `findme` web service's host
carry the Find My jar from `FindMySession()` instead (none yet:
`FindMyAuthRequired`, nothing sent), with its client params, and its
`Set-Cookie`s go to `MergeFindMyCookies`. A 450 there (or 421/401) retries
once if the Find My jar changed since the request was sent (it was
authorized again meanwhile), or if `ReportFindMyAuthRequired()` answers
true (the daemon signed in again with the stored password); otherwise it
returns `FindMyAuthRequired`. One retry at most, so a 450 never loops.

Errors: `SignInRequired`, `FindMyAuthRequired`, `Http { status, body }` (any other non-2xx),
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
| `SigningIn` | property | `b`, the sign-in window is open (also for `AuthorizeFindMy()`) |
| `FindMyAuthorized` | property | `b`, a Find My jar is held and has `X-APPLE-WEBAUTH-FMIP` |
| `Session()` | method | `→ (s cookie_header, a{ss} client_params, a{ss} webservices)`; error `io.github.ferdousbhai.ICloudSession.Error.SignInRequired` when signed out; revalidates first if the last validate is older than 10 minutes (if Apple is unreachable it answers with what it has) |
| `MergeCookies(as)` | method | raw `Set-Cookie` header values a client received |
| `ReportSignInRequired()` | method | `→ b still_signed_in`. A client got 421/401. The daemon runs `/validate`: on 2xx it keeps the fresh jar and answers true (fetch `Session()` and retry once); on 421/401 it signs out and answers false |
| `SignIn()` | method | opens the sign-in window unless it is open; returns at once, the outcome arrives as property changes |
| `AuthorizeFindMy()` | method | opens the sign-in window on `www.icloud.com/find` (`--find`) unless a window is open; returns at once. On success the captured one-factor jar is kept, unvalidated, as the Find My jar (not if it names another dsid, or when signed out); the main jar is untouched |
| `FindMySession()` | method | `→ (s cookie_header, a{ss} client_params)` of the Find My jar, for the `findme` host; with none, and a password stored, signs in to Find My first; error `io.github.ferdousbhai.ICloudSession.Error.FindMyAuthRequired` when there is still none (`SignInRequired` when signed out) |
| `MergeFindMyCookies(as)` | method | raw `Set-Cookie` header values a client received from the `findme` host |
| `ReportFindMyAuthRequired()` | method | `→ b reauthorized`. A client got HTTP 450 from Find My: the Find My jar is forgotten; with a password stored, the daemon signs in to Find My again (see below) and answers true, so the client retries once |
| `FindMyPasswordStored` | property | `b`, the keyring holds the Apple ID password for the signed-in account (false when signed out) |
| `SetPassword(s)` | method | signs in to Find My once with the password (the autofill window, below). If Apple refuses it, nothing is stored (`…Error.PasswordRejected`); otherwise it is stored in the keyring, and a sign-in that failed for another reason is reported as `…Error.Failed` ("stored the password, but the Find My sign-in with it failed: …"). Also `…Error.Failed` (keyring), `…Error.SignInRequired` |
| `ForgetPassword()` | method | removes every icloud-session item from the keyring |
| `SignOut()` | method | forgets the account (and its Find My jar) and the WebKit profile |

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
{"signed_in":true,"apple_id":"you@example.com","dsid":"1234567890","expires_at":1793000000,"signing_in":false,"find_my_authorized":false,"find_my_password_stored":false}
$ icloud-session sign-in     # opens the window, waits for it to close, prints status
$ icloud-session sign-in --no-wait   # opens it and prints status at once
$ icloud-session authorize-find-my   # the same on Find My's password page (also --no-wait)
$ icloud-session set-password        # store the Apple ID password (see below)
$ icloud-session forget-password
$ icloud-session validate    # {"dsid":…,"apple_id":…,"webservices":{…}}
$ icloud-session sign-out
$ icloud-session <command> --help
```

Exit codes: 0 ok, 1 error, 2 sign-in required (or sign-in not completed),
4 Find My not authorized (or its authorization not completed; 3 in
0.2.1 and older), 64 usage: the table every iCloud tool shares
([docs/CLI.md](../docs/CLI.md)). With `--json` (accepted anywhere; the
output is JSON anyway) an error is one line on stderr,
`{"error":{"code":"sign_in_required","message":…,"exit_code":2,"hint":…}}`.

## Files

| path | written by | contents |
|---|---|---|
| `$XDG_STATE_HOME/icloud-session/account.json` (0600) | daemon | `apple_id`, `dsid`, `cookies` (name, value, domain, path, expires), `client_params` (clientId, clientBuildNumber, clientMasteringNumber), `webservices`, `validated_at`, `captured_at`, and `find_my` (the Find My jar: `cookies`, `client_params`, `captured_at`) once authorized. Session-only cookies (`expires: null`) are kept too. One that cannot be read is moved to `account.json.bad` and the daemon starts signed out. |
| `$XDG_DATA_HOME/icloud-session/webkit/` | sign-in window | its WebKit profile (cookies.sqlite, storage): device trust for later sign-ins |
| `$XDG_CACHE_HOME/icloud-session/webkit/` | sign-in window | WebKit cache |

`$XDG_STATE_HOME` defaults to `~/.local/state`, `$XDG_DATA_HOME` to
`~/.local/share`, `$XDG_CACHE_HOME` to `~/.cache`. Every daemon
write is atomic: temp file in the same directory, mode 0600, fsync, rename.
One account at a time; signing in with another Apple ID replaces it.

## Automatic Find My re-authorization

Opt-in. Find My asks for the Apple ID password again every so often
(HTTP 450). By default an app then shows a banner and the user types the
password into the `AuthorizeFindMy()` window. With the password stored,
the daemon does that step itself:

```console
$ icloud-session set-password                       # asks without echo (or reads stdin)
$ bw get password "Apple ID" | icloud-session set-password           # from Bitwarden
$ op read "op://Private/Apple ID/password" | icloud-session set-password  # from 1Password
$ icloud-session forget-password                    # undo
```

When stdin is not a terminal, `set-password` reads the password from it
(one trailing newline dropped), so any password manager's CLI can pipe it
in: the password never goes through argv, env or disk. `bw` needs an
unlocked vault (`export BW_SESSION=$(bw unlock --raw)` first); `op`
unlocks through the 1Password app's CLI integration.

`set-password` checks the password with one Find My sign-in and stores it
unless Apple refuses it; if the sign-in fails for another reason (Apple
unreachable, the page changed) the password is stored anyway and the
failure reported, since it says nothing about the password. It lives in the Secret Service's default
collection (GNOME Keyring, unlocked at login) as `iCloud (icloud-session):
<apple id>` with the attributes `application=icloud-session`,
`apple-id=<apple id>`; nothing else is written to disk. `sign-out` keeps it (it is yours);
`forget-password` removes it, as does deleting the item in Seahorse
(the daemon notices at its next start).

When a client reports a 450, or asks `FindMySession()` with no Find My
jar, the daemon signs in on Apple's own page, filled in as a password
manager would (a plain-password `POST setup/ws/1/accountLogin` now
answers 421). It runs `icloud-session-signin --find --autofill` hidden
and writes the Apple ID and the password, one per line, to its stdin
(never argv, env or disk). The window loads `www.icloud.com/find`, opens
the sign-in, fills Apple's form (the Apple ID if asked, then the
password; at most twice, so a refused password cannot be retried into a
lockout), and once Find My's `initClient` answers prints the capture as
`--find` does: the one-factor jar with `X-APPLE-WEBAUTH-FMIP`, the
`dsid`, and the page's client params, which become the Find My jar's. If
Apple wants more than the password (a 2FA code, a new-browser check) the
window shows itself after 40 s for the user to finish. The window uses
its own WebKit profile, as `--find` does; the daemon hands it no cookies,
and the main jar is left alone. The client then
retries its request once. Concurrent reports share one sign-in. Apple
saying the password is wrong (the window exits 3), or Find My refusing
the session it just made, stops automatic sign-in until the stored
password changes; any other failure (the window exits non-zero) is
retried after a minute. Either way the apps fall back to the banner and
`AuthorizeFindMy()`. The password and cookie values are never logged;
`ICLOUD_SESSION_SIGNIN_TRACE=1` makes the window report the page's form
fields, buttons and requests (never values) on stderr, to see why an
autofill stalls.

The trade-off: any process running as you can read keyring items while
the keyring is unlocked (as with every Secret Service secret), so a
program that can do that can read your Apple ID password. The password
alone does not pass 2FA for a new sign-in, but it is still your Apple ID
password. Skip this if that is not acceptable; the manual path stays.

## Mock mode

`ICLOUD_SESSION_MOCK=1` (any value but empty or `0`) makes the client
library use no D-Bus at all: a
signed-in fake session (dsid `mock`, Apple ID `mock@example.com`) whose
every `webservices` URL is `ICLOUD_SESSION_MOCK_URL` (default
`http://127.0.0.1:8765`). A request to any other host is sent there with its
path and query unchanged, so an app's mock server can serve fixtures for
CloudKit, Find My and download URLs alike. `sign_in` posts to the mock
server's `/mock/reauthenticate` (a fake that plays a signed-out account
signs it back in); `authorize_find_my`/`sign_out` do nothing, `watch`
never yields and `watch_forever` returns at once.

## Environment

| variable | used by | default |
|---|---|---|
| `ICLOUD_SESSION_MOCK`, `ICLOUD_SESSION_MOCK_URL` | client library | off, `http://127.0.0.1:8765` |
| `ICLOUD_SESSION_SIGNIN_BIN` | daemon | `icloud-session-signin` beside `icloud-sessiond`, else on `PATH` |
| `ICLOUD_SESSION_SIGNIN_TRACE` | sign-in window | off; `1` reports the page's form fields, buttons and requests (never values) on stderr |
| `ICLOUD_SESSION_SIGNIN_UA` | sign-in window | WebKitGTK's own user agent; `safari` for a macOS Safari one, anything else verbatim |
| `ICLOUD_SESSION_SETUP_URL` | daemon (tests) | `https://setup.icloud.com` |
| `ICLOUD_SESSION_TEST_SECRET_FILE` | daemon (tests only) | unset: the Secret Service. Set: a JSON file stands in for the keyring |
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

The iCloud apps pull the package in as a dependency from the signed
`[icloud-for-omarchy]` pacman repository. On its own:

```bash
curl -fsSL https://github.com/ferdousbhai/icloud-for-omarchy/releases/latest/download/install.sh | sudo bash -s -- icloud-session
```

or build the package from this checkout: `cd packaging/icloud-session &&
makepkg -si`. Either installs the three binaries and the D-Bus activation
file. Nothing to enable; the bus starts the daemon on first use. Updates
arrive through `omarchy update`.

For development, without the package (from the repository root):

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
cargo test -p icloud-session -p icloud-sessiond                          # sign-in window included
cargo clippy -p icloud-session -p icloud-sessiond --all-targets -- -D warnings
cargo test -p icloud-session -p icloud-sessiond --no-default-features    # without webkitgtk-6.0
```

The sign-in window is the `signin` feature of `icloud-sessiond` (on by
default); everything else builds and tests without it. The daemon tests run
`icloud-sessiond` through D-Bus activation on a private `dbus-daemon`, with
HOME and the XDG directories in a temp dir, a local HTTP server standing in
for Apple, and a shell script standing in for the sign-in window
(`ICLOUD_SESSION_SIGNIN_BIN`). They never touch the real session bus,
`~/.config`, `~/.local` or Apple. Covered: `Session()` and revalidation,
`MergeCookies`, `ReportSignInRequired` both ways, sign-in (captured,
closed, without params; nothing written under `~/.config/icloud-md`),
sign-out, the client library against the
daemon (requests, rotation, retry, sign-out), the CLI, idle exit, a second
daemon refusing to start, mock mode without D-Bus, and automatic Find My
re-authorization through a fake `--find --autofill` window (which checks
the password reaches it on stdin only) with a file standing in for the
keyring (`ICLOUD_SESSION_TEST_SECRET_FILE`).

## Releasing

Released with the other packages from the repository root; see the root
[README](../README.md#releasing). Its tags are `session-v<version>`.
Releasing icloud-session also sets the version in both crates, the
workspace's `icloud-session` dependency and the snippet above. With
`PUBLISH_CRATE=1` it also publishes the client crate to crates.io once the
release is verified (so it then needs a crates.io token); by default the
crate is not published.

## License

MIT, see [LICENSE](../LICENSE).
