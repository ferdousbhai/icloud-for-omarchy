# The command line: every GUI feature, and how the tools agree

Every iCloud app here can be driven from a terminal or by an agent, with no
window and no display. This page maps each thing a person can do in a window
to the command that does it, then fixes the conventions all the tools share.
[AGENTS.md](AGENTS.md) is the short version for an agent.

| Tool | Package | What it drives |
|---|---|---|
| `icloud-session` | icloud-session | the one Apple sign-in every app uses (D-Bus daemon, sign-in window) |
| `icloud-notes <command>` | icloud-notes | the Notes app: the vault `~/Documents/icloud-notes`, its rules and its lock |
| (`icloud-notes-sync`) | icloud-notes | the sync engine under Notes, installed off PATH at `/usr/lib/icloud-notes/icloud-notes-sync`; reached through `icloud-notes`, run directly only for development |
| `icloud-photos <command>` | icloud-photos | the Photos app: catalog, downloads, uploads, deletes |
| `icloud-findmy <command>` | icloud-findmy | the Find My app: devices, locate, sound, Lost Mode, history |

`icloud-notes`, `icloud-photos` and `icloud-findmy` open their window when
run with no arguments, and run a command without GTK/Qt GUI initialization
when given one.

## The audit: GUI feature → command

"Added" marks a gap this audit found and filled; everything else already
existed. The GUI code audited: `notes/qml/main.qml` and
`notes/src/notesbackend.*`, `photos/src/ui/`, `findmy/src/ui/`, and the
sign-in flows in `sessiond/src/bin/signin.rs` and `sessiond/src/daemon.rs`.

### Notes (`notes/qml`, `notes/src`)

