# Driving the iCloud apps as an agent

A compact reference for an AI agent (or a script) using iCloud Notes,
Photos, Find My and Reminders on this computer through their command lines. The full
audit, every JSON shape and every error code are in [CLI.md](CLI.md).

## The rules that always hold

- Pass `--json` to every command. stdout is then only the JSON result.
  On failure, the last line of stderr is
  `{"error":{"code":"...","message":"...","exit_code":N,"hint":"..."}}`;
  branch on `code` and the exit status, show `message`/`hint` to the user.
- Exit codes, the same in every tool:

  | Exit | Meaning | What to do |
  |---|---|---|
  | 0 | ok | |
  | 1 | error | read `error.code` |
  | 2 | iCloud sign-in required | ask the user to run `icloud-session sign-in` (see Auth) |
  | 3 | has changes / differences (`push --dry-run`, `diff`) | not an error |
  | 4 | Find My needs the Apple password | ask the user to run `icloud-session authorize-find-my` |
  | 64 | usage: bad arguments, or a destructive command without `--yes` | fix the call |

- Nothing prompts when stdin is not a terminal. Destructive commands refuse
  (exit 64) unless given `--yes`: `icloud-notes delete | delete-folder |
  restore`, `icloud-photos delete`, `icloud-findmy play-sound | lost-mode`,
  `icloud-reminders delete`. Pass `--yes` only when the user asked for that
  exact action.
- `TOOL COMMAND --help` describes any command and its JSON.

## Auth: icloud-session owns the sign-in

One Apple account per computer, held by the `icloud-session` daemon; every
tool takes its session from it and none signs in by itself.

```console
$ icloud-session status
{"signed_in":true,"apple_id":"you@example.com","dsid":"1234567890","expires_at":1793000000,"signing_in":false,"find_my_authorized":true,"find_my_password_stored":false}
```

Needs a human, always:

- **Signing in**: `icloud-session sign-in` opens Apple's own page in a
  window; the person types the password and the two-factor code there
  (ticking "Keep me signed in", and "Trust", makes it last about 30 days).
  The command waits for the window and exits 0 once signed in, 2 if it was
  closed without. `--no-wait` returns at once; poll `icloud-session status`
  until `signing_in` is false.
- **Find My's password page** (exit 4 from `icloud-findmy`):
  `icloud-session authorize-find-my`, the same way. Once the user has run
  `icloud-session set-password` (it reads the password from the terminal,
  or from stdin: `bw get password "Apple ID" | icloud-session set-password`,
  `op read "op://Private/Apple ID/password" | icloud-session set-password`,
  and stores it in the keyring), the daemon answers that page itself and
  exit 4 stops happening.

`expires_at` (Unix seconds) says when the sign-in lapses; warn the user a
few days ahead.

## Notes: `icloud-notes`

Notes are Markdown files in `~/Documents/icloud-notes`, one folder per
Notes folder, synced with iCloud. Always go through `icloud-notes`, never
edit the files or run the sync engine (`icloud-notes-sync`, inside the
icloud-notes package, off PATH) yourself: `icloud-notes` applies the app's
rules, takes the vault's lock, and reaches every engine command you need.

- **NOTE** is a path in the vault (`Notes/Groceries.md`, `.md` optional),
  an apple-note-id, or a title exactly one note has (`ambiguous` otherwise:
  use the path).
- **The lock.** Commands that change the vault or sync take the lock the
  Notes window holds while it is open. If Notes is open they fail at once
  with `vault_busy` (exit 1): tell the user to make the change in Notes or
  quit it. A background sync holding it is waited for (30 s; `--wait SECS`).
  Reading (`status`, `folders`, `list`, `read`, `search`, and `history`,
  `diff`, `push --dry-run`) never needs it.
- **Changes stay local until a sync.** Add `--push` to a change to sync
  right after it (push, then pull), or run `icloud-notes sync`. Before a
  sync you did not make yourself, preview with `icloud-notes push --dry-run`
  (exit 3 means there is something to push; `entries[].resolution:
  "refused"` says what will not go and why). A sync without a network
  fails with `offline` (or `network`), exit 1: retry once it is back.
- A deleted note goes to the trash and, on push, to Recently Deleted in
  iCloud (recoverable there for about 30 days).
- Read-only notes (`read_only` set: very large notes, attachments Apple
  keeps elsewhere) cannot be written, renamed or moved: say so.

```console
$ icloud-notes --json list --folder Notes
[{"path":"Notes/Groceries.md","folder":"Notes","file":"Groceries.md","title":"Groceries","snippet":"bread",
  "modified":"2026-09-30T08:12:00Z","modified_ms":1790755920000,"id":"8C1F...","flags":[]}]
$ icloud-notes --json read Groceries
{"path":"Notes/Groceries.md","title":"Groceries","id":"8C1F...","body":"# Groceries\nbread\n","read_only":null,
 "attachments":[],"conflicts":[],"conflicts_unreadable":false,"flags":[],...}
$ icloud-notes --json new "Packing" --body "- [ ] passport" --push
{"action":"new","path":"Notes/Packing.md","folder":"Notes","file":"Packing.md","title":"Packing",
 "sync":{"ok":true,"runs":[{"command":"push","ok":true,"exit_code":0,"result":{...}},{"command":"pull",...}],"log":"..."}}
```

