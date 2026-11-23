//! The command line's commands and its help: every command's synopsis, what
//! it does, its JSON, the options it takes and how many arguments.

/// One command. `options`: what it takes besides the output flags,
/// `--vault`, `--wait` and `--help`.
pub struct Spec {
    pub name: &'static str,
    pub synopsis: &'static str,
    pub help: &'static str,
    pub options: &'static [&'static str],
    pub min_args: usize,
    pub max_args: usize,
}

const FOOTER: &str = concat!(
    "Output: text on a terminal; when piped (or with --json) the JSON envelope\n",
    "{\"ok\":true,\"data\",\"summary\",\"meta\"?} (\"JSON:\" above describes data); --quiet prints\n",
    "only data; --styled forces text; --ids-only prints one id per line (the note paths of list,\n",
    "search and read, the folder paths of folders, the path a change touched); --count the\n",
    "number of results. They combine: ids-only > count > quiet > json > styled. A failure is\n",
    "{\"ok\":false,\"error\":{\"code\",\"message\",\"hint\"?}} on one line of stderr in those machine\n",
    "modes, `error:`/`hint:` lines on stderr in text mode; stdout holds only results. Key order\n",
    "is not part of the contract.\n",
    "Global options: --vault DIR (a vault other than ~/Documents/Notes), --wait SECS (how long to\n",
    "wait for the vault's lock; 30 when a background sync holds it, none while the Notes window\n",
    "holds it). Nothing asks without a terminal: delete, delete-folder, restore, detach and a write\n",
    "that empties a note or drops an attachment's link need --yes, and so does a push or sync that\n",
    "would delete most of the vault.\n",
    "Exit codes: 0 on success, 1 on any failure; the error's code says which: usage, exists,\n",
    "not_found, sign_in_required or session_rejected (icloud-session sign-in), not set up\n",
    "(not_cloned, account_mismatch, ...), unavailable (offline, network, session_service:\n",
    "icloud-sessiond unreachable, sync_tool_missing, iCloud trouble). push --dry-run and diff\n",
    "report their changes in the result and exit 0. A failed sync, pull, push or clone (or the\n",
    "sync of a change's --push) still prints its result, with the failure in it as error: {code,\n",
    "message, hint?} (the sync engine's own error when it gave one; sync_failed when it did not),\n",
    "and exits 1. --version prints `icloud-notes VERSION` in every mode.",
);

