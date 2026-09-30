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

#endif