Notes had no command line beyond `icloud-notes --sync` (the background
timer's sync). All of it is new, in the app binary: see
[Why in `icloud-notes`](#why-notes-commands-live-in-icloud-notes).

| In the window | Command | |
|---|---|---|
| Folder list with note counts | `icloud-notes folders` | added |
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
| Automatic sync (launch, focus, every minute) | `icloud-notes sync` (push, then pull) | added |
| Pull | `icloud-notes pull` | added (the engine's `pull` existed) |
| Push… preview, then Push now; Status preview | `icloud-notes push --dry-run`, then `icloud-notes push` | added |
| Note history, diffs | `icloud-notes history NOTE [--records]`, `icloud-notes diff NOTE REF` | added (the engine's existed) |
| Discard local edits (the History dialog points to a terminal) | `icloud-notes restore NOTE --yes` | added (the engine's `restore` existed) |
| Link your Apple Notes: Clone my notes | `icloud-notes clone` | added |
| Sign-in banner, days left, Sign in | `icloud-notes status` (`signed_in`, `sign_in_days_left`), then `icloud-session sign-in` | added |
| Sync log | each command's output; `sync --json` carries `log` | added |
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
| Sign-in banner | `icloud-photos status` (exits 2 signed out), `icloud-photos sign-in` | existed |
| About | `icloud-photos --version` | existed |
| Cache trimming (automatic) | `icloud-photos prune-cache` | existed |
| Favourites, editing, album management | not in the app either | n/a |

### Find My (`findmy/src/ui`)

| In the window | Command | |
|---|---|---|
| Device list: model, battery, online, Lost Mode | `icloud-findmy devices` | existed |
| Map pins | `icloud-findmy devices --coords` | existed |
| Refresh (Ctrl+R; every 60 s while shown) | `icloud-findmy devices --locate`; the cadence is the caller's | existed |
| Select a device for a fresh fix | `icloud-findmy locate NAME\|ID [--wait SECS]` | existed |
| History trail on the map | `icloud-findmy history NAME\|ID [--since 24h]` | existed |
| Play Sound | `icloud-findmy play-sound NAME\|ID --yes` | existed |
| Lost Mode… (phone, message) | `icloud-findmy lost-mode NAME\|ID --phone P --message M --yes` | existed |
| Sign-in banner | exit 2, then `icloud-session sign-in` | existed |
| Find My password banner (Enter Password) | exit 4, then `icloud-session authorize-find-my` (or `set-password` once) | existed |
| Old positions dropped after 30 days | `icloud-findmy prune-history` | existed |

### Sign-in (`sessiond`)

| In the window / daemon | Command | |
|---|---|---|
| State, expiry | `icloud-session status` | existed |
| Sign in (Apple's page) | `icloud-session sign-in [--no-wait]` | `--no-wait` added |
| Find My's password page | `icloud-session authorize-find-my [--no-wait]` | `--no-wait` added |
| Remember the password for Find My | `icloud-session set-password` (a no-echo prompt, or stdin: `bw get password "Apple ID" \| icloud-session set-password`, `op read "op://…/password" \| icloud-session set-password`) | existed |
| Forget it | `icloud-session forget-password` | existed |
| Sign out | `icloud-session sign-out` | existed |
| Check the session with Apple | `icloud-session validate` | existed |

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
pull, push and restore (the app hands it the one it holds), and tells the
app what it needs of the vault's state through `vault-info`.
`icloud-notes <command>` runs that code headless (a `QCoreApplication`; a
`QGuiApplication` on the offscreen platform only for `export-pdf`), the same
way `icloud-photos` and `icloud-findmy` put their commands in the app
binary. `icloud-notes-sync` stays the engine, shipped inside the
icloud-notes package and off PATH, and `icloud-notes` is the one command
for notes: it runs the engine for `sync`, `pull`, `push` and `clone` the
way the window does (a refused sign-in is reported to icloud-session), and
for `push --dry-run`, `history [--records]`, `diff` and `restore` becomes
the engine (exec), with `--json` and `--wait` passed on: the first three
only read and take no lock, so they work while the window is open. The engine's other flags are the app's to choose: `clone
--filename-as-title` (the app clones with titles in the first line) and
`pull --defer-renames` (icloud-md compatibility) are not exposed.

The app runs the engine at `$ICLOUD_NOTES_SYNC_BIN` when set (tests,
development; nothing else is tried then), else
`/usr/lib/icloud-notes/icloud-notes-sync`, else `icloud-notes-sync` on
PATH (a development build).

## Conventions every tool follows

- **`--json`** on every command: the result as JSON on stdout (stdout
  carries nothing else), progress and warnings on stderr. `icloud-session`
  prints JSON anyway; `--json` there makes its errors JSON too.
- **Errors** go to stderr. With `--json` the last line of stderr is one
  JSON object:

  ```json
  {"error":{"code":"sign_in_required","message":"Not signed in to iCloud.","exit_code":2,"hint":"Run `icloud-session sign-in` ..."}}
  ```

  `code` is stable and machine-readable; `hint` is optional; `message` and
  `hint` are for people.
- **`--help`** on every command (`TOOL COMMAND --help`), with the command's
  JSON shape where it has one; `TOOL help` or `TOOL --help` lists them all.
- **Nothing asks unless stdin is a terminal.** Destructive commands
  (`icloud-notes delete`, `delete-folder`, `restore`; `icloud-photos
  delete`; `icloud-findmy play-sound`, `lost-mode`) ask on a terminal and,
  without one, refuse with a usage error (64) unless `--yes` is given.
- **Mock and scratch modes** for tests: `ICLOUD_SESSION_MOCK=1` with
  `ICLOUD_SESSION_MOCK_URL` (photos, findmy: a fake Apple server),
  `icloud-photos --data-dir DIR`, `icloud-findmy --data-dir DIR`,
  `icloud-notes --vault DIR` (or `ICLOUD_NOTES_VAULT`).

### Exit codes

| Code | Meaning | session | notes | notes-sync (engine) | photos | findmy |
|---|---|---|---|---|---|---|
| 0 | ok | ✓ | ✓ | ✓ | ✓ | ✓ |
| 1 | error (the JSON `code` says which) | ✓ | ✓ | ✓ | ✓ (also: some items of a batch failed) | ✓ |
| 2 | iCloud sign-in required: `icloud-session sign-in` | ✓ (also: the window closed without a sign-in) | ✓ (`status` too, when signed out) | ✓ | ✓ (`status` too, when signed out) | ✓ |
| 3 | has changes / differences (not an error) | | `push --dry-run`, `diff` | `status`, `push --dry-run`, `diff` | | |
| 4 | Find My needs the Apple password: `icloud-session authorize-find-my` | ✓ (also: authorization not completed) | | | | ✓ |
| 64 | usage (bad arguments; a destructive command without `--yes` and no terminal) | ✓ | ✓ | ✓ | ✓ | ✓ |
| 70 | internal error, a bug | | (passed through from the engine) | ✓ | | |

### Error codes

| Tool | `error.code` values |
|---|---|
| all | `usage`, `sign_in_required` |
| icloud-session | `sign_in_not_completed`, `find_my_auth_required`, `find_my_auth_not_completed`, `session_service` (daemon unreachable), `network`, `http`, `io`, `error` |
| icloud-notes | `not_found`, `ambiguous`, `not_cloned`, `already_cloned`, `exists`, `read_only`, `has_attachments`, `guardrail`, `not_a_list_item`, `no_conflicts`, `conflicts_unreadable`, `choices_mismatch` (exit 64), `no_synced_copy`, `vault_busy`, `vault_lock`, `session_unavailable`, `sync_tool_missing`, `sync_failed`, `cancelled`, `error` |
| icloud-notes-sync | the error's class in snake case: `untracked_file`, `not_cloned_directory`, `ambiguous_tracked_file`, `already_cloned_directory`, `account_mismatch`, `unknown_version_snapshot`, `cloudkit_request_failed`, `internal`, ... |
| icloud-photos | `not_found`, `cancelled`, `error` |
| icloud-findmy | `find_my_auth_required`, `not_found`, `ambiguous`, `cancelled`, `unsupported`, `no_fix`, `error` |

## JSON shapes

Stable: fields are added, never renamed or removed without a note here.

### icloud-session

`status`, `sign-in`, `authorize-find-my`, `forget-password`, `sign-out`:

```json
{"signed_in":true,"apple_id":"you@example.com","dsid":"1234567890","expires_at":1793000000,"signing_in":false,"find_my_authorized":true,"find_my_password_stored":false}
```

`validate`: `{"dsid","apple_id","webservices":{"findme":"https://...",...}}`.

### icloud-notes

NOTE is a vault path (`Notes/Groceries.md`, `.md` optional), an
apple-note-id, or a title exactly one note has (ignoring case).

| Command | stdout JSON |
|---|---|
| `status` | `{vault, cloned, title_mode, sync_tool, sign_in_known, signed_in, apple_id, sign_in_days_left, lock:{held_by, app_open}, notes, folders, flagged:{FLAG:[path]}}` |
| `folders` | `[{folder, name, notes, default}]` (`folder` `""` is the vault root, All Notes) |
| `list` | `[{path, folder, file, title, snippet, modified, modified_ms, id, flags}]` |
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
| `sync`, `pull`, `push`, `clone` (and `sync` under `--push`) | `{ok, runs:[{command, ok, exit_code, result}], log}`; `result` is icloud-notes-sync's own JSON for that run |
| `push --dry-run`, `history [--records]`, `diff`, `restore` | icloud-notes-sync's own output |

`flags`: `new` (not synced yet), `conflict`, `read-only`, `missing-id`,
`foreign-id`, `tables`. `modified` is UTC ISO 8601.

### icloud-notes-sync (the engine, inside icloud-notes)

`--json` results are icloud-md's shapes: `status`/`push --dry-run`
`{entries:[{kind, file, resolution, reason?, ...}], unchanged, notices}`,
`history` `{mode:"epochs", epochs:[{id, timestamp, changed, carriedOver}]}`
(with `--records`, `{mode:"records", records:[...]}`), `pull`/`clone`
summaries; see `/usr/lib/icloud-notes/icloud-notes-sync COMMAND --help`.

### icloud-photos

| Command | stdout JSON |
|---|---|
| `status` | `{signed_in, sign_in_error, mock, catalog, catalog_exists, assets, albums, downloaded, last_sync, last_sync_unix, incremental_sync_ready, library_dir, cache_dir, settings, download_mode}` |
| `sync` | `{mode:"full"\|"incremental", assets, removed, albums, fell_back}` |
| `albums` | `[{id, name, count}]` |
| `list` | `[{id, filename, created, created_unix, kind:"photo"\|"video"\|"live", size, width, height, local_path, live_path}]` |
| `info` | a `list` row plus `{master_id, deleted, change_tag, original_type, live_type, thumb_path, medium_path, albums:[{id, name}]}` |
| `thumb` | `{id, path, cache_path}` |
| `download` | `[{id, path, live_path?, live_error?} \| {id, error}]` |
| `open` | `{id, path, opened}` |
| `upload` | `{files:[{file, asset_id, master_id, duplicate} \| {file, error}], uploaded, duplicates, failed, album, album_error, sync, not_in_catalog}`; only `{files}` (exit 1) when every file was skipped |
| `delete` | `[{id, deleted, error?}]` |
| `prune-cache` | `{removed}` |
| `sign-in` | `{started, mock}` |
| `config` | `{library_dir, download, file, saved}` |

### icloud-findmy

| Command | stdout JSON |
|---|---|
| `devices`, `locate` | `[{id, name, model, class, battery_percent, charging, online, lost_mode, can_play_sound, can_lost_mode, last_fix:{time, timestamp_ms, age_secs, accuracy_m, is_old, lat?, lon?} \| null}]` (`last_fix` is `null` for a device with no fix; `locate`: one object, with coordinates) |
| `play-sound`, `lost-mode` | `{ok:true, action:"play_sound"\|"lost_mode", device:{id, name}}` |
| `history` | `{device:{id, name}, since, points:[{time, timestamp, lat, lon, accuracy_m, battery_percent}]}` |
| `prune-history` | `{deleted, remaining, retention_days}` |
