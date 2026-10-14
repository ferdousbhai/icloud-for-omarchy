# The command line: every GUI feature, and how the tools agree

Every iCloud app here can be driven from a terminal or by an agent, with no
window and no display. This page maps each thing a person can do in a window
to the command that does it, then fixes the conventions all the tools share.
[AGENTS.md](AGENTS.md) is the short version for an agent.

| Tool | Package | What it drives |
|---|---|---|
| `icloud-session` | icloud-session | the one Apple sign-in every app uses (D-Bus daemon, sign-in window) |
| `icloud-notes <command>` | icloud-notes | the Notes app: the vault `~/Documents/Notes`, its rules and its lock |
| (`icloud-notes-sync`) | icloud-notes | the sync engine under Notes, installed off PATH at `/usr/lib/icloud-notes/icloud-notes-sync`; reached through `icloud-notes`, run directly only for development |
| `icloud-photos <command>` | icloud-photos | the Photos app: catalog, downloads, uploads, deletes |
| `icloud-findmy <command>` | icloud-findmy | the Find My app: devices, locate, sound, Lost Mode, history |

`icloud-notes`, `icloud-photos`, and `icloud-findmy` open their window without
arguments and run their CLI without GUI initialization when given a command.
Photos and Find My use GTK-free CLI binaries and separate native windows.
Contacts and tasks are now separate unified products; see the migration section.

## The audit: GUI feature → command

"Added" marks a gap this audit found and filled; everything else already
existed. The GUI code audited: `notes/qml/main.qml` and
`notes/src/notesbackend.*`, `photos/src/ui/`, `findmy/src/bin/app/`, and the
sign-in flows in `sessiond/src/bin/signin.rs` and `sessiond/src/daemon.rs`.

### Notes (`notes/qml`, `notes/src`)

