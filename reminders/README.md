# Reminders (icloud-reminders)

Your iCloud Reminders on Omarchy: the lists you keep on your iPhone, a
check box to complete each reminder, a line to add one, and a desktop
notification when one falls due, whether the window is open or not.

The iCloud sign-in comes from the
[`icloud-session`](../session/README.md) package, which every iCloud app
on the machine shares: after the first sign-in from any of them,
Reminders opens with no prompt.

## Requirements

- An Apple ID with **Advanced Data Protection turned off** (off by
  default) and **Access iCloud Data on the Web turned on**, the same as
  the other apps. On iPhone/iPad: Settings → your name → iCloud.
- Reminders upgraded to the iOS 13 format (every account made or used
  since 2019 is): those live in CloudKit, which this app reads; the old
  CalDAV reminders are not shown.

## Install

On Omarchy (or any Arch Linux), one command trusts the package-signing
key, adds the signed `[icloud-for-omarchy]` repository, and installs the
app:

```bash
curl -fsSL https://github.com/ferdousbhai/icloud-for-omarchy/releases/latest/download/install-reminders.sh | sudo bash
```

`install-reminders.sh` is [`install.sh`](../install.sh) set to install
just this app. Updates arrive
with `omarchy update`. It also starts the notification timer (below) at
once; otherwise it starts at your next login.

## Using it

- The sidebar lists **Upcoming** (every open reminder, soonest due first)
  and each of your lists with how many reminders are open in it.
- Tick a reminder's circle to complete it; the check button in the header
  shows completed ones too, so you can untick them.
- Type a title in **New reminder** (Ctrl+N), and optionally a due time in
  the box beside it (`tomorrow 9:00`, `2026-10-12`, `17:30`, `+2h`), then
  Enter. It goes to the list on screen, or under Upcoming to the list
  named Reminders.
- Click a reminder to change its title, notes or due date, or delete it.
  A deleted reminder goes to **Recently Deleted** on your Apple devices.
- The window syncs on opening, every minute while it is on screen, and on
  **Ctrl+R**. Overdue reminders show their date in red.
- When nobody is signed in to iCloud, a banner offers **Sign In**.

Changes go straight to iCloud: without a network a change fails and says
so, and nothing is queued.

### Notifications

A systemd user timer, `icloud-reminders-background.timer`, runs
`icloud-reminders background` every minute. That syncs when the last
attempt is over 4 minutes old (skipped when signed out or offline; the
journal says why), then shows a notification for each open reminder that
has just fallen due:

- through Omarchy's own `omarchy-notification-send` (its look, a
  reminder glyph, click to open the app); without Omarchy the journal
  says it is missing and nothing is shown;
- once per due time: moving a reminder's due time makes it notify again;
- a timed reminder at its time; an all-day one at 09:00;
- up to 12 hours late, for a machine that slept through the time;
- not for reminders created or moved into the past, nor, on the very
  first run, for what was already overdue.

Alerts set on an Apple device (extra alarms, location alerts) are not
read: notifications follow the due date. Changing the due date here does
not move those alerts; the CLI and the edit dialog say so when a reminder
has some.

```bash
systemctl --user status icloud-reminders-background.timer
journalctl --user -u icloud-reminders-background     # what each run did
systemctl --user disable --now icloud-reminders-background.timer   # no notifications
```

### What is kept on this computer

