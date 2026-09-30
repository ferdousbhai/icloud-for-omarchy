#ifndef CLI_H
#define CLI_H

// `icloud-notes <command>`: every feature of the Notes window from a
// terminal (or an agent), without a window: list, read, search, create,
// edit, rename, move and delete notes and folders, resolve conflicts, and
// sync. Commands go through NotesBackend and SyncModel, the window's own
// rules, and take the vault's lock (VaultLock) before they change the vault
// or run icloud-notes-sync, as the window and the background sync do.
//
// Output is plain text, or JSON on stdout with --json; an error is then
// one JSON line on stderr, {"error":{"code","message","exit_code","hint"}}.
// Exit codes, as in every iCloud tool: 0 ok, 1 error, 2 sign-in required,
// 3 `push --dry-run` has changes or `diff` found differences, 64 usage.

// argv[1] starts a command (or global options, help, the version) rather
// than the window. `--sync`, the background sync, is not one.
bool isCliInvocation(const char *arg);

// Runs the command line, creating the application object itself (a
// QGuiApplication on the offscreen platform only for export-pdf, which
// lays out text). Returns the exit code.
int cliMain(int argc, char *argv[]);

class QTextStream;

// `icloud-notes --sync`: the app's sync (push, then pull) with no window,
// for the systemd user timer that keeps the vault current while Notes is
// closed. The `sync` command's path through NotesBackend (conflict
// handling, reporting a refused session to icloud-session, the one retry),
// but it never waits for the lock and skips (0) when there is no vault,
// nobody is signed in to icloud-session (or it is unknown), or the app or
// another sync holds the vault's lock. Returns 0 once both halves worked,
// non-zero when either failed or the engine is missing. What it did is
// written to `log`. Needs a Q(Core|Gui)Application.
int runBackgroundSync(QTextStream &log);

#endif
