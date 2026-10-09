---
name: icloud
description: Use when the user wants something done in their iCloud from this Linux machine - read, search, create, edit, move or delete Apple Notes, resolve a note's sync conflict, sync notes; list, download, upload or delete iCloud Photos; list devices, locate a device, play a sound on it, turn on Lost Mode or show its location history in Find My; list, add, edit, complete or delete iCloud Reminders; or check or fix the iCloud sign-in. Drives the icloud-notes, icloud-photos, icloud-findmy, icloud-reminders and icloud-session command-line tools (icloud-for-omarchy).
---

# iCloud from the command line

Five tools, one convention. Full reference: `docs/AGENTS.md` and
`docs/CLI.md` in ferdousbhai/icloud-for-omarchy; `TOOL COMMAND --help`
describes any command and its JSON.

## Always

- Pass `--json`. stdout = the result only. On failure the last stderr line
  is `{"error":{"code","message","exit_code","hint"}}`: branch on `code`.
- Exit: 0 ok · 1 error · 2 sign-in required · 3 has changes/differences
  (not an error) · 4 Find My needs the Apple password · 64 usage.
- Nothing prompts without a terminal. Destructive commands need `--yes`
  (else exit 64): `icloud-notes delete|delete-folder|restore`,
  `icloud-photos delete`, `icloud-findmy play-sound|lost-mode`,
  `icloud-reminders delete`. Use `--yes` only for the exact action the user
  asked for.

## Auth (needs the human)

`icloud-session status --json` → `signed_in`, `apple_id`, `expires_at`,
`find_my_authorized`. Exit 2 anywhere → ask the user to run
`icloud-session sign-in` (Apple's page in a window: password + 2FA; tick
"Keep me signed in"). Exit 4 from Find My → `icloud-session
authorize-find-my`, or once `icloud-session set-password` so it stops
asking (it prompts on a terminal, or reads stdin: `bw get password "Apple
ID" | icloud-session set-password`, or `op read "op://…/password" |
icloud-session set-password`). Never try to sign in yourself.

## Notes: `icloud-notes` (vault `~/Documents/icloud-notes`)

Go through `icloud-notes`, not the files or its sync engine (`icloud-notes-sync`,
off PATH). NOTE = vault
path (`Notes/Groceries.md`), apple-note-id, or a unique title.

- Read: `status`, `folders`, `list [--folder F] [--flag conflict]`,
  `read NOTE` (`.body`; first line is the title), `search QUERY`.
- Change: `new TITLE [--folder F] [--body T|--file P|--stdin]`,
  `write NOTE --body T [--append]`, `rename NOTE TITLE`, `move NOTE FOLDER`,
  `delete NOTE --yes`, `toggle NOTE LINE`, `new-folder`, `rename-folder`,
  `delete-folder --yes`, `export-pdf NOTE`.
- Conflicts: `read` → `.conflicts[i].local|remote`; `resolve NOTE --choices
  local,remote,both,...` or `--all local`; if `.conflicts_unreadable`:
  `recover NOTE --strip|--synced` (backs up first).
- Sync: changes stay local until `sync` (push then pull) or `--push` on the
  change. Preview first: `push --dry-run` (exit 3 = something to push).
  `pull`, `clone`, `history NOTE [--records]`, `diff NOTE REF`, `restore NOTE --yes`.
- `vault_busy` = the Notes window is open and owns the vault: ask the user
  to make the change there or quit Notes. Reads (`history`, `diff` and
  `push --dry-run` too) always work.
- `read_only` notes cannot be changed here; `guardrail` = the text would add
  conflict markers (don't `--force`; use `resolve`).

## Photos: `icloud-photos`

`sync` · `albums` · `list [--album ID] [--since DATE] [--kind photo|video|live] [--limit N]`
· `info ID` · `download ID...|--all [--medium] [--out DIR]` · `thumb ID` ·
`open ID` · `upload FILE... [--album ID] [--no-sync]` · `delete ID... --yes` · `config` ·
`status` (exit 2 signed out). Batch commands exit 1 if any item failed;
each item has its own `error`.

## Find My: `icloud-findmy`

`devices [--locate] [--coords]` · `locate NAME|ID [--wait 30]` ·
`play-sound NAME|ID --yes` · `lost-mode NAME|ID --phone P --message M --yes` ·
`history NAME|ID [--since 24h]`. NAME: case-insensitive, unique part is
enough (`ambiguous` → use the ID). Show coordinates only when asked; never
Lost Mode without an explicit request.

## Reminders: `icloud-reminders`

`lists` · `list [LIST] [--completed|--all]` · `show REMINDER` ·
`add TITLE [--list L] [--notes T] [--due WHEN]` ·
`edit REMINDER [--title T] [--notes T] [--due WHEN|--no-due]` ·
`complete|uncomplete REMINDER` · `delete REMINDER --yes` · `sync`.
REMINDER: ID, title or unique part (open ones first). WHEN: `2026-10-10`
(all day), `"2026-10-10 17:30"`, `tomorrow 9:00`, `17:30`, `+2h`. Reads sync
first (`--cached` skips); writes need the network (`offline`). `due.at` is
UTC; `alerts` > 0 means alarms set on an Apple device that `--due` does not
move. Notifications are the timer's job, not yours.

## Recipes

```bash
icloud-notes --json sync && icloud-notes --json read "Groceries"
icloud-notes --json new "Meeting" --folder Work --body "- agenda" --push
icloud-photos --json sync; icloud-photos --json list --album "$ID" | jq -r '.[].id' | xargs icloud-photos --json download --out ~/Downloads/album
icloud-findmy --json locate "iPhone" && icloud-findmy --json play-sound "iPhone" --yes
icloud-reminders --json add "Call the dentist" --due "tomorrow 9:00" && icloud-reminders --json list
```