Notes had no command line beyond `icloud-notes --sync` (the background
timer's sync). All of it is new, in the app binary: see
[Why in `icloud-notes`](#why-notes-commands-live-in-icloud-notes).

| In the window | Command | |
|---|---|---|
| Folder list with recursive note counts | `icloud-notes folders` | added |
| Note list: title, preview, date, badges (new, conflict, read-only, ...) | `icloud-notes list [--folder F] [--flag FLAG]` | added |
| Search box | `icloud-notes search QUERY` | added |
| Open a note: text, attachments, read-only reason | `icloud-notes read NOTE [--raw]` | added |
| Edit (saves on its own) | `icloud-notes write NOTE --body/--file/--stdin [--append]` | added |
| Ctrl+S past a save warning (conflict markers, duplicate id) | `icloud-notes write ... --force` | added |
| Bold, italic, link buttons | `write` (they only insert Markdown) | added |
| Checklist toggle (Ctrl+Enter) | `icloud-notes toggle NOTE LINE` | added |
| New note (Ctrl+N) | `icloud-notes new TITLE [--folder F] [--body ...]` | added |
| Rename (edit the title line) | `icloud-notes rename NOTE TITLE` | added |
| Delete note (to the trash) | `icloud-notes delete NOTE --yes` | added |
| (not in the window) move a note to another folder | `icloud-notes move NOTE FOLDER` | added |
| Export PDF | `icloud-notes export-pdf NOTE` | added |
| New folder / Rename folder… / Delete folder… | `icloud-notes new-folder`, `rename-folder`, `delete-folder --yes` | added |
| Conflict picker: per change, All from this computer, All from iCloud, Keep both | `icloud-notes resolve NOTE --choices local,remote,both,...` / `--all local\|remote\|both` | added |
| Conflict: Edit as text | `read --raw`, then `write --force` | added |
| Unreadable markers: Remove the markers / Use the last synced version | `icloud-notes recover NOTE --strip` / `--synced` | added |
| Automatic sync (launch, focus, every minute) | `icloud-notes sync` (push, then pull, in one engine run) | added |
| Pull | `icloud-notes pull` | added (the engine's `pull` existed) |
| Push… preview, then Push now; Status preview | `icloud-notes push --dry-run`, then `icloud-notes push` | added |
| Note history, diffs | `icloud-notes history NOTE [--records]`, `icloud-notes diff NOTE REF` | added (the engine's existed) |
| Discard local edits (the History dialog points to a terminal) | `icloud-notes restore NOTE --yes` | added (the engine's `restore` existed) |
| Link your Apple Notes: Clone my notes | `icloud-notes clone` | added |
| Sign-in banner, days left, Sign in | `icloud-notes status` (`signed_in`, `sign_in_days_left`), then `icloud-session sign-in` | added |
| Sync log | each command's output; `sync`'s data carries `log` | added |
| Background sync while closed | `icloud-notes --sync` (the systemd timer) | existed |
| Refresh | nothing: every command reads the disk afresh | n/a |
| Merging unsaved editor text with a change on disk | nothing: a command writes whole under the lock, with no editor buffer | n/a |
| Theme, fonts, pane sizes | presentation only | n/a |

### Photos (`photos/src/ui`)

| In the window | Command | |
|---|---|---|
| All Photos grid, by month; album sidebar | `icloud-photos list [--album ID] [--since DATE] [--kind K] [--limit N]`, `icloud-photos albums` | existed |
| Thumbnails | `icloud-photos thumb ID [--out PATH]` | existed |
| Viewer: large preview | `icloud-photos download ID --medium [--out DIR]` | existed |
| Viewer: info line | `icloud-photos info ID` | existed |
| Viewer: Download Original (Ctrl+S); the Saved toast's Show | `icloud-photos download ID...` (prints the path) | existed |
| Viewer: Open in Default App (Ctrl+O) | `icloud-photos open ID [--medium]` | added |
| Viewer: Delete (Del) | `icloud-photos delete ID... --yes` | existed |
| Viewer: previous / next | the order of `list` | n/a |
| Sync Now (Ctrl+R) | `icloud-photos sync [--full]` | existed |
| Upload… (Ctrl+U), Stop After This File | `icloud-photos upload FILE... [--album ID] [--no-sync]` (album, no-sync: command line only) | existed |
| Preferences: library folder, download originals on demand / all | `icloud-photos config [--library-dir DIR] [--download on-demand\|all]`; `download --all` | existed |
| Sign-in banner | `icloud-photos status` (exits 3 signed out), `icloud-photos sign-in` | existed |
| About | `icloud-photos --version` | existed |
| Cache trimming (automatic) | `icloud-photos prune-cache` | existed |
| Favourites, editing, album management | not in the app either | n/a |

### Find My (`findmy/src/bin/app`)

| In the window | Command | |
|---|---|---|
| Device list: model, battery, online, Lost Mode | `icloud-findmy devices` | existed |
| Map pins | `icloud-findmy devices --coords` | existed |
| Refresh (Ctrl+R; every 60 s while shown) | `icloud-findmy devices --locate`; the cadence is the caller's | existed |
| Select a device for a fresh fix | `icloud-findmy locate NAME\|ID [--wait SECS]` | existed |
| History trail on the map | `icloud-findmy history NAME\|ID [--since 24h]` | existed |
| Play Sound | `icloud-findmy play-sound NAME\|ID --yes` | existed |
| Lost Mode… (phone, message) | `icloud-findmy lost-mode NAME\|ID --phone P --message M --yes` | existed |
| Sign-in banner | exit 3, then `icloud-session sign-in` | existed |
| Find My password banner (Enter Password) | exit 3 (code `find_my_auth_required`), then `icloud-session authorize-find-my` (or `set-password` once) | existed |
| Old positions dropped after 30 days | `icloud-findmy prune-history` | existed |

### Sign-in (`sessiond`)

| In the window / daemon | Command | |
|---|---|---|
| State, expiry | `icloud-session status` (exits 3 signed out) | existed |
| Sign in (Apple's page) | `icloud-session sign-in [--no-wait]` | `--no-wait` added |
| Find My's password page | `icloud-session authorize-find-my [--no-wait]` | `--no-wait` added |
| Remember the password for Find My | `icloud-session set-password` (a no-echo prompt, or stdin: `bw get password "Apple ID" \| icloud-session set-password`, `op read "op://…/password" \| icloud-session set-password`) | existed |
| Forget it | `icloud-session forget-password` | existed |
| Sign out | `icloud-session sign-out` | existed |
| Check the session with Apple | `icloud-session validate` | existed |
| Agent skill for these tools | `icloud-session skill` (print), `skill install`, `skill remove` | added |

### What cannot be a command

- **Signing in.** Apple's sign-in page (password, two-factor code, "Trust
  this browser") runs in icloud-session's WebKit window, and a person types
  into it. `icloud-session sign-in` opens it and waits; nothing signs in
  headlessly. A sign-in with "Keep me signed in" lasts about 30 days.
- **Find My's password page**, the same way, unless the password was stored
  once with `icloud-session set-password`; after that the daemon answers
  it by itself.
- **Unlocking a password manager** to pipe the password into
  `set-password`: `bw unlock` asks on the terminal unless `BW_SESSION` is
  exported (1Password's `op` unlocks through its app).

## Why Notes' commands live in `icloud-notes`

They could have gone into the sync engine, `icloud-notes-sync`, instead. They did not,
because everything that makes an edit safe is the app's: which notes are
read-only, the save checks, how a title maps to a file in each vault shape,
how the conflict picker rewrites a note, the backups `recover` makes, and the
vault's lock. That is C++ in `notes/src` (`NotesBackend`, `SyncModel`); a
Rust copy would drift. The engine takes the same lock itself for clone,
pull, push, sync and restore (the app hands it the one it holds), and tells the
app what it needs of the vault's state through `vault-info` (and in each
`sync` answer, so a sync costs one engine run).
`icloud-notes <command>` runs that code headless (a `QCoreApplication`; a
`QGuiApplication` on the offscreen platform only for `export-pdf`), the same
way `icloud-photos` puts its commands in the app binary. `icloud-notes-sync` stays the engine, shipped inside the
icloud-notes package and off PATH, and `icloud-notes` is the one command
for notes: it runs the engine for `sync`, `pull`, `push` and `clone` the
way the window does (a refused sign-in is reported to icloud-session), and
for `push --dry-run`, `history [--records]`, `diff` and `restore` becomes
the engine (exec), with the output flags and `--wait` passed on: the first three
only read and take no lock, so they work while the window is open. The engine's other flags are the app's to choose: `clone
--filename-as-title` (the app clones with titles in the first line) and
`pull --defer-renames` are not exposed.

The app runs the engine at `$ICLOUD_NOTES_SYNC_BIN` when set (tests,
development; nothing else is tried then), else
`/usr/lib/icloud-notes/icloud-notes-sync`, else `icloud-notes-sync` on
PATH (a development build).

## Conventions every tool follows

All five tools (`icloud-session`, `icloud-notes`, `icloud-notes-sync`,
`icloud-photos`, `icloud-findmy`) follow the same output contract
(`session/src/output.rs` for the Rust tools; `notes/src/cli.cpp` mirrors it).

### Output modes

| Flag | stdout |
|---|---|
| (none, on a terminal) | human-readable text |
| (none, piped) | the JSON envelope, as `--json` |
| `--json` | the envelope `{"ok":true,"data":…,"summary":"…","meta":{…}?}` |
| `--styled` | human-readable text, even when piped |
| `--quiet` | only `data`, as JSON: the bare result. Use this when you want the result and nothing else |
| `--ids-only` | one id per line (what an id is depends on the command, see below) |
| `--count` | one number: how many results |

- The flags are global (before or after the command) and may be combined;
  the strongest wins: `--ids-only` > `--count` > `--quiet` > `--json` >
  `--styled`.
- `data` is the command's result; the [JSON shapes](#json-shapes) below
  describe `data`. `summary` is one line for people ("6 items", "Created
  Notes/Packing.md"). `meta` is optional extra context (`icloud-notes-sync`:
  `meta.lines`, the text report of clone, pull and push, and `meta.text`,
  diff's text).
- In a machine mode (everything but text) stdout is exactly one JSON
  document (or the id lines, or the number), pretty-printed over several
  lines: parse the whole of stdout, not a line. Progress (`[1/2] sending …`,
  `icloud-notes:progress:…`), status lines and warnings go to stderr.
- `ok` comes first; any other key order is not part of the contract
  (`icloud-notes` sorts the keys inside `data`).
- `--ids-only` on a command without ids prints the result as `--quiet`
  would, plus `note: this command has no ids to list; printing its result
  instead` on stderr. `--count` on a command without a list counts 1 for an
  object (an array: its length).
- `--version` prints `TOOL VERSION` as plain text in every mode;
  `icloud-session skill` (no action) prints the skill's Markdown as it is in
  every mode.

**`ok:true` with a non-zero exit.** The envelope's `ok` says the command
produced its result; the exit code says whether everything succeeded. A
batch that partly failed, or a sync that ran but failed, prints its data
and exits non-zero:

- `icloud-photos download`/`delete`/`upload`: exit 1 (or 3 if the sign-in
  lapsed mid-upload) when some items failed; each failed item carries its
  own `error`.
- `icloud-notes sync`/`pull`/`push`/`clone`: their data gains `ok:false`
  and `error:{code, message, hint?}` and the process exits with that
  failure's code (offline: 5). A change's `--push` puts the sync's result,
  with its `error`, in `data.sync`, and exits with the sync's code.
- `icloud-notes-sync sync`: a failed half is `{ok:false, exit_code, error}`
  inside `data`; the exit is the worse half's.
- `status` in `icloud-session`, `icloud-photos` and `icloud-notes` prints
  the status and exits 3 when signed out.

### Errors

A command that could not produce its result prints only an error:

- machine modes (`--json`, piped, `--quiet`, `--ids-only`, `--count`): one
  JSON document on **stdout**, stderr empty:

  ```json
  {
    "ok": false,
    "error": {
      "code": "sign_in_required",
      "message": "sign in to iCloud required",
      "hint": "run `icloud-session sign-in` (a person signs in in the window)"
    }
  }
  ```

- text mode: `error: <message>` and `hint: <hint>` on **stderr**, stdout
  empty.

`code` is stable and machine-readable: branch on it together with the exit
code. `message` says what happened and `hint` (optional) how to fix it; both
are for people. Remediation commands (`run icloud-session sign-in`) are in
`hint`, never in `message`. A usage error carries the hint ``run `TOOL
--help` ``.

### Other conventions

- **`--help`** on every command (`TOOL COMMAND --help`), with the command's
  JSON shape (of `data`) where it has one; `TOOL help` or `TOOL --help`
  lists them all, with the output modes and the exit codes.
- **Nothing asks unless it is in text mode with stdin and stdout on a
  terminal.** Destructive commands (`icloud-notes delete`, `delete-folder`,
  `restore`; `icloud-photos delete`; `icloud-findmy play-sound`,
  `lost-mode`) ask there; anywhere else they refuse with `usage` (exit 2)
  unless `--yes` is given. (`icloud-session set-password` reads the password
  from a terminal without echo, or from stdin.)
- **Mock and scratch modes** for tests: `ICLOUD_SESSION_MOCK=1` with
  `ICLOUD_SESSION_MOCK_URL` (no D-Bus; photos and findmy talk to a fake
  Apple server, `examples/` in each crate), `icloud-photos --data-dir DIR`,
  `icloud-findmy --data-dir DIR`, `icloud-notes --vault DIR` (or
  `ICLOUD_NOTES_VAULT`).

### `--ids-only` and `--count` per command

| Tool | `--ids-only` | `--count` |
|---|---|---|
| icloud-session | `validate`: the webservice keys, sorted; `skill install`/`remove`: the paths | the same, counted |
| icloud-notes | `list`, `search`: note paths; `read`: its path; `folders`: folder paths (not the vault root `""`); a change: the path it touched (`export-pdf`: the PDF); `history`, `push --dry-run`, `diff`, `restore`: the engine's | `list`, `search`: notes; `folders`: folders (not the root); `status`: notes; engine commands: the engine's |
| icloud-notes-sync | `status`, `push` (and `--dry-run`): entry files; `restore`: the file; `history`: epoch or snapshot ids | `status`, `push`: entries; `pull`: changes; `history`: rows; `vault-info`: tracked notes |
| icloud-photos | `albums`: album ids; `list`, `info`, `thumb`, `open`: item ids; `download`, `delete`: the ids that succeeded; `upload`: the new asset ids (duplicates included) | the same ids, counted; `prune-cache`: files removed |
| icloud-findmy | `devices`, `locate`, `play-sound`, `lost-mode`: device ids | `devices`: devices; `history`: positions; `prune-history`: positions deleted |

### Exit codes

The shared table. The JSON `code` tells failures with the same exit apart.

| Exit | Meaning | What to do |
|---|---|---|
| 0 | ok. `push --dry-run`, `diff` and the engine's `status` report their changes in the result and exit 0 | |
| 1 | other failure (a partly failed batch, a refused edit, a busy vault, an HTTP 4xx, …) | read `error.code` (or the items' `error`) |
| 2 | invalid usage or request: bad arguments, an ambiguous name, something that already exists, a destructive command without `--yes` and nobody to ask | fix the call |
| 3 | not signed in, credentials rejected, or not set up: `sign_in_required`, `session_rejected` → `icloud-session sign-in`; `find_my_auth_required` → `icloud-session authorize-find-my`; `password_rejected`; `*_unavailable` (the service is off for this Apple ID); `not_cloned` | a person acts; see `error.code` and `hint` |
| 4 | not found | check the name or id |
| 5 | a service or a required tool unavailable: no network (`offline`, `network`), icloud-sessiond unreachable (`session_service`), a locked keyring, iCloud failing (HTTP 5xx, `bad_response`), the sync engine or icloud-session missing | retry later, or fix what `hint` names |

### Error codes

Every code each tool can emit, by exit.

**icloud-session**

| Exit | Codes |
|---|---|
| 1 | `error` (icloud-sessiond answered with a failure), `io`, `http` (a 4xx), `skill_unmanaged` (a path `skill install` did not write is in the way) |
| 2 | `usage` |
| 3 | `sign_in_required`, `sign_in_not_completed` (the window closed and you are still signed out), `find_my_auth_required`, `find_my_auth_not_completed`, `password_rejected` (Apple refused the password given to `set-password`), `session_rejected` (Apple keeps answering 401/421 although icloud-sessiond still holds the session: sign in again); also `status` while signed out (the status is printed) |
| 5 | `session_service` (icloud-sessiond unreachable), `keyring_unavailable` (signed in, but the keyring holding the session is locked or missing: unlock it), `network`, `offline` (no network: a name that does not resolve, a refused or timed-out connect), `bad_response` (Apple answered with something unreadable), `http` (a 5xx) |

**icloud-notes**

| Exit | Codes |
|---|---|
| 1 | `read_only`, `has_attachments`, `guardrail` (the text would add conflict markers or another note's id; `--force` overrides), `not_a_list_item`, `no_conflicts`, `conflicts_unreadable`, `no_synced_copy`, `vault_busy` (the Notes window, or another sync, holds the vault), `vault_lock`, `sync_failed` (a sync, pull, push or clone failed without an error from the engine: see the log; when the engine reports an error, that code and its exit are passed through, e.g. `account_mismatch` 3, `cloudkit_request_failed` 5), `cancelled`, `error` |
| 2 | `usage`, `ambiguous` (a title several notes have: use the path), `exists`, `already_cloned`, `choices_mismatch` |
| 3 | `sign_in_required`, `not_cloned` (run `icloud-notes clone`); also `status` while signed out (the status is printed; with icloud-session unreachable the sign-in is unknown, `sign_in_known:false`, and `status` exits 0) |
| 4 | `not_found` (no such note or folder) |
| 5 | `offline`, `network` (iCloud out of reach: retry later), `session_service` (icloud-sessiond unreachable), `sync_tool_missing` (the engine is missing) |

`push --dry-run`, `history`, `diff` and `restore` are the engine itself, so
they exit with the engine's codes, below.

**icloud-notes-sync** (`code` is the error's class in snake case)

| Exit | Codes |
|---|---|
| 1 | `internal` (a bug or an unexpected answer), `io` (a file or disk failure), `error` (icloud-sessiond answered with a failure), `vault_busy`, `vault_lock`, `vault_needs_update` (a read-only command on a layout 2 vault: a sync updates it), `vault_from_newer_tool`, `unsupported_vault_layout`, `corrupt_state_file`, `version_content_unavailable`, `cloudkit_request_failed` for an HTTP 4xx (iCloud refused the request) |
| 2 | `usage`, `ambiguous_tracked_file`, `already_cloned_directory` |
| 3 | `sign_in_required`, `session_rejected` (both: sign in again), `not_cloned_directory`, `unbound_account`, `account_mismatch`, `requested_account_mismatch`, `notes_unavailable` (Notes is not on for this Apple ID) |
| 4 | `untracked_file`, `unknown_version_snapshot`, `cloudkit_zone_fetch_failed` for a zone that no longer exists (`ZONE_NOT_FOUND`) |
| 5 | `offline` (no network at all), `network` (a connection that failed), `session_service`, `keyring_unavailable`, `bad_response`, `cloudkit_request_failed` (an HTTP 5xx or a transport failure), `cloudkit_zone_fetch_failed` |

**icloud-photos**

| Exit | Codes |
|---|---|
| 1 | `cancelled`, `io`, `error`, `http` (a 4xx); a batch with failed items (the data lists them) |
| 2 | `usage` (also `delete` without `--yes` when nobody can be asked) |
| 3 | `sign_in_required`, `photos_unavailable` (iCloud Photos is not on for this Apple ID), `session_rejected`; also `status` while signed out (the status is printed) |
| 4 | `not_found` (no such item or album in the catalog: `sync` first) |
| 5 | `offline`, `network`, `session_service`, `keyring_unavailable`, `bad_response`, `http` (a 5xx) |

**icloud-findmy**

| Exit | Codes |
|---|---|
| 1 | `unsupported`, `no_fix` (no fresh fix in time), `cancelled`, `io`, `error`, `http` (a 4xx) |
| 2 | `usage` (also `play-sound`/`lost-mode` without `--yes` when nobody can be asked), `ambiguous` (the hint lists the matches: use the ID) |
| 3 | `sign_in_required`, `find_my_auth_required` (`icloud-session authorize-find-my`, or `set-password` once), `find_my_unavailable` (Find My is not on for this Apple ID), `session_rejected` |
| 4 | `not_found` (no device matches; the hint lists them) |
| 5 | `offline`, `network`, `session_service`, `keyring_unavailable`, `bad_response`, `http` (a 5xx) |

In mock mode (`ICLOUD_SESSION_MOCK=1`) there is no daemon to confirm a
session, so a 401/421 is `sign_in_required`, not `session_rejected`.

## JSON shapes

These are the `data` of the envelope (what `--quiet` prints). Stable:
fields are added, never renamed or removed without a note here.

### icloud-session

`status`, `sign-in`, `authorize-find-my`, `forget-password`,
`set-password`, `sign-out` (`sign-in`/`authorize-find-my` print it once the
window closes, or at once with `--no-wait`; when the window closes without
success they print only the error):

```json
{"signed_in":true,"apple_id":"you@example.com","full_name":"Your Name","dsid":"1234567890","expires_at":1793000000,"signing_in":false,"find_my_authorized":true,"find_my_password_stored":false,"sign_out_reason":null}
```

`expires_at` is Unix seconds (null for a sign-in without "Keep me signed
in"); `sign_out_reason` says why the account was last signed out.

| Command | `data` |
|---|---|
| `validate` | `{dsid, apple_id, webservices:{"findme":"https://...",...}}` (keys sorted) |
| `skill install`, `skill remove` | `{paths:[path], notices:[text]}` |
| `skill` | not JSON: the Markdown |

### icloud-notes

NOTE is a vault path (`Notes/Groceries.md`, `.md` optional), an
apple-note-id, or a title exactly one note has (ignoring case).

| Command | `data` |
|---|---|
| `status` | `{vault, cloned, title_mode, sync_tool, sign_in_known, signed_in, apple_id, sign_in_days_left, lock:{held_by, app_open}, notes, folders, flagged:{FLAG:[path]}}` |
| `folders` | `[{folder, name, notes, default}]` (`folder` `""` is the vault root, All Notes; counts include descendants) |
| `list` | `[{path, folder, file, title, snippet, modified, modified_ms, id, flags}]` (`--folder` includes descendants, matching the window and folder counts) |
| `read` | `{path, folder, file, title, id, modified, modified_ms, flags, read_only, body, text?, attachments:[{name, path, image}], conflicts:[{local:[line], remote:[line], before:[line], after:[line]}], conflicts_unreadable, has_synced_copy}` |
| `search` | `[{path, folder, file, title, snippet}]` |
| `new` | `{action:"new", path, folder, file, title, sync?}` |
| `write` | `{action:"write", path, changed, sync?}` |
| `rename`, `move` | `{action, path, from, title?, sync?}` |
| `delete` | `{action:"delete", path, sync?}` |
| `toggle` | `{action:"toggle", path, line, text, sync?}` |
| `resolve` | `{action:"resolve", path, choices, sync?}` |
| `recover` | `{action:"recover", path, how, backup, message, sync?}` |
| `export-pdf` | `{action:"export-pdf", path, pdf}` |
| `new-folder`, `rename-folder`, `delete-folder` | `{action, folder, from?, sync?}` |
| `sync`, `pull`, `push`, `clone` | `{ok, runs:[{command, ok, exit_code, result}], log, error?}`; `result` is icloud-notes-sync's data for that run, or `{error:{code, message, hint?}}` when it failed; `error` (with `ok:false`) says why the whole command failed, and the exit code is that failure's. `sync` is one engine run (`icloud-notes-sync sync`) reported as two runs, `push` then `pull` (no `pull` after a refused sign-in) |
| `sync` under a change's `--push` | the same object as `sync?`, or `{ok:false, error}` when the sync could not start (signed out, icloud-session missing, …) |
| `push --dry-run`, `history [--records]`, `diff`, `restore` | icloud-notes-sync's own output (below) |

`flags`: `new` (not synced yet), `conflict`, `read-only`, `missing-id`,
`foreign-id`, `tables`. `modified` is UTC ISO 8601.

### icloud-notes-sync (the engine, inside icloud-notes)

| Command | `data` |
|---|---|
| `status` | `{entries:[{kind, file, resolution, reason?, ...}], unchanged, notices}` |
| `push` (and `--dry-run`) | `{dryRun, pushed?, entries:[{kind, file, resolution, outcome?, ...}], unchanged, notices}` |
| `pull` | `{added, updated, merged, removed, changes:[{kind, file}], conflicts, notices, ...}` |
| `clone` | `{written, writtenShared, attachmentsDownloaded, notices, ...}` |
| `history` | `{mode:"epochs", epochs:[{id, timestamp, changed, carriedOver}]}`; with `--records`, `{mode:"records", records:[...]}` |
| `diff` | `{kind, from, to, hasDifferences, text}` (`text` also in `meta.text`) |
| `restore` | `{file}` |
| `sync` | `{push, pull, vault_info}` (below) |
| `vault-info` | `{vault, cloned, stateDir, stateFile, titleMode, defaultFolderDir, notes, lockPath}` |

See `/usr/lib/icloud-notes/icloud-notes-sync COMMAND --help`. `sync` (push,
then pull, in one run: what the app runs) prints `{push, pull, vault_info}`
even when a half failed: each half is `{ok, exit_code, result, lines}`
(`result` the command's own data, `lines` its human report) or `{ok,
exit_code, error}`; `pull` is null when it did not run (the push found the
sign-in gone or refused, or iCloud out of reach), and `vault_info` is what
`vault-info` would print after it. It exits 3 if either half exited 3, else
with the worse half's code. `stateDir` is the engine's state directory,
`.icloud-notes` (or a layout 3 vault's `.icloud-md`, until a command that
takes the lock moves it; see notes-sync/docs/DESIGN.md §1). In a machine
mode progress goes to stderr as `icloud-notes:progress:...` lines (older
engines: `icloud-md:progress:...`).

### icloud-photos

| Command | `data` |
|---|---|
| `status` | `{signed_in, sign_in_error, mock, catalog, catalog_exists, assets, albums, downloaded, last_sync, last_sync_unix, incremental_sync_ready, library_dir, cache_dir, settings, download_mode}` |
| `sync` | `{mode:"full"\|"incremental", assets, removed, albums, fell_back}` |
| `albums` | `[{id, name, count}]` |
| `list` | `[{id, filename, created, created_unix, kind:"photo"\|"video"\|"live", size, width, height, local_path, live_path}]` |
| `info` | a `list` row plus `{master_id, deleted, change_tag, original_type, live_type, thumb_path, medium_path, albums:[{id, name}]}` |
| `thumb` | `{id, path, cache_path}` |
| `download` | `[{id, path, live_path, live_error} \| {id, error}]`, one per id asked for, in order |
| `open` | `{id, path, opened}` |
| `upload` | `{files:[{file, asset_id, master_id, duplicate} \| {file, error}], uploaded, duplicates, failed, album, album_error, sync, not_in_catalog}` |
| `delete` | `[{id, deleted, error?}]` |
| `prune-cache` | `{removed}` |
| `sign-in` | `{started, mock}` |
| `config` | `{library_dir, download, file, saved}` |

### icloud-findmy

| Command | `data` |
|---|---|
| `devices`, `locate` | `[{id, name, model, class, battery_percent, charging, online, lost_mode, can_play_sound, can_lost_mode, last_fix:{time, timestamp_ms, age_secs, accuracy_m, is_old, lat?, lon?} \| null}]` (`last_fix` is `null` for a device with no fix; `locate`: one object, with coordinates) |
| `play-sound`, `lost-mode` | `{action:"play_sound"\|"lost_mode", device:{id, name}}` |
| `history` | `{device:{id, name}, since, points:[{time, timestamp, lat, lon, accuracy_m, battery_percent}]}` |
| `prune-history` | `{deleted, remaining, retention_days}` |

### Contacts and tasks

These APIs moved to [Cloud Contacts](https://github.com/ferdousbhai/cloud-contacts)
and [Cloud Tasks](https://github.com/ferdousbhai/cloud-tasks). The standalone
apps/CLIs are retired; see those products for CLI contracts and cache-preserving
migration.
