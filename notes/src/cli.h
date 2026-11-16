#ifndef CLI_H
#define CLI_H

// `icloud-notes <command>`: every feature of the Notes window from a
// terminal (or an agent), without a window: list, read, search, create,
// edit, rename, move and delete notes and folders, resolve conflicts, and
// sync. Commands go through NotesBackend and SyncModel, the window's own
// rules, and take the vault's lock (VaultLock) before they change the vault
// or run icloud-notes-sync, as the window and the background sync do.
//
// Output follows the contract every iCloud tool shares (icloud_session::
// output): text on a terminal, the JSON envelope {"ok":true,"data","summary"}
// when piped or with --json; --quiet (data only), --styled (text),
// --ids-only, --count, combinable (ids-only > count > quiet > json >
// styled). A failure is {"ok":false,"error":{"code","message","hint"?}} on
// stdout in those machine modes, `error:`/`hint:` on stderr in text mode.
// `ok` prints first; keys inside data and error are sorted (QJsonObject),
// and key order is not part of the contract. Exit codes, as in every iCloud
// tool: 0 ok, 1 error, 2 usage or invalid request, 3 not signed in or not
// cloned, 4 not found, 5 offline or a service (icloud-session, the engine)
// unavailable. `push --dry-run` and `diff` report their changes and exit 0;
// a sync, pull, push or clone that ran but failed prints its result (with
// `error` in it: the engine's own code, message, hint and exit when it gave
// one, sync_failed/1 only when it did not) and exits with the failure's
// code. icloud-session unreachable is `session_service` (exit 5), and the
// sign-in, network and session-service hints are icloud_session::output's
// shared wording. `--version` is plain `icloud-notes VERSION` in every mode.

// argv[1] starts a command (or global options, help, the version) rather
// than the window. `--sync`, the background sync, is not one.
bool isCliInvocation(const char *arg);

// Runs the command line, creating the application object itself (a
// QGuiApplication on the offscreen platform only for export-pdf, which
// lays out text). Returns the exit code.
int cliMain(int argc, char *argv[]);

class QTextStream;

// `icloud-notes --sync`: what the systemd user timer runs every minute
// while Notes is closed: the engine's step (`icloud-notes-sync background`,
// NotesBackend::syncIfDue), which syncs every 15 minutes and within about a
// minute of a local change (an editor's, an agent's) to push, and otherwise
// does nothing. The decision is the engine's (notes-sync/src/cmd/
// background.rs); the window runs the same step while it is open. Once it
// syncs, the `sync` command's path through NotesBackend (conflict handling,
// reporting a refused session to icloud-session, the one retry). It never
// waits for the lock and does nothing (0) when there is no vault or the
// window or another sync holds the vault's lock; a due sync with nobody
// signed in to icloud-session (or it unknown) is a one-line skip (0).
// Returns 0 once both halves worked (or nothing was due), non-zero when
// either failed or the engine is missing. What it did is written to `log`;
// a minute with nothing to do writes nothing. Needs a
// Q(Core|Gui)Application.
int runBackgroundSync(QTextStream &log);

#endif
