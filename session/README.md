# icloud-session

A Rust crate and command-line tool that reads the iCloud sign-in saved by
[icloud-md](https://www.npmjs.com/package/icloud-md) and makes requests to
icloud.com with it, so several apps can share one sign-in.

icloud-md signs in through Apple's own page and stores the icloud.com cookie
jar. `icloud-session` never signs in by itself: it reads that jar, attaches it
to requests, writes back the cookies Apple rotates, and reports
`SignInRequired` when Apple stops accepting it. Notes, Find My and Photos all
go through it, so there is one sign-in dialog, one 2FA prompt and one 30-day
session for all of them.

## Requirements

- `npm install -g icloud-md` (Node.js 20+), signed in once, for example
  through Notes or `icloud-md clone`.
- An Apple ID with Advanced Data Protection off and "Access iCloud Data on the
  Web" on.

## Library

```toml
[dependencies]
icloud-session = "=0.1.0"
```

```rust
use icloud_session::{Error, Session};

let session = Session::load()?;          // newest account icloud-md knows
let ws = session.webservices()?;         // cached /validate, validates if stale
let findme = ws.url("findme").unwrap();
let r = session.post_json(&format!("{findme}/fmipservice/client/web/refreshClient"), &body)?;
let devices: serde_json::Value = r.json()?;

match session.get(url) {
    Err(Error::SignInRequired) => { /* show a banner that runs `icloud-session reauthenticate` */ }
    other => { /* ... */ }
}
```

| item | what it does |
|---|---|
| `Session::load()` | The account whose `session.local.json` was written last. `SignInRequired` if there is none. |
| `Session::load_dsid(dsid)` | One specific account. |
| `session.webservices()` | The `webservices` map (`ckdatabasews`, `findme`, ...) from `/validate`, cached for 10 minutes machine-wide. |
| `session.get(url)`, `post_json(url, &value)`, `post_bytes(url, content_type, bytes)` | Request with the cookie jar, `Origin`/`Referer: https://www.icloud.com`, and `clientBuildNumber`, `clientMasteringNumber`, `clientId`, `dsid` appended to the query (a parameter already in the URL is left alone). |
| `session.download(url, dest)` | Streams to a temp file beside `dest`, renamed on success. Cookies attached, no client parameters. |
| `session.apple_id()`, `dsid()`, `validated_at()`, `expires_at()` | Account facts from local files. `expires_at` is the persistent `X-APPLE-WEBAUTH-TOKEN` expiry from the Chromium profile. |
| `Session::reauthenticate()`, `Session::reauthenticate_in(dir)` | Runs `icloud-md reauthenticate [dir]` with the terminal attached and waits. |
| `status()` | Offline `Status { signed_in, apple_id, dsid, expires_at, validated_at }`. |
| `expiry::token_expiry(db)`, `expiry::latest_token_expiry(dir)` | The expiry query on its own. |
| `Config`, `Session::load_with(config)`, `status_with(&config)` | Same, with explicit paths (tests, tools). |

Errors: `SignInRequired` (no session file, HTTP 421 or 401, or a sign-in stuck
at 2FA), `Corrupt` (session file unreadable after retries), `Http { status,
body }` (any other non-2xx), `Network`, `Io`.

Every call is blocking (ureq with rustls, no async runtime). GTK apps run them
on `gio::spawn_blocking` or a small thread pool. `Session` is cheap to clone
and safe to share between threads.

## Command line

JSON goes to stdout, errors to stderr.

```console
$ icloud-session status
{"signed_in":true,"apple_id":"you@example.com","dsid":"1234567890","expires_at":"2026-10-28T09:00:00Z","validated_at":"2026-09-28T10:02:11Z"}

$ icloud-session validate
{"dsid":"1234567890","apple_id":"you@example.com","validated_at":"2026-09-28T10:02:11Z","webservices":{"ckdatabasews":"https://p42-ckdatabasews.icloud.com:443", ...}}

$ icloud-session reauthenticate ~/Documents/icloud-notes
```

- `status` reads local files only, never the network, and always exits 0.
  Times are RFC 3339 UTC or `null`. `signed_in` is false when the session file
  is missing, when Apple answered 421/401 after the session file was last
  written, or when the persistent sign-in cookie has expired.
- `validate` prints the cached `/validate` result, calling Apple only when the
  cache is older than 10 minutes.
- `reauthenticate [directory]` runs `icloud-md reauthenticate [directory]`
  interactively. icloud-md finds the account from a folder cloned with
  `icloud-md clone` (it walks up from the directory, or from the current
  directory when none is given), then prints `status`.

Exit codes: 0 ok, 1 error, 2 sign-in required, 64 usage.

## Files

| path | owner | contents |
|---|---|---|
| `~/.config/icloud-md/accounts/<dsid>/session.local.json` | icloud-md | `cookie`, `clientId`, `clientBuildNumber`, `clientMasteringNumber`, `capturedAt` |
| `~/.config/icloud-md/accounts/<dsid>/session.local.json.lock` | icloud-session | empty; `flock`ed while writing |
| `~/.config/icloud-md/accounts/<dsid>/meta.json` | icloud-md | `appleId`, `dsid` (read for `apple_id` before the first validate) |
| `~/.config/icloud-md/accounts/<dsid>/browser-profile/**/Cookies` | Chromium | read-only, for the token expiry |
| `~/.cache/icloud-session/<dsid>.json` | icloud-session | `validated_at`, `webservices`, `apple_id`, `sign_in_required_at` |

`icloud-session` is the only code in these apps that reads `~/.config/icloud-md`.

## How the session file is shared

icloud-md writes `session.local.json` with a plain `writeFile` and no lock, so
`icloud-session` assumes the file can change, or be half-written, at any time:

- **Re-read before every request.** Nothing caches cookies in memory, so a
  cookie rotated by icloud-md or another app is used on the next request.
- **Retry a truncated read.** A file that does not parse (or lacks a required
  field) is re-read up to five more times, 50 ms apart, before it is `Corrupt`.
- **Merge only what rotated.** After any 2xx response with `Set-Cookie`, take
  an exclusive `flock` on `session.local.json.lock`, re-read the file, apply
  just the cookies this response set (existing cookies keep their place, new
  names go at the end, the same rule as icloud-md's
  `mergeSetCookiesIntoSession`), and write it only if something changed.
  Unknown fields and their order are kept. The jar in memory never overwrites
  the file.
- **Write atomically.** Temp file in the same directory, mode 0600, fsync,
  rename.
- **Validate once for everyone.** `webservices()` uses the cache if it is
  younger than 10 minutes (the browser's own heartbeat is 14). Otherwise it
  takes the lock, reads the cache again (another process may have just
  validated), and only then calls `/validate`, so racing processes make one
  call and one token rotation.
- **Record sign-in failures.** A 421 or 401 sets `sign_in_required_at` in the
  cache. `status` reports signed out until icloud-md writes a newer session
  file (a new sign-in) or a later `/validate` succeeds.

The lock orders `icloud-session`'s own processes; icloud-md does not take it,
which is why the merge re-reads under the lock rather than trusting memory.

`/validate` is called as icloud-md's `checkAuthentication` calls it: `POST
https://setup.icloud.com/setup/ws/1/validate` with `clientBuildNumber`,
`clientMasteringNumber`, `clientId`, a random `requestId` and `dsid` in the
query, and `Cookie`, `Origin: https://www.icloud.com`,
`Referer: https://www.icloud.com/`, `Accept: application/json` headers.

## Mock mode

`ICLOUD_SESSION_MOCK=1` gives a signed-in fake session (dsid `mock`, Apple ID
`mock@example.com`) that reads and writes no files and calls no Apple host.
Every `webservices` URL is `ICLOUD_SESSION_MOCK_URL` (default
`http://127.0.0.1:8765`), and a request to any other host is sent there with
its path and query unchanged, so an app's mock server can serve fixtures for
CloudKit, Find My and download URLs alike. `reauthenticate` does nothing.

## Environment

| variable | default |
|---|---|
| `ICLOUD_MD_CONFIG_DIR` | `~/.config/icloud-md` |
| `ICLOUD_SESSION_CACHE_DIR` | `$XDG_CACHE_HOME/icloud-session`, else `~/.cache/icloud-session` |
| `ICLOUD_SESSION_MOCK`, `ICLOUD_SESSION_MOCK_URL` | off, `http://127.0.0.1:8765` |
| `ICLOUD_MD_BIN` | `icloud-md` |
| `ICLOUD_SESSION_SETUP_URL` | `https://setup.icloud.com` (tests only) |

## Development

```bash
cargo test
cargo clippy --all-targets -- -D warnings
```

The tests run against a local HTTP server in temp directories and never touch
`~/.config/icloud-md` or `~/.cache`. They cover the rotation merge, truncated
reads, racing threads and processes making one `/validate` call, the expiry
query against a fixture Cookies database (including one Chromium holds
locked), 421/401 handling and mock mode.

`pkgbuild/PKGBUILD` builds the `icloud-session` pacman package from the
committed tree (`cd pkgbuild && makepkg`).

## License

MIT, see [LICENSE](LICENSE).