// NOTE is a note's vault-relative path ("Notes/Groceries.md", .md optional),
// its apple-note-id, or its title (ignoring case) when exactly one note has it.
pub const SPECS: [Spec; 27] = [
    Spec {
        name: "status",
        synopsis: "status",
        help: concat!(
            "Where the vault is, whether it is cloned, the sign-in (from icloud-session), who holds the\n",
            "vault's lock (the Notes window, a background sync), and the notes the window badges: conflict,\n",
            "new, read-only, missing-id, foreign-id, conflict-copy (a Syncthing conflict copy, which is not\n",
            "synced), tables. Signed out, it still prints all that, and exits 1. --count: the number of notes.\n",
            "JSON: {vault, cloned, title_mode, sync_tool, sign_in_known, signed_in, apple_id,\n",
            "sign_in_days_left, lock: {held_by, app_open}, notes, folders, flagged: {FLAG: [path]}}",
        ),
        options: &[],
        min_args: 0,
        max_args: 0,
    },
    Spec {
        name: "folders",
        synopsis: "folders",
        help: concat!(
            "Every folder, Apple Notes' order (the default folder first), with how many notes it and its\n",
            "subfolders hold. The vault root is \"\" (All Notes). --ids-only lists the folder paths and\n",
            "--count counts them (not the vault root).\n",
            "JSON: [{folder, name, notes, default}]",
        ),
        options: &[],
        min_args: 0,
        max_args: 0,
    },
    Spec {
        name: "list",
        synopsis: "list [--folder F] [--flag FLAG]",
        help: concat!(
            "Every note, newest first within each folder: path, title, preview, modified time, id and the\n",
            "badges the window shows (conflict, new, read-only, missing-id, foreign-id, conflict-copy,\n",
            "tables). --folder lists that folder and its subfolders; --flag only notes with that badge.\n",
            "--ids-only: the paths.\n",
            "JSON: [{path, folder, file, title, snippet, modified, modified_ms, id, flags}]",
        ),
        options: &["--folder", "--flag"],
        min_args: 0,
        max_args: 0,
    },
    Spec {
        name: "read",
        synopsis: "read NOTE [--raw]",
        help: concat!(
            "A note's text as the window shows it (without the ID block), or with --raw the whole file.\n",
            "Also its attachments, why it is read-only if it is, and its conflicts: each block's two\n",
            "versions, this computer's (local) and iCloud's (remote), for `resolve`.\n",
            "JSON: {path, folder, file, title, id, modified, modified_ms, flags, read_only, body, text?,\n",
            "attachments: [{name, path, image}], conflicts: [{local: [line], remote: [line], before, after}],\n",
            "conflicts_unreadable, has_synced_copy}",
        ),
        options: &["--raw"],
        min_args: 1,
        max_args: 1,
    },
    Spec {
        name: "search",
        synopsis: "search QUERY",
        help: concat!(
            "Notes whose text contains QUERY (ignoring case, at least 2 characters), up to 100, as the\n",
            "window's search box finds them. --ids-only: the paths.\n",
            "JSON: [{path, folder, file, title, snippet}]",
        ),
        options: &[],
        min_args: 1,
        max_args: 1,
    },
    Spec {
        name: "new",
        synopsis: "new TITLE [--folder F] [--body TEXT | --file PATH | --stdin] [--push]",
        help: concat!(
            "Create a note in folder F (the account's default folder, \"Notes\", unless given; \"\" is the\n",
            "vault root). The title is its first line (or its file name in a filename-as-title vault);\n",
            "--body/--file/--stdin is the text below it. --push syncs afterwards (push, then pull).\n",
            "JSON: {action, path, folder, file, title, sync?}",
        ),
        options: &["--folder", "--body", "--file", "--stdin", "--push"],
        min_args: 1,
        max_args: 1,
    },
    Spec {
        name: "write",
        synopsis: "write NOTE (--body TEXT | --file PATH | --stdin) [--append] [--force] [--yes] [--push]",
        help: concat!(
            "Replace the note's text (everything `read` prints: in the default vault shape its first line\n",
            "is the title), or with --append add it at the end. The ID block is kept as it is. Refused for a\n",
            "read-only note, and (unless --force) when the text would add conflict markers or another\n",
            "note's id, the checks the window warns about. Empty text (blank lines only) empties the note,\n",
            "which the next push deletes in iCloud: it asks first, or needs --yes (use `delete` to delete).\n",
            "Text that no longer holds the link of one of the note's attachments (`read` lists them)\n",
            "removes that attachment at the next push, on every device: that asks first too, or needs --yes.\n",
            "JSON: {action, path, changed, sync?}",
        ),
        options: &["--body", "--file", "--stdin", "--append", "--force", "--yes", "--push"],
        min_args: 1,
        max_args: 1,
    },
    Spec {
        name: "rename",
        synopsis: "rename NOTE TITLE [--push]",
        help: concat!(
            "Retitle a note: its first line, or its file name in a filename-as-title vault.\n",
            "JSON: {action, path, from, title, sync?}",
        ),
        options: &["--push"],
        min_args: 2,
        max_args: 2,
    },
    Spec {
        name: "move",
        synopsis: "move NOTE FOLDER [--push]",
        help: concat!(
            "Move a note to another existing folder (\"\" is the vault root); the next push moves it in\n",
            "Notes, and takes the note's attachment files along. Refused for read-only notes.\n",
            "JSON: {action, path, from, sync?}",
        ),
        options: &["--push"],
        min_args: 2,
        max_args: 2,
    },
    Spec {
        name: "delete",
        synopsis: "delete NOTE [--yes] [--push]",
        help: concat!(
            "Move a note to the trash; the next push moves it to Recently Deleted in iCloud (recoverable\n",
            "there for about 30 days). Asks on a terminal; without one, --yes is required.\n",
            "JSON: {action, path, sync?}",
        ),
        options: &["--yes", "--push"],
        min_args: 1,
        max_args: 1,
    },
    Spec {
        name: "toggle",
        synopsis: "toggle NOTE LINE [--push]",
        help: concat!(
            "Tick or untick the checklist item on LINE (1 is the first line `read` prints); a plain list\n",
            "item gains a box, as Ctrl+Enter does in the window.\n",
            "JSON: {action, path, line, text, sync?}",
        ),
        options: &["--push"],
        min_args: 2,
        max_args: 2,
    },
    Spec {
        name: "attach",
        synopsis: "attach NOTE FILE [--push]",
        help: concat!(
            "Add FILE to the note as an attachment: it is copied into the note folder's attachments/\n",
            "(under another name if one is taken, never over a file) and a link to it is added at the end\n",
            "of the note. The next push uploads it to iCloud; until then, removing the link and the copy\n",
            "undoes it. FILE is a regular file of at most 100 MB. Refused for a read-only note.\n",
            "JSON: {action, path, file, link, sync?}",
        ),
        options: &["--push"],
        min_args: 2,
        max_args: 2,
    },
    Spec {
        name: "detach",
        synopsis: "detach NOTE FILE [--yes] [--push]",
        help: concat!(
            "Remove an attachment from the note: FILE is its file name, as `read` lists it (attachments:\n",
            "name). Its link is taken out of the note's text; the next push removes the attachment in\n",
            "iCloud, on every device, and deletes the file from attachments/ here. Asks on a terminal;\n",
            "without one, --yes is required. Refused for a read-only note. (A file added with `attach`\n",
            "and never pushed is only unlinked: its copy stays in attachments/.)\n",
            "JSON: {action, path, file, sync?}",
        ),
        options: &["--yes", "--push"],
        min_args: 2,
        max_args: 2,
    },
    Spec {
        name: "resolve",
        synopsis: "resolve NOTE (--all local|remote|both | --choices C1,C2,...) [--push]",
        help: concat!(
            "Settle a note's conflict blocks as the window's version picker does: keep this computer's\n",
            "version (local), iCloud's (remote) or both (local first), for every block (--all) or block by\n",
            "block (--choices, one per block, in order; `read` lists them).\n",
            "JSON: {action, path, choices, sync?}",
        ),
        options: &["--all", "--choices", "--push"],
        min_args: 1,
        max_args: 1,
    },
    Spec {
        name: "recover",
        synopsis: "recover NOTE (--strip | --synced) [--push]",
        help: concat!(
            "For conflict markers the picker cannot read (nested or out of order): --strip drops only the\n",
            "marker lines and keeps every other line; --synced goes back to the text last synced with\n",
            "iCloud. The note is first copied as it was to .icloud-notes/conflict-backups/.\n",
            "JSON: {action, path, how, backup, message, sync?}",
        ),
        options: &["--strip", "--synced", "--push"],
        min_args: 1,
        max_args: 1,
    },
    Spec {
        name: "export-pdf",
        synopsis: "export-pdf NOTE",
        help: concat!(
            "Save the note as a PDF next to it (never over an existing one).\n",
            "JSON: {action, path, pdf}",
        ),
        options: &[],
        min_args: 1,
        max_args: 1,
    },
    Spec {
        name: "new-folder",
        synopsis: "new-folder NAME [--in PARENT] [--push]",
        help: concat!(
            "Create a folder (inside PARENT, a folder path); the next push creates it in Notes.\n",
            "JSON: {action, folder, sync?}",
        ),
        options: &["--in", "--push"],
        min_args: 1,
        max_args: 1,
    },
    Spec {
        name: "rename-folder",
        synopsis: "rename-folder FOLDER NAME [--push]",
        help: concat!(
            "Rename a folder; the next push renames it in iCloud too.\n",
            "JSON: {action, folder, from, sync?}",
        ),
        options: &["--push"],
        min_args: 2,
        max_args: 2,
    },
    Spec {
        name: "delete-folder",
        synopsis: "delete-folder FOLDER [--yes] [--push]",
        help: concat!(
            "Move a folder and its notes to the trash; the next push moves the notes to Recently Deleted\n",
            "and deletes the folder in iCloud. Asks on a terminal; without one, --yes is required.\n",
            "JSON: {action, folder, sync?}",
        ),
        options: &["--yes", "--push"],
        min_args: 1,
        max_args: 1,
    },
    Spec {
        name: "sync",
        synopsis: "sync [--yes]",
        help: concat!(
            "What the window does on its own: push what changed here, then pull what changed in iCloud,\n",
            "in one icloud-notes-sync run with the vault's lock held. A refused sign-in is reported to\n",
            "icloud-session, which checks it with Apple (one retry if it still works). No network is the\n",
            "error offline (or network). A sync that ran but failed still prints its result, with error\n",
            "in it, and exits 1. When more than 3 tracked notes, and most of them, have no file any more\n",
            "(or an empty one), the push is refused (too_many_deletions: files gone by accident look like\n",
            "this) and the pull still runs; --yes pushes anyway, which moves those notes to Recently\n",
            "Deleted in iCloud.\n",
            "JSON: {ok, runs: [{command, ok, exit_code, result}], log, error?} (result: icloud-notes-sync's\n",
            "data for that half, or {error: {code, message, ...}} when it failed; error: why it failed)",
        ),
        options: &["--yes"],
        min_args: 0,
        max_args: 0,
    },
    Spec {
        name: "pull",
        synopsis: "pull",
        help: concat!(
            "Fetch what changed in iCloud (as sync, pull only).\n",
            "JSON: as for sync",
        ),
        options: &[],
        min_args: 0,
        max_args: 0,
    },
    Spec {
        name: "push",
        synopsis: "push [--dry-run] [--yes]",
        help: concat!(
            "Send what changed here to iCloud (as sync, push only). --dry-run previews it instead (exit 0,\n",
            "the changes in the result): icloud-notes-sync's own push --dry-run output (it takes no lock,\n",
            "so it works while the Notes window is open; the output flags are its own: --ids-only lists\n",
            "the files, --count counts them). A push that would delete most of the vault (more than 3\n",
            "tracked notes, and most of them, without a file) is refused: too_many_deletions,\n",
            "nothing sent; --dry-run lists them, and --yes pushes anyway (they go to Recently Deleted).\n",
            "JSON: as for sync; with --dry-run icloud-notes-sync's {entries, unchanged, notices, ...}",
        ),
        options: &["--dry-run", "--yes"],
        min_args: 0,
        max_args: 0,
    },
    Spec {
        name: "clone",
        synopsis: "clone",
        help: concat!(
            "Download every note into the (empty) vault, as the window's Clone my notes, for the account\n",
            "icloud-session is signed in to. Never opens a sign-in window: signed out is sign_in_required.\n",
            "JSON: as for sync",
        ),
        options: &[],
        min_args: 0,
        max_args: 0,
    },
    Spec {
        name: "history",
        synopsis: "history NOTE [--records]",
        help: concat!(
            "The note's past versions, newest first (icloud-notes-sync history; no lock, so it works while\n",
            "the Notes window is open). --records lists every snapshot record instead of the epoch timeline.\n",
            "JSON: icloud-notes-sync's {mode, epochs: [{id, timestamp, changed, carriedOver}]}, or with\n",
            "--records {mode: \"records\", records: [...]}. --ids-only: the epoch (or record) ids.",
        ),
        options: &["--records"],
        min_args: 1,
        max_args: 1,
    },
    Spec {
        name: "diff",
        synopsis: "diff NOTE REF",
        help: concat!(
            "A past version (an id from history) against iCloud's copy, or FROM..TO; exits 0 whether or\n",
            "not they differ (icloud-notes-sync diff; no lock).",
        ),
        options: &[],
        min_args: 2,
        max_args: 2,
    },
    Spec {
        name: "restore",
        synopsis: "restore NOTE [--yes]",
        help: concat!(
            "Throw away the note's local edits and go back to the last synced copy (icloud-notes-sync\n",
            "restore, which takes the vault's lock). Asks on a terminal; without one, --yes is required.",
        ),
        options: &["--yes"],
        min_args: 1,
        max_args: 1,
    },
    Spec {
        name: "leave",
        synopsis: "leave [--yes]",
        help: concat!(
            "Leave iCloud for Omacloud (icloud-notes-sync leave): Omacloud must be installed, set up and\n",
            "syncing the vault's folder of home (else omacloud_missing, omacloud_not_set_up or\n",
            "omacloud_folder_off, with the command that fixes it, and nothing changes); then one\n",
            "last sync (if it fails, nothing changes), and Omacloud syncs the folder. The engine's state\n",
            "moves to ~/.local/share/icloud-notes/left-icloud-state; iCloud keeps its copy as it was. Notes\n",
            "keeps working on the files; sync, pull, push, clone, history, diff and restore then refuse\n",
            "(left_icloud). Asks on a terminal; without one, --yes is required. The Notes window\n",
            "must be closed (vault_busy otherwise).\n",
            "JSON: icloud-notes-sync's {folder, left_at, kept_state, omacloud_synced, already}",
        ),
        options: &["--yes"],
        min_args: 0,
        max_args: 0,
    },
];

pub fn find(name: &str) -> Option<&'static Spec> {
    SPECS.iter().find(|spec| spec.name == name)
}

/// `icloud-notes help`: every command's synopsis.
pub fn usage_text() -> String {
    let mut out = String::from(
        "Usage: icloud-notes [open]          open the app\n       icloud-notes <command> [options]\n\nCommands:\n",
    );
    for spec in &SPECS {
        out += &format!("  {}\n", spec.synopsis);
    }
    out += "\nNOTE is a note's path in the vault (\"Notes/Groceries.md\", .md optional), its\n\
            apple-note-id, or its title when exactly one note has it.\n\
            `icloud-notes COMMAND --help` describes one command and its JSON.\n\n";
    out + FOOTER
}

/// `icloud-notes COMMAND --help`.
pub fn command_help(spec: &Spec) -> String {
    format!(
        "Usage: icloud-notes {}\n\n{}\n\nNOTE: a path in the vault (.md optional), an apple-note-id, or a title only one \
         note has.\n{FOOTER}",
        spec.synopsis, spec.help
    )
}
