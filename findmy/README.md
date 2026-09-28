# Find My (icloud-findmy)

Find My for Omarchy: see where your Apple devices are on a map, play a
sound on one, or turn on Lost Mode. The app also keeps a local history of
where each device has been, so the map can draw its trail.

It uses the same iCloud sign-in as Notes (icloud-notes), through the
[`icloud-session`](https://github.com/ferdousbhai/icloud-session) crate:
if you signed in once for Notes, Find My opens with no prompt.

## Requirements

- An Apple ID with **Advanced Data Protection turned off** (off by
  default) and **Access iCloud Data on the Web turned on**, the same as
  Notes. On iPhone/iPad: Settings → your name → iCloud.
- **Find My** turned on for the devices you want to see.
- The sign-in tool: `npm install -g icloud-md` (needs Node.js 20+). The
  installer does this for you when npm is present.

AirTags and other Find My network items are not shown: Apple's web
service lists only devices signed in to your Apple ID.

## Install

On Omarchy (or any Arch Linux), one command trusts the package-signing
key, adds the signed `[icloud-findmy]` and `[icloud-session]`
repositories, and installs the app:

```bash
curl -fsSL https://ferdousbhai.com/icloud-findmy/install.sh | sudo bash
```

Updates then arrive with `omarchy update`. The script is
[`install.sh`](install.sh) in this repo, and the copy the one-liner runs
is the one attached to the latest release, verified with it. It also
installs an Omarchy `pre-refresh-pacman` hook per repository so
`omarchy refresh pacman` keeps them.

To uninstall: `omarchy pkg drop icloud-findmy`, then remove
`/etc/pacman.d/icloud-findmy.conf`, its `Include` line in
`/etc/pacman.conf`, and
`~/.config/omarchy/hooks/pre-refresh-pacman.d/icloud-findmy` (and the same
three for `icloud-session` if nothing else uses it).

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
- If the iCloud sign-in has expired, a banner offers **Sign In**, which
  opens Apple's sign-in page through icloud-md and then carries on.

### History

Positions are kept only on this computer, in
`~/.local/share/icloud-findmy/history.db` (SQLite, table `history`:
`device_id, ts, lat, lon, accuracy, battery`). A row is written on a
refresh only when the device moved more than 25 m, and more than the
accuracy radius of its fixes, since the last stored point, so a phone
sitting on a desk does not fill the file. Delete the file to clear the
history.

## Building from source

Needs `rust`, `cargo`, `gtk4`, `libadwaita` and `libshumate`, and the
[`icloud-session`](https://github.com/ferdousbhai/icloud-session)
checkout next to this one (`../icloud-session`).

```bash
./bin/build
./target/release/icloud-findmy
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
keeps the fake history out of your real one.)

### Tests

```bash
./bin/test
```

runs the Rust tests and the installer check. The core (`findme.rs`,
`history.rs`, `models.rs`) has no GTK dependency;
`cargo test --no-default-features` tests it on a machine without the GTK
stack, and `bin/test` falls back to that when libshumate is missing.
`--features gtk` builds the widgets that need only GTK and libadwaita.

## How it works

`src/findme.rs` talks to Apple's `findme` web service the way icloud.com
does (endpoints and payloads as in pyicloud's
`FindMyiPhoneServiceManager`): `initClient` once, then `refreshClient`
with the returned `serverContext` and `shouldLocate`, plus `playSound`
and `lostDevice`. Every request goes through `icloud-session`, which
attaches the cookies, rotates them safely alongside the other iCloud
apps, and reports `SignInRequired` when Apple ends the session. Network
and database calls run on worker threads, never on the GTK main loop.

## Releasing

`bin/release <version>` tests, tags, builds and signs the package, and
publishes the one-package pacman repository as the tag's GitHub release;
`bin/verify-release` then installs it in a clean Arch container with the
public one-liner and rolls the release back if that fails.