| Command | Does |
|---|---|
| `status` | vault, sign-in, lock holder, notes flagged `conflict`/`new`/`read-only`/... (exit 2 signed out) |
| `folders`, `list [--folder F] [--flag FLAG]`, `read NOTE [--raw]`, `search QUERY` | read |
| `new TITLE [--folder F] [--body T \| --file P \| --stdin]` | create (default folder: the account's "Notes"; the body goes below the title line) |
| `write NOTE (--body T \| --file P \| --stdin) [--append] [--force]` | replace the text `read` shows (its first line is the title), or append to it |
| `rename NOTE TITLE`, `move NOTE FOLDER`, `delete NOTE --yes`, `toggle NOTE LINE` | as in the window (LINE counts from 1 in `read`'s body) |
| `resolve NOTE --all local\|remote\|both` / `--choices C1,C2,...` | settle conflict blocks (`local` = this computer, `remote` = iCloud) |
| `recover NOTE --strip \| --synced` | unreadable conflict markers (`conflicts_unreadable`): keep every line, or the last synced text; backs up first |
| `new-folder NAME [--in F]`, `rename-folder F NAME`, `delete-folder F --yes` | folders |
| `sync`, `pull`, `push [--dry-run]`, `clone` | talk to iCloud (exit 2: sign-in) |
| `history NOTE [--records]`, `diff NOTE REF`, `restore NOTE --yes`, `export-pdf NOTE` | versions, discard local edits, PDF next to the note |

`write` refuses (`guardrail`) text that adds conflict markers or another
note's id; `--force` overrides, but prefer `resolve`. Conflicts: `read
--json` lists each block's `local` and `remote` lines; ask the user which
to keep when it is not obvious.

## Photos: `icloud-photos`

A local catalog of the iCloud library; `sync` refreshes it.

```console
$ icloud-photos --json albums
[{"id":"A1B2...","name":"Holiday","count":42}]
$ icloud-photos --json list --album A1B2... --limit 2
[{"id":"AQx...","filename":"IMG_0001.HEIC","created":"2026-08-01T10:00:00Z","kind":"live","size":2048576,
  "width":4032,"height":3024,"local_path":null,"live_path":null}]
$ icloud-photos --json download AQx... --out ~/Downloads/holiday
[{"id":"AQx...","path":"/home/me/Downloads/holiday/2026/08/IMG_0001.HEIC","live_path":"...MOV","live_error":null}]
```

| Command | Does |
|---|---|
| `status` (exit 2 signed out), `sync [--full]` | state; refresh the catalog |
| `albums`, `list [--album ID] [--since DATE] [--kind photo\|video\|live] [--limit N]`, `info ID` | read the catalog |
| `thumb ID [--out P]`, `download ID... \| --all [--medium] [--out DIR]`, `open ID` | fetch (originals go to `~/Pictures/icloud-photos` unless `--out`) |
| `upload FILE... [--album ID] [--no-sync]` | upload, then (unless `--no-sync`) sync until the new ids are in the catalog |
| `delete ID... --yes` | to Recently Deleted on every device |
| `config [--library-dir DIR] [--download on-demand\|all]`, `prune-cache` | preferences, cache |

`download` and `delete` exit 1 when some items failed; each failed item
carries its own `error`.

## Find My: `icloud-findmy`

```console
$ icloud-findmy --json devices
[{"id":"aVBo...","name":"Dous's iPhone","model":"iPhone 15 Pro","class":"iphone","battery_percent":81,
  "charging":false,"online":true,"lost_mode":false,"can_play_sound":true,"can_lost_mode":true,
  "last_fix":{"time":"2026-09-30T09:58:02Z","timestamp_ms":1790762282000,"age_secs":95,"accuracy_m":12.0,"is_old":false}}]
$ icloud-findmy --json locate "iphone" --wait 30
{"id":"aVBo...","name":"Dous's iPhone",...,"last_fix":{...,"lat":60.17,"lon":24.94}}
$ icloud-findmy --json play-sound "iphone" --yes
{"ok":true,"action":"play_sound","device":{"id":"aVBo...","name":"Dous's iPhone"}}
```

| Command | Does |
|---|---|
| `devices [--locate] [--coords]` | every device; `--locate` wakes them for fresh fixes; coordinates only with `--coords` |
| `locate NAME\|ID [--wait SECS]` | one fresh fix (`no_fix` if none in time) |
| `play-sound NAME\|ID --yes`, `lost-mode NAME\|ID --phone P --message M --yes` | act on a device |
| `history NAME\|ID [--since 24h]`, `prune-history` | the stored trail |

NAME matches case-insensitively, a unique part is enough (`ambiguous`
lists the candidates: use the ID). Locations are personal: print
coordinates only when the user asked for them. Lost Mode locks the device;
never turn it on without an explicit request.

## Reminders: `icloud-reminders`

iCloud Reminders (the CloudKit ones every iPhone has used since iOS 13).
Reading commands sync first (`--cached` skips that; offline they fall back
to the cache with a warning on stderr). Changes go straight to iCloud:
without a network they fail with `offline`, nothing is queued.

```console
$ icloud-reminders --json lists
[{"id":"6F0A...","name":"Groceries","color":"#FF9500","open":2}]
$ icloud-reminders --json list
[{"id":"9B1C...","list":{"id":"6F0A...","name":"Groceries"},"title":"Milk","notes":"Oat","completed":false,
  "completed_at":null,"due":{"date":"2026-10-09","time":"17:30","all_day":false,"time_zone":"Europe/Helsinki",
  "at":"2026-10-09T14:30:00Z"},"flagged":false,"priority":0,"alerts":0}]
$ icloud-reminders --json add "Call the dentist" --list Reminders --due "tomorrow 9:00"
{"action":"add","reminder":{"id":"...","title":"Call the dentist",...}}
$ icloud-reminders --json complete "dentist"
{"action":"complete","reminder":{...,"completed":true,"completed_at":"2026-10-09T11:02:41Z"}}
```

| Command | Does |
|---|---|
| `lists`, `list [LIST] [--completed \| --all]`, `show REMINDER` | read (open reminders, soonest due first) |
| `add TITLE [--list LIST] [--notes T] [--due WHEN]` | create (default list: the one named Reminders) |
| `edit REMINDER [--title T] [--notes T] [--due WHEN \| --no-due]` | change |
| `complete REMINDER`, `uncomplete REMINDER`, `delete REMINDER --yes` | as in the window (a delete goes to Recently Deleted) |
| `sync [--full]` | fetch what changed |
| `background` | what the systemd timer runs every minute: sync if stale, notify about due ones |

REMINDER is an ID, an exact title, or a unique part of one (open
reminders first; `ambiguous` lists the candidates). WHEN: `2026-10-10`
(all day), `"2026-10-10 17:30"`, `today`/`tomorrow` with an optional time,
`17:30`, or `+30m`/`+2h`/`+3d`. `due.at` is the moment it is due, in UTC.
`alerts` counts alerts set on an Apple device: they keep their own time
when `--due` changes, so say so if it is not 0. Due reminders notify on
their own (the timer); never run `background` to notify on purpose.

## Recipes

**Sync notes and read one**

```bash
icloud-notes --json sync                       # exit 2: ask the user to sign in
icloud-notes --json search "dentist"           # find it
icloud-notes --json read "Notes/Dentist.md"    # .body is the text
```

**Add a note and push it**

```bash
icloud-notes --json new "Meeting 30 Sep" --folder Work --body "- agenda" --push
# exit 1 with error.code "vault_busy": Notes is open; ask the user to quit it or add it there
# exit 2: written locally; after the user signs in: icloud-notes --json sync
```

**Append to a checklist and tick an item**

```bash
icloud-notes --json write Groceries --append --body "- [ ] jam"
icloud-notes --json read Groceries             # find the line number in .body (first line is 1)
icloud-notes --json toggle Groceries 3 --push
```

**Resolve a conflict**

```bash
icloud-notes --json list --flag conflict
icloud-notes --json read "Work/Plan.md"        # .conflicts[i].local / .remote
icloud-notes --json resolve "Work/Plan.md" --choices local,both --push
# .conflicts_unreadable true instead: icloud-notes recover "Work/Plan.md" --strip
```

**Download the photos of an album**

```bash
icloud-photos --json sync
icloud-photos --json albums                    # pick the album's id
icloud-photos --json list --album "$ID" | jq -r '.[].id' | xargs icloud-photos --json download --out ~/Downloads/album
```

**Locate a device and play a sound**

```bash
icloud-findmy --json locate "iPhone" --wait 30      # exit 4: ask for icloud-session authorize-find-my
icloud-findmy --json play-sound "iPhone" --yes
```

**Remind the user of something**

```bash
icloud-reminders --json add "Renew passport" --due "2026-11-01 09:00"   # notifies here, and on their iPhone if it has an alert
icloud-reminders --json list --all | jq '.[] | select(.title | test("passport"; "i"))'
```

## Testing without the real account

`ICLOUD_SESSION_MOCK=1 ICLOUD_SESSION_MOCK_URL=http://127.0.0.1:PORT`
points photos, findmy and reminders at a local fake server (`cargo run -p
icloud-photos --example fake_cloudkit -- --port PORT`, `cargo run -p
icloud-findmy --example fake_findme`, `cargo run -p icloud-reminders
--example fake_reminders -- 127.0.0.1:PORT`; their READMEs say more);
`--data-dir DIR` keeps their catalog, cache and history there; `icloud-notes --vault DIR` works on a scratch vault.
