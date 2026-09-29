# Find My (icloud-findmy)

Find My for Omarchy: see where your Apple devices are on a map, play a
sound on one, or turn on Lost Mode. The app also keeps a local history of
where each device has been, so the map can draw its trail.

The iCloud sign-in comes from the
[`icloud-session`](https://github.com/ferdousbhai/icloud-session) package,
which every iCloud app on the machine shares: after the first sign-in from
any of them (Notes, Find My, Photos), Find My opens with no prompt.

## Requirements

- An Apple ID with **Advanced Data Protection turned off** (off by
  default) and **Access iCloud Data on the Web turned on**, the same as
  Notes. On iPhone/iPad: Settings → your name → iCloud.
- **Find My** turned on for the devices you want to see.

AirTags and other Find My network items are not shown: Apple's web
service lists only devices signed in to your Apple ID.

## Install

On Omarchy (or any Arch Linux), one command trusts the package-signing
key, adds the signed `[icloud-for-omarchy]` repository (all the iCloud
apps and what they share), and installs the app:

```bash
curl -fsSL https://ferdousbhai.com/icloud-findmy/install.sh | sudo bash -s -- icloud-findmy
```

Updates then arrive with `omarchy update`. The script is
[`install.sh`](../install.sh) at the root of this repository, attached to
every release; the one-liner runs the copy from the latest release. It
also installs an Omarchy `pre-refresh-pacman` hook so
`omarchy refresh pacman` keeps the repository. See the root
[README](../README.md#install) for uninstalling.

## Using it

- Your devices are listed on the left with their model, battery and when
  they were last seen. Click one to center the map on it.
- Markers on the map are clickable too; the selected device is
  highlighted and its trail for the last 24 hours is drawn.
- The **⋮** menu has **Play Sound** and **Lost Mode…** for the selected
  device. Lost Mode asks for a phone number and a message to show on the
  locked device.
- The list refreshes every minute while the window is on screen (not
  while it is minimised or on another workspace), and on **Ctrl+R**.
  Only the first load and **Ctrl+R** (or the refresh button) ask your
  devices to report a fresh position; the minute ticks show what Apple
  last heard, so the devices are not woken every minute.
- When no one is signed in to iCloud, or the sign-in has expired, a
  banner offers **Sign In**, which opens Apple's sign-in page (the
  icloud-session sign-in window). The banner follows the sign-in as it
  happens, in this app or any other, and the list loads once it is done.
- Apple's Find My also asks for your Apple password again from time to
  time, even while you are signed in (as icloud.com does on its Find
  Devices page). The banner then says **Find My needs your Apple
  password**; **Enter Password** opens Apple's Find My page in the
  sign-in window, the banner reads "Finish in the Apple window…" while
  it is open, and the devices are located again once Find My answers.

### History

Positions are kept only on this computer, in
`~/.local/share/icloud-findmy/history.db` (SQLite, table `history`:
`device_id, ts, lat, lon, accuracy, battery`). A row is written on a
refresh only when the device moved more than 25 m, and more than the
larger accuracy radius of its fixes, since the last stored point, so a
phone sitting on a desk does not fill the file. Positions older than 30
days are deleted when the app opens the file and after every refresh.
The directory is readable only by you (`0700`, the database files
`0600`). Delete the file to clear the history.

## Command line

The same `icloud-findmy` binary runs without its window when given a
command (GTK is not started), with the same Find My client and history as
the app:

```console
$ icloud-findmy devices                  # name, model, battery, online, last fix
$ icloud-findmy devices --locate --coords
$ icloud-findmy locate "Dous iPhone 15 Pro" --wait 60
$ icloud-findmy play-sound iphone        # asks first; --yes to skip
$ icloud-findmy lost-mode ipad --phone "+358 40 123 4567" --message "Please call me" --yes
$ icloud-findmy history iphone --since 7d
$ icloud-findmy prune-history
$ icloud-findmy help
```

- `devices` shows what Apple last heard; `--locate` asks every device to
  report first (it wakes them, like **Ctrl+R** in the app). Coordinates
  are printed only with `--coords`. Positions are stored in the history
  as the app stores them.
- `locate` asks one device for a fresh fix and waits (30 s unless
  `--wait SECS`) until one newer than the last arrives, then prints it
  with its coordinates; it fails if none comes.
- `play-sound` and `lost-mode` ask for confirmation on a terminal, and
  refuse to act without `--yes` when stdin is not one.
- A device is named by its ID or its name, ignoring case (a unique part
  of the name is enough); an ambiguous name lists the matching devices.
- `history` prints the stored trail (24 h unless `--since`, e.g. `90m`,
  `7d`); a device ID already in the history needs no network.
- `--json` prints JSON on stdout (errors as JSON on stderr) for scripts
  and agents; `--data-dir DIR` keeps `history.db` in `DIR` instead of
  `~/.local/share/icloud-findmy`.

Exit codes: 0 ok, 1 error, 2 sign-in required (`icloud-session
sign-in`), 4 Find My needs the Apple password after `icloud-session`
could not re-authorize with a stored one (`icloud-session
authorize-find-my`), 64 usage.

## Building from source

Needs `rust`, `cargo`, `gtk4`, `libadwaita` and `libshumate`; the
`icloud-session` crate is the workspace's [`session/`](../session). From
the repository root:

```bash
bin/build icloud-findmy
target/release/icloud-findmy
```

### Running without an Apple account

`examples/fake_findme.rs` is a small fake of Apple's Find My service
that serves `tests/fixtures/` (and walks the iPhone a little on every
refresh, so the trail has something to show). `icloud-session`'s mock
mode points every service at it:

```bash
cargo run --example fake_findme &     # listens on 127.0.0.1:8765
ICLOUD_SESSION_MOCK=1 XDG_DATA_HOME=/tmp/findmy-dev cargo run
```

(`ICLOUD_SESSION_MOCK_URL` moves the fake elsewhere; `XDG_DATA_HOME`
keeps the fake history out of your real one.) The command line works the
same way, with `--data-dir` for the history; the fake records every Play
Sound and Lost Mode request instead of acting, and lists them at
`GET /fake/actions`:

```bash
ICLOUD_SESSION_MOCK=1 cargo run -- play-sound "Test's iPhone" --yes --data-dir /tmp/findmy-dev
curl -s http://127.0.0.1:8765/fake/actions
```

`tests/cli.rs` runs the built binary this way for every command.

### Tests

```bash
cargo test -p icloud-findmy
```

runs the full Rust test suite (the root `bin/test` runs every crate's). The core
(`findme.rs`, `history.rs`, `models.rs`) and the command line (`cli.rs`)
have no GTK dependency, so `cargo test --no-default-features` tests them
on a machine without the GTK stack (the binary is then the command line
only); `--features gtk` builds the widgets that need only GTK and
libadwaita.

## How it works

`src/findme.rs` talks to Apple's `findme` web service the way icloud.com
does (endpoints and payloads as in pyicloud's
`FindMyiPhoneServiceManager`): `initClient` once, then `refreshClient`
with the returned `serverContext` and `shouldLocate`, plus `playSound`
and `lostDevice`. Every request goes through the `icloud-session` client,
which gets the cookies from `icloud-sessiond` (the D-Bus service that owns
the Apple account), hands rotated cookies back to it, and reports
`SignInRequired` when Apple ends the session, or `FindMyAuthRequired`
when Find My answers HTTP 450 (it wants the password again; not retried,
since another `initClient` would only answer 450 too). The banner watches
the service's status on its own thread. Network and database calls run
on worker threads, never on the GTK main loop.

## Releasing

Released with the other packages from the repository root; see the root
[README](../README.md#releasing). Its tags are `findmy-v<version>`.
