#ifndef CLI_H
#define CLI_H

#include <QString>

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

// `icloud-notes --sync`: the app's sync (push, then pull) with no window,
// for the systemd user timer that runs it every minute while Notes is
// closed. It syncs when backgroundStep() says so: every 15 minutes, and
// within about a minute of a local change (an editor's, an agent's) to
// push. The `sync` command's path through NotesBackend (conflict
// handling, reporting a refused session to icloud-session, the one retry),
// but it never waits for the lock and skips (0) when there is no vault,
// nobody is signed in to icloud-session (or it is unknown), or the app or
// another sync holds the vault's lock. Returns 0 once both halves worked
// (or nothing was due), non-zero when either failed or the engine is
// missing. What it did is written to `log`; a minute with nothing to do
// writes nothing. Each attempt is recorded in background.json in the
// engine's state directory. Needs a Q(Core|Gui)Application.
int runBackgroundSync(QTextStream &log);

// What the last background run did (background.json).
struct BackgroundRecord {
    qint64 lastRunMs = 0; // its time; 0: never
    bool lastOk = true; // false: the sync failed, or was skipped offline or signed out
    QString pendingKey; // the local changes it ran for (LocalChanges::key)
};

// What `icloud-notes-sync pending` found: what the next push would send,
// from the vault alone (no network, no lock).
struct LocalChanges {
    bool pending = false;
    qint64 newestChangeMs = 0; // the newest modification among them; 0: unknown
    QString key; // the files and that time: the same key, the same changes
};

// The background sync's choice each minute, at `nowMs`:
// - Sync when the last run is kBackgroundFullSyncMs (15 min) old, or there
//   was none (the pull cadence, and what retries a failure);
// - Sync sooner when there are local changes to push, unless the last run
//   failed (that waits its turn, so a broken network is not retried every
//   minute) or ran for these very changes already (a push that left them,
//   refused, is not repeated every minute);
// - and never while one of the changed files was modified in the last
//   kBackgroundSettleMs (10 s): it is likely being written (an editor's
//   autosave, an agent mid-edit), and each push is a version in iCloud. The
//   next minute sends it.
enum class BackgroundStep { Skip, Sync };
constexpr qint64 kBackgroundFullSyncMs = 15 * 60 * 1000;
constexpr qint64 kBackgroundSettleMs = 10 * 1000;
BackgroundStep backgroundStep(qint64 nowMs, const BackgroundRecord &last, const LocalChanges &changes);

#endif