`~/.local/share/icloud-reminders/`: `cache.json` (the lists and
reminders as last synced, and CloudKit's sync token; only a cache, since
iCloud holds the reminders), `notified.json` (what has notified), and
`replica` (this computer's ID in the merge tokens Apple keeps per field).
Delete the directory to start over; the next sync fetches everything.

## Command line

`icloud-reminders` given a command runs it without the window. It links
no GTK, so the timer can run it every minute; with no command it opens
the app, `icloud-reminders-app`.

```console
$ icloud-reminders lists
$ icloud-reminders list                       # open reminders, soonest due first
$ icloud-reminders list groceries --all
$ icloud-reminders show passport
$ icloud-reminders add "Buy bread" --list Groceries --due "tomorrow 9:00" --notes rye
$ icloud-reminders edit bread --due +2h
$ icloud-reminders edit bread --no-due --title "Buy rye bread"
$ icloud-reminders complete bread
$ icloud-reminders uncomplete bread
$ icloud-reminders delete bread --yes
$ icloud-reminders sync [--full]
$ icloud-reminders background                 # what the timer runs
```

- A reminder is named by its ID, its title (ignoring case) or a unique
  part of it; open reminders are matched before completed ones. A list by
  its ID or name.
- Reading commands sync first and fail with `offline` without a network;
  `--cached` reads what the last sync left instead.
- `delete` asks on a terminal and needs `--yes` without one.
- `--json` prints JSON on stdout and an error as one JSON line on stderr,
  `{"error":{"code":"not_found","message":…,"exit_code":1}}` (codes:
  `usage`, `sign_in_required`, `offline`, `not_found`, `ambiguous`,
  `cancelled`, `error`); `--data-dir DIR` keeps the cache elsewhere.
- `icloud-reminders COMMAND --help` describes one command and its JSON.

Exit codes: 0 ok, 1 error, 2 sign-in required (`icloud-session
sign-in`), 64 usage: the table every iCloud tool shares
([docs/CLI.md](../docs/CLI.md)).

## Building from source

Needs `rust`, `cargo`, `gtk4` and `libadwaita`. From the repository root:

```bash
bin/build icloud-reminders
target/release/icloud-reminders-app      # the app; icloud-reminders is the command line
```

### Running without an Apple account

`examples/fake_reminders.rs` is an in-memory fake of the CloudKit
Reminders zone (sync tokens, paging, partial updates, change tags and
`CONFLICT`), seeded with two lists and reminders due around now.
`icloud-session`'s mock mode points every service at it:

```bash
cargo run -p icloud-reminders --example fake_reminders &     # 127.0.0.1:8765
ICLOUD_SESSION_MOCK=1 XDG_DATA_HOME=/tmp/reminders-dev cargo run -p icloud-reminders
ICLOUD_SESSION_MOCK=1 cargo run -p icloud-reminders -- list --data-dir /tmp/reminders-dev
curl -s http://127.0.0.1:8765/fake/requests                    # what was sent
```

### Tests

```bash
cargo test -p icloud-reminders
```

`tests/cli.rs` runs the built command line against the fake for every
command, including notifications (a stand-in `omarchy-notification-send`
records them). The library has no GTK dependency (the window's code is in
`src/bin/app/`), so `cargo test --no-default-features` runs on a machine
without the GTK stack.

## How it works

Since iOS 13, Reminders live in CloudKit: the private database of the
`com.apple.reminders` container, zone `Reminders`, reached through the
`ckdatabasews` web service as icloud.com's Reminders does. There is no
public description of it; the request shapes and record fields follow
[timlaing/pyicloud](https://github.com/timlaing/pyicloud)'s Reminders
service (`services/reminders/`), whose synthetic fixtures are in
`tests/fixtures/pyicloud/`, and the due-date semantics follow
[Psavvas/iCloud-Reminders-for-Windows](https://github.com/Psavvas/iCloud-Reminders-for-Windows)'
protocol notes.

- **Records.** `List` (`Name`, `Color`, `ReminderIDs` for its order) and
  `Reminder` (`TitleDocument`, `NotesDocument`, `List`, `Completed`,
  `CompletionDate`, `DueDate`, `AllDay`, `TimeZone`, `Priority`,
  `Flagged`, `Deleted`, `LastModifiedDate`, `ResolutionTokenMap`, ...).
  Titles and notes are Apple's mergeable "topotext" strings, a protobuf
  in a protobuf, zlib-compressed (`src/topotext.rs`).
- **Sync** (`src/service.rs`): every `List` through `changes/zone`, then
  the `Reminder` records changed since the stored sync token (all of them
  without one, or when CloudKit refuses it).
- **Writes**: `records/modify` with the record's change tag. An update
  writes only the fields it changes, and raises those fields' counters in
  `ResolutionTokenMap` (Apple's per-field merge tokens) with this
  computer's replica ID; a delete is an update to `Deleted = 1`. If the
  reminder changed on another device meanwhile (`CONFLICT`), it is
  fetched again and the change written once more over it.
- **Due dates** (`src/due.rs`): `DueDate` is a wall-clock time stored as
  if it were UTC (09:00 is `T09:00:00Z`); `TimeZone` names the zone it is
  in, or it floats in the local one. New timed reminders are anchored to
  this computer's zone.

Not done yet: lists cannot be created, renamed or deleted (the Windows
client found such writes accepted but not shown on Apple devices),
subtasks show flat, and tags, attachments, recurrence and alarms are not
read.

## Releasing

Released with the other packages from the repository root; see the root
[README](../README.md#releasing). Its tags are `reminders-v<version>`.
The first release must name it (`bin/release icloud-reminders 0.1.0`),
since a release carries forward only packages an earlier one published.
