#include "cli.h"
#include "notesbackend.h"
#include "syncmodel.h"
#include "vaultlock.h"

#include <QCoreApplication>
#include <QDateTime>
#include <QDir>
#include <QElapsedTimer>
#include <QEventLoop>
#include <QFile>
#include <QFileInfo>
#include <QGuiApplication>
#include <QHash>
#include <QJsonArray>
#include <QJsonDocument>
#include <QJsonObject>
#include <QSet>
#include <QTextStream>
#include <QThread>
#include <QTimeZone>
#include <QUrl>

#include <cerrno>
#include <cstdio>
#include <cstring>
#include <functional>
#include <memory>
#include <optional>
#include <unistd.h>
#include <variant>
#include <vector>

#ifndef ICLOUD_NOTES_VERSION
#define ICLOUD_NOTES_VERSION "dev"
#endif

namespace {

constexpr int kExitOk = 0;
constexpr int kExitError = 1;
constexpr int kExitSignIn = 2;
constexpr int kExitUsage = 64;

// How long a command waits for the vault's lock when a background sync or
// another command holds it (the window holds it while open: no waiting
// for that unless --wait says so).
constexpr int kDefaultWaitSecs = 30;

struct Failure {
    QString code;
    QString message;
    int exit = kExitError;
    QString hint;
};

Failure usageError(const QString &message)
{
    return { QStringLiteral("usage"), message, kExitUsage, {} };
}

// ---- commands ------------------------------------------------------------

struct Spec {
    const char *name;
    const char *synopsis;
    const char *help;
    QStringList options; // the options it takes, besides --json, --vault, --wait
    int minArgs;
    int maxArgs;
};

const char *const kFooter =
    "Global options: --json (JSON on stdout; an error is one JSON line on stderr,\n"
    "{\"error\":{\"code\",\"message\",\"exit_code\",\"hint\"}}), --vault DIR (a vault other than\n"
    "~/Documents/icloud-notes), --wait SECS (how long to wait for the vault's lock; 30 when\n"
    "a background sync holds it, none while the Notes window holds it).\n"
    "Exit codes: 0 ok, 1 error, 2 sign-in required (icloud-session sign-in), 3 push --dry-run\n"
    "has changes or diff found differences, 64 usage.";

// NOTE is a note's vault-relative path ("Notes/Groceries.md", .md optional),
// its apple-note-id, or its title (ignoring case) when exactly one note has it.
const QList<Spec> &specs()
{
    static const QList<Spec> list{
        { "status", "status",
          "Where the vault is, whether it is cloned, the sign-in (from icloud-session), who holds the\n"
          "vault's lock (the Notes window, a background sync), and the notes the window badges: conflict,\n"
          "new, read-only, missing-id, foreign-id, tables. Exits 2 when signed out.\n"
          "JSON: {vault, cloned, title_mode, sync_tool, sign_in_known, signed_in, apple_id,\n"
          "sign_in_days_left, lock: {held_by, app_open}, notes, folders, flagged: {FLAG: [path]}}",
          {}, 0, 0 },
        { "folders", "folders",
          "Every folder, Apple Notes' order (the default folder first), with how many notes it and its\n"
          "subfolders hold. The vault root is \"\" (All Notes).\n"
          "JSON: [{folder, name, notes, default}]",
          {}, 0, 0 },
        { "list", "list [--folder F] [--flag FLAG]",
          "Every note, newest first within each folder: path, title, preview, modified time, id and the\n"
          "badges the window shows (conflict, new, read-only, missing-id, foreign-id, tables). --folder\n"
          "lists one folder only (not its subfolders); --flag only notes with that badge.\n"
          "JSON: [{path, folder, file, title, snippet, modified, modified_ms, id, flags}]",
          { QStringLiteral("--folder"), QStringLiteral("--flag") }, 0, 0 },
        { "read", "read NOTE [--raw]",
          "A note's text as the window shows it (without the ID block), or with --raw the whole file.\n"
          "Also its attachments, why it is read-only if it is, and its conflicts: each block's two\n"
          "versions, this computer's (local) and iCloud's (remote), for `resolve`.\n"
          "JSON: {path, folder, file, title, id, modified, flags, read_only, body, text?, attachments:\n"
          "[{name, path, image}], conflicts: [{local: [line], remote: [line], before, after}],\n"
          "conflicts_unreadable, has_synced_copy}",
          { QStringLiteral("--raw") }, 1, 1 },
        { "search", "search QUERY",
          "Notes whose text contains QUERY (ignoring case, at least 2 characters), up to 100, as the\n"
          "window's search box finds them.\n"
          "JSON: [{path, folder, file, title, snippet}]",
          {}, 1, 1 },
        { "new", "new TITLE [--folder F] [--body TEXT | --file PATH | --stdin] [--push]",
          "Create a note in folder F (the account's default folder, \"Notes\", unless given; \"\" is the\n"
          "vault root). The title is its first line (or its file name in a filename-as-title vault);\n"
          "--body/--file/--stdin is the text below it. --push syncs afterwards (push, then pull).\n"
          "JSON: {action, path, folder, file, title, sync?}",
          { QStringLiteral("--folder"), QStringLiteral("--body"), QStringLiteral("--file"), QStringLiteral("--stdin"),
            QStringLiteral("--push") },
          1, 1 },
        { "write", "write NOTE (--body TEXT | --file PATH | --stdin) [--append] [--force] [--push]",
          "Replace the note's text (everything `read` prints: in the default vault shape its first line\n"
          "is the title), or with --append add it at the end. The ID block is kept as it is. Refused for a\n"
          "read-only note, and (unless --force) when the text would add conflict markers or another\n"
          "note's id, the checks the window warns about.\n"
          "JSON: {action, path, changed, sync?}",
          { QStringLiteral("--body"), QStringLiteral("--file"), QStringLiteral("--stdin"), QStringLiteral("--append"),
            QStringLiteral("--force"), QStringLiteral("--push") },
          1, 1 },
        { "rename", "rename NOTE TITLE [--push]",
          "Retitle a note: its first line, or its file name in a filename-as-title vault.\n"
          "JSON: {action, path, from, title, sync?}",
          { QStringLiteral("--push") }, 2, 2 },
        { "move", "move NOTE FOLDER [--push]",
          "Move a note to another existing folder (\"\" is the vault root); the next push moves it in\n"
          "Notes. Refused for read-only notes and notes with attachments.\n"
          "JSON: {action, path, from, sync?}",
          { QStringLiteral("--push") }, 2, 2 },
        { "delete", "delete NOTE [--yes] [--push]",
          "Move a note to the trash; the next push moves it to Recently Deleted in iCloud (recoverable\n"
          "there for about 30 days). Asks on a terminal; without one, --yes is required.\n"
          "JSON: {action, path, sync?}",
          { QStringLiteral("--yes"), QStringLiteral("--push") }, 1, 1 },
        { "toggle", "toggle NOTE LINE [--push]",
          "Tick or untick the checklist item on LINE (1 is the first line `read` prints); a plain list\n"
          "item gains a box, as Ctrl+Enter does in the window.\n"
          "JSON: {action, path, line, text, sync?}",
          { QStringLiteral("--push") }, 2, 2 },
        { "resolve", "resolve NOTE (--all local|remote|both | --choices C1,C2,...) [--push]",
          "Settle a note's conflict blocks as the window's version picker does: keep this computer's\n"
          "version (local), iCloud's (remote) or both (local first), for every block (--all) or block by\n"
          "block (--choices, one per block, in order; `read` lists them).\n"
          "JSON: {action, path, choices, sync?}",
          { QStringLiteral("--all"), QStringLiteral("--choices"), QStringLiteral("--push") }, 1, 1 },
        { "recover", "recover NOTE (--strip | --synced) [--push]",
          "For conflict markers the picker cannot read (nested or out of order): --strip drops only the\n"
          "marker lines and keeps every other line; --synced goes back to the text last synced with\n"
          "iCloud. The note is first copied as it was to .icloud-md/conflict-backups/.\n"
          "JSON: {action, path, how, backup, message, sync?}",
          { QStringLiteral("--strip"), QStringLiteral("--synced"), QStringLiteral("--push") }, 1, 1 },
        { "export-pdf", "export-pdf NOTE",
          "Save the note as a PDF next to it (never over an existing one).\n"
          "JSON: {action, path, pdf}",
          {}, 1, 1 },
        { "new-folder", "new-folder NAME [--in PARENT] [--push]",
          "Create a folder (inside PARENT, a folder path); the next push creates it in Notes.\n"
          "JSON: {action, folder, sync?}",
          { QStringLiteral("--in"), QStringLiteral("--push") }, 1, 1 },
        { "rename-folder", "rename-folder FOLDER NAME [--push]",
          "Rename a folder. iCloud folders carry no id, so the next push creates the new folder and moves\n"
          "the notes; the old one stays in Notes, empty, until it is deleted there.\n"
          "JSON: {action, folder, from, sync?}",
          { QStringLiteral("--push") }, 2, 2 },
        { "delete-folder", "delete-folder FOLDER [--yes] [--push]",
          "Move a folder and its notes to the trash; the next push moves the notes to Recently Deleted\n"
          "(the empty folder stays in Notes). Asks on a terminal; without one, --yes is required.\n"
          "JSON: {action, folder, sync?}",
          { QStringLiteral("--yes"), QStringLiteral("--push") }, 1, 1 },
        { "sync", "sync",
          "What the window does on its own: push what changed here, then pull what changed in iCloud,\n"
          "through icloud-notes-sync with the vault's lock held. A refused sign-in is reported to\n"
          "icloud-session, which checks it with Apple (one retry if it still works).\n"
          "JSON: {ok, runs: [{command, ok, exit_code, result}], log} (result: icloud-notes-sync's JSON)",
          {}, 0, 0 },
        { "pull", "pull", "Fetch what changed in iCloud (as sync, pull only).\nJSON: as for sync", {}, 0, 0 },
        { "push", "push [--dry-run]",
          "Send what changed here to iCloud (as sync, push only). --dry-run previews it instead and exits 3\n"
          "when there is something to push: icloud-notes-sync's own push --dry-run output (it takes no\n"
          "lock, so it works while the Notes window is open).\n"
          "JSON: as for sync; with --dry-run icloud-notes-sync's {entries, unchanged, notices, ...}",
          { QStringLiteral("--dry-run") }, 0, 0 },
        { "clone", "clone",
          "Download every note into the (empty) vault, as the window's Clone my notes, for the account\n"
          "icloud-session is signed in to. Never opens a sign-in window: signed out exits 2.\n"
          "JSON: as for sync",
          {}, 0, 0 },
        { "history", "history NOTE [--records]",
          "The note's past versions, newest first (icloud-notes-sync history; no lock, so it works while\n"
          "the Notes window is open). --records lists every snapshot record instead of the epoch timeline.\n"
          "JSON: icloud-notes-sync's {mode, epochs: [{id, timestamp, changed, carriedOver}]}, or with\n"
          "--records {mode: \"records\", records: [...]}",
          { QStringLiteral("--records") }, 1, 1 },
        { "diff", "diff NOTE REF",
          "A past version (an id from history) against iCloud's copy, or FROM..TO; exits 3 when they\n"
          "differ (icloud-notes-sync diff; no lock).",
          {}, 2, 2 },
        { "restore", "restore NOTE [--yes]",
          "Throw away the note's local edits and go back to the last synced copy (icloud-notes-sync\n"
          "restore, which takes the vault's lock). Asks on a terminal; without one, --yes is required.",
          { QStringLiteral("--yes") }, 1, 1 },
    };
    return list;
}

const Spec *findSpec(const QString &name)
{
    for (const Spec &s : specs())
        if (name == QLatin1StringView(s.name))
            return &s;
    return nullptr;
}

QString usageText()
{
    QString out = QStringLiteral("Usage: icloud-notes                 open the app\n"
                                 "       icloud-notes <command> [options]\n\nCommands:\n");
    for (const Spec &s : specs())
        out += QStringLiteral("  %1\n").arg(QLatin1StringView(s.synopsis));
    out += QStringLiteral("\nNOTE is a note's path in the vault (\"Notes/Groceries.md\", .md optional), its\n"
                          "apple-note-id, or its title when exactly one note has it.\n"
                          "`icloud-notes COMMAND --help` describes one command and its JSON.\n\n");
    out += QLatin1StringView(kFooter);
    return out;
}

QString commandHelp(const Spec &s)
{
    return QStringLiteral("Usage: icloud-notes %1\n\n%2\n\nNOTE: a path in the vault (.md optional), an apple-note-id, "
                          "or a title only one note has.\n%3")
        .arg(QLatin1StringView(s.synopsis), QLatin1StringView(s.help), QLatin1StringView(kFooter));
}

// ---- arguments -----------------------------------------------------------

const QSet<QString> kValued{ QStringLiteral("--folder"), QStringLiteral("--flag"),   QStringLiteral("--body"),
                             QStringLiteral("--file"),   QStringLiteral("--in"),     QStringLiteral("--all"),
                             QStringLiteral("--choices"), QStringLiteral("--vault"), QStringLiteral("--wait") };
const QSet<QString> kSwitches{ QStringLiteral("--json"),    QStringLiteral("--yes"),    QStringLiteral("--push"),
                               QStringLiteral("--append"),  QStringLiteral("--force"),  QStringLiteral("--stdin"),
                               QStringLiteral("--raw"),     QStringLiteral("--dry-run"), QStringLiteral("--strip"),
                               QStringLiteral("--synced"),  QStringLiteral("--records"), QStringLiteral("--help"),
                               QStringLiteral("--version") };

struct Args {
    QString command;
    QStringList positional;
    QHash<QString, QString> values;
    QSet<QString> switches;
    bool json = false;

    bool has(const char *name) const { return switches.contains(QLatin1StringView(name)); }
    std::optional<QString> value(const char *name) const
    {
        const auto it = values.constFind(QLatin1StringView(name));
        return it == values.cend() ? std::nullopt : std::optional<QString>(*it);
    }
};

std::optional<Failure> parseArgs(const QStringList &argv, Args &a)
{
    for (qsizetype i = 0; i < argv.size(); ++i) {
        QString arg = argv.at(i);
        if (arg == u"--") {
            a.positional += argv.mid(i + 1);
            break;
        }
        if (arg == u"-h")
            arg = QStringLiteral("--help");
        if (arg == u"-V")
            arg = QStringLiteral("--version");
        if (!arg.startsWith(u"--") || arg.size() == 2) {
            if (arg.startsWith(u'-') && arg.size() > 1)
                return usageError(QStringLiteral("unknown option %1").arg(arg));
            if (a.command.isEmpty())
                a.command = arg;
            else
                a.positional << arg;
            continue;
        }
        QString name = arg, inline_;
        bool hasInline = false;
        if (const qsizetype eq = arg.indexOf(u'='); eq > 0) {
            name = arg.left(eq);
            inline_ = arg.mid(eq + 1);
            hasInline = true;
        }
        if (kValued.contains(name)) {
            if (!hasInline) {
                if (i + 1 >= argv.size())
                    return usageError(QStringLiteral("%1 needs a value").arg(name));
                inline_ = argv.at(++i);
            }
            a.values.insert(name, inline_);
        } else if (kSwitches.contains(name)) {
            if (hasInline)
                return usageError(QStringLiteral("%1 takes no value").arg(name));
            a.switches.insert(name);
        } else {
            return usageError(QStringLiteral("unknown option %1").arg(name));
        }
    }
    a.json = a.has("--json");
    return std::nullopt;
}

std::optional<Failure> checkArgs(const Spec &spec, const Args &a)
{
    static const QSet<QString> global{ QStringLiteral("--json"), QStringLiteral("--vault"), QStringLiteral("--wait"),
                                       QStringLiteral("--help") };
    for (const QString &name : a.values.keys() + QStringList(a.switches.values()))
        if (!global.contains(name) && !spec.options.contains(name))
            return usageError(QStringLiteral("%1 does not take %2").arg(QLatin1StringView(spec.name), name));
    const qsizetype n = a.positional.size();
    if (n < spec.minArgs || n > spec.maxArgs)
        return usageError(QStringLiteral("usage: icloud-notes %1").arg(QLatin1StringView(spec.synopsis)));
    return std::nullopt;
}

// ---- output ---------------------------------------------------------------

QTextStream &out()
{
    static QTextStream stream(stdout);
    return stream;
}

void printJson(const QJsonValue &value)
{
    const QJsonDocument doc = value.isArray() ? QJsonDocument(value.toArray()) : QJsonDocument(value.toObject());
    out() << doc.toJson(QJsonDocument::Indented);
    out().flush();
}

// A command's result: `json` under --json, else `text` (if any) on a line of its own.
void print(bool asJson, const QJsonValue &json, const QString &text)
{
    if (asJson)
        printJson(json);
    else if (!text.isEmpty())
        out() << text << (text.endsWith(u'\n') ? "" : "\n");
    out().flush();
}

int report(const Failure &f, bool json)
{
    QTextStream err(stderr);
    if (json) {
        QJsonObject error{ { QStringLiteral("code"), f.code },
                           { QStringLiteral("message"), f.message },
                           { QStringLiteral("exit_code"), f.exit } };
        if (!f.hint.isEmpty())
            error.insert(QStringLiteral("hint"), f.hint);
        err << QJsonDocument(QJsonObject{ { QStringLiteral("error"), error } }).toJson(QJsonDocument::Compact) << '\n';
    } else {
        err << "icloud-notes: " << f.message << '\n';
        if (!f.hint.isEmpty())
            err << f.hint << '\n';
    }
    return f.exit;
}

QString iso(qint64 ms)
{
    return QDateTime::fromMSecsSinceEpoch(ms, QTimeZone::UTC).toString(Qt::ISODate);
}

QJsonValue orNull(const QString &s)
{
    return s.isEmpty() ? QJsonValue() : QJsonValue(s);
}

// ---- the event loop ---------------------------------------------------------

// Spin until cond holds or timeoutMs (<0: no limit) passes: D-Bus answers
// and the sync tool's exit arrive through the event loop.
bool waitFor(const std::function<bool()> &cond, int timeoutMs = -1)
{
    QElapsedTimer timer;
    timer.start();
    while (!cond()) {
        if (timeoutMs >= 0 && timer.elapsed() >= timeoutMs)
            return false;
        QCoreApplication::processEvents(QEventLoop::AllEvents, 50);
        QThread::msleep(10);
    }
    return true;
}

// ---- notes ----------------------------------------------------------------

struct Note {
    QString folder;
    QString file;
    QString title;
    QString snippet;
    QString id;
    qint64 modifiedMs = 0;
    QStringList flags;

    QString path() const { return folder.isEmpty() ? file : folder + u'/' + file; }
};

// Every note in the given folders (all by default), as the window lists
// them: its note list's order, details and badges.
QList<Note> scanNotes(NotesBackend &b, const QStringList &folders = {})
{
    QList<Note> notes;
    b.refresh();
    for (const QString &folder : folders.isEmpty() ? b.folders() : folders) {
        b.setCurrentFolder(folder);
        for (const QString &file : b.notes()) {
            const QVariantMap d = b.noteDetails().value(file).toMap();
            notes << Note{ folder,
                           file,
                           d.value(QStringLiteral("title")).toString(),
                           d.value(QStringLiteral("snippet")).toString(),
                           d.value(QStringLiteral("id")).toString(),
                           d.value(QStringLiteral("modifiedMs")).toLongLong(),
                           b.noteStates().value(file).toStringList() };
        }
    }
    return notes;
}

QJsonObject noteJson(const Note &n)
{
    return { { QStringLiteral("path"), n.path() },
             { QStringLiteral("folder"), n.folder },
             { QStringLiteral("file"), n.file },
             { QStringLiteral("title"), n.title },
             { QStringLiteral("snippet"), n.snippet },
             { QStringLiteral("modified"), iso(n.modifiedMs) },
             { QStringLiteral("modified_ms"), n.modifiedMs },
             { QStringLiteral("id"), orNull(n.id) },
             { QStringLiteral("flags"), QJsonArray::fromStringList(n.flags) } };
}

// A note by path, id or unique title (see the usage text).
std::optional<Failure> resolveNote(NotesBackend &b, const QString &arg, Note &found)
{
    QString want = arg.trimmed();
    const QString root = QFileInfo(NotesBackend::rootPath()).absoluteFilePath();
    if (QFileInfo(want).isAbsolute())
        want = QDir(root).relativeFilePath(QFileInfo(want).absoluteFilePath());
    if (want.startsWith(QStringLiteral("./")))
        want = want.mid(2);
    const QList<Note> notes = scanNotes(b);
    for (const Note &n : notes)
        if (n.path() == want || n.path() == want + QStringLiteral(".md")) {
            found = n;
            return std::nullopt;
        }
    QList<Note> matches;
    for (const Note &n : notes)
        if (!n.id.isEmpty() && n.id == want)
            matches << n;
    if (matches.isEmpty())
        for (const Note &n : notes)
            if (n.title.compare(want, Qt::CaseInsensitive) == 0 || n.file.chopped(3).compare(want, Qt::CaseInsensitive) == 0)
                matches << n;
    if (matches.size() == 1) {
        found = matches.first();
        return std::nullopt;
    }
    if (matches.isEmpty())
        return Failure{ QStringLiteral("not_found"), QStringLiteral("no note \"%1\"").arg(arg), kExitError,
                        QStringLiteral("`icloud-notes list` shows every note's path; a title must match exactly.") };
    QStringList paths;
    for (const Note &n : matches)
        paths << n.path();
    return Failure{ QStringLiteral("ambiguous"),
                    QStringLiteral("\"%1\" names %2 notes: %3").arg(arg).arg(matches.size()).arg(paths.join(QStringLiteral(", "))),
                    kExitError, QStringLiteral("Name the note by its path.") };
}

// Makes the note the backend's open one, as a click in the list does.
std::optional<Failure> openNote(NotesBackend &b, const Note &n)
{
    b.setCurrentFolder(n.folder);
    b.refresh();
    b.openNote(n.file);
    if (b.currentNote() != n.file)
        return Failure{ QStringLiteral("not_found"), QStringLiteral("could not open %1").arg(n.path()), kExitError, {} };
    return std::nullopt;
}

// A folder path as given ("" and "/" are the vault root), if it exists.
std::optional<Failure> resolveFolder(NotesBackend &b, const QString &arg, QString &folder)
{
    folder = arg.trimmed();
    while (folder.endsWith(u'/'))
        folder.chop(1);
    while (folder.startsWith(u'/'))
        folder = folder.mid(1);
    if (b.folders().contains(folder))
        return std::nullopt;
    for (const QString &f : b.folders())
        if (f.compare(folder, Qt::CaseInsensitive) == 0) {
            folder = f;
            return std::nullopt;
        }
    return Failure{ QStringLiteral("not_found"), QStringLiteral("no folder \"%1\"").arg(arg), kExitError,
                    QStringLiteral("`icloud-notes folders` lists them; `icloud-notes new-folder` makes one.") };
}

// ---- the lock, the sign-in, syncing -----------------------------------------

// The vault's lock, as the window and the background sync take it. The
// window holds it for as long as it is open: refused at once then (unless
// --wait), since it may be syncing this very vault. A background sync or
// another command is waited for (--wait SECS, 30 by default).
std::optional<Failure> takeLock(NotesBackend &b, const Args &a)
{
    std::optional<int> waitSecs;
    if (const auto w = a.value("--wait")) {
        bool ok = false;
        waitSecs = w->toInt(&ok);
        if (!ok || *waitSecs < 0)
            return usageError(QStringLiteral("--wait wants whole seconds, not \"%1\"").arg(*w));
    }
    QElapsedTimer timer;
    timer.start();
    for (;;) {
        switch (b.lockVault()) {
        case VaultLock::Locked:
            return std::nullopt;
        case VaultLock::Failed:
            return Failure{ QStringLiteral("vault_lock"),
                            QStringLiteral("could not open the vault's lock %1").arg(NotesBackend::lockPath()), kExitError, {} };
        case VaultLock::Busy:
            break;
        }
        const QString holder = b.lockHolder();
        const bool app = holder.startsWith(QStringLiteral("Notes (pid"));
        const int limitMs = waitSecs.value_or(app ? 0 : kDefaultWaitSecs) * 1000;
        if (timer.elapsed() >= limitMs)
            return Failure{ QStringLiteral("vault_busy"),
                            app ? QStringLiteral("%1 is open and owns the notes vault while it is open.").arg(holder)
                                : QStringLiteral("%1 holds the notes vault.").arg(holder),
                            kExitError,
                            app ? QStringLiteral("Make the change in Notes, or quit it and retry (or --wait SECS for it to close).")
                                : QStringLiteral("Retry later, or pass --wait SECS.") };
        waitFor([] { return false; }, 200);
    }
}

// What icloud-session says, for commands that talk to iCloud.
std::optional<Failure> needSignIn(NotesBackend &b)
{
    b.refreshSignIn();
    waitFor([&] { return !b.signInPending(); }, 20000);
    if (!b.signInKnown())
        return Failure{ QStringLiteral("session_unavailable"),
                        QStringLiteral("icloud-session is not available, so the sign-in is unknown."), kExitError,
                        QStringLiteral("Install icloud-session (sudo pacman -S icloud-session); `icloud-session status` "
                                       "shows what it says.") };
    if (!b.signedIn() || b.authExpired())
        return Failure{ QStringLiteral("sign_in_required"), QStringLiteral("Not signed in to iCloud."), kExitSignIn,
                        QStringLiteral("Run `icloud-session sign-in`: a person signs in in the window that opens.") };
    return std::nullopt;
}

std::optional<Failure> needSyncTool(NotesBackend &b)
{
    if (b.syncToolAvailable())
        return std::nullopt;
    return Failure{ QStringLiteral("sync_tool_missing"),
                    QStringLiteral("the sync engine (icloud-notes-sync) is missing."), kExitError,
                    QStringLiteral("Reinstall icloud-notes: sudo pacman -S icloud-notes") };
}

std::optional<Failure> needCloned()
{
    if (NotesBackend::vaultCloned())
        return std::nullopt;
    return Failure{ QStringLiteral("not_cloned"),
                    QStringLiteral("no notes cloned at %1").arg(NotesBackend::rootPath()), kExitError,
                    QStringLiteral("Run `icloud-notes clone` (signed in to icloud-session) first.") };
}

struct SyncOutcome {
    QJsonObject json;
    QString text;
    std::optional<Failure> failure;
};

// `what`: "sync" (push, then pull), "pull", "push" or "clone", through the
// backend with its lock held, as the window runs them.
SyncOutcome runSync(NotesBackend &b, const QString &what, bool json)
{
    SyncOutcome result;
    if (auto f = needSyncTool(b)) {
        result.failure = f;
        return result;
    }
    if (auto f = needSignIn(b)) {
        result.failure = f;
        return result;
    }
    b.setToolJson(json);
    b.clearLog();
    QJsonArray runs;
    QHash<QString, bool> last;
    bool cloneDone = false;
    const QMetaObject::Connection finished =
        QObject::connect(&b, &NotesBackend::syncFinished, &b, [&](const QString &label, bool ok) {
            const QString command = label.toLower();
            const QJsonDocument doc = QJsonDocument::fromJson(b.lastOutput());
            runs.append(QJsonObject{ { QStringLiteral("command"), command },
                                     { QStringLiteral("ok"), ok },
                                     { QStringLiteral("exit_code"), b.lastExitCode() },
                                     { QStringLiteral("result"), doc.isObject() ? QJsonValue(doc.object()) : QJsonValue() } });
            last.insert(command, ok);
        });
    const QMetaObject::Connection cloned =
        QObject::connect(&b, &NotesBackend::cloneFinished, &b, [&] { cloneDone = true; });
    if (what == u"sync")
        b.runSync();
    else if (what == u"pull")
        b.runPull();
    else if (what == u"push")
        b.runPush();
    else
        b.runClone();
    waitFor([&] { return b.idle() && (what != u"clone" || cloneDone); });
    QObject::disconnect(finished);
    QObject::disconnect(cloned);

    bool ok = !last.isEmpty();
    for (const QString &half : what == u"sync" ? QStringList{ QStringLiteral("push"), QStringLiteral("pull") }
                                               : QStringList{ what })
        ok = ok && last.value(half);
    result.json = QJsonObject{ { QStringLiteral("ok"), ok }, { QStringLiteral("runs"), runs },
                               { QStringLiteral("log"), b.syncLog() } };
    result.text = b.syncLog();
    if (b.authExpired())
        result.failure = Failure{ QStringLiteral("sign_in_required"),
                                  QStringLiteral("iCloud refused the sign-in; icloud-session was told."), kExitSignIn,
                                  QStringLiteral("Run `icloud-session sign-in`: a person signs in in the window that opens.") };
    else if (!ok)
        result.failure = Failure{ QStringLiteral("sync_failed"), QStringLiteral("%1 failed; see the log.").arg(what),
                                  kExitError, {} };
    return result;
}

// Becomes icloud-notes-sync itself (exec) for push --dry-run, history,
// diff and restore, in the vault: its output, errors and exit codes are this
// tool's. The engine takes the vault's lock where it needs it (restore; the
// others only read), honouring --wait.
int execEngine(NotesBackend &b, const Args &a, const QStringList &toolArgs)
{
    if (auto f = needSyncTool(b))
        return report(*f, a.json);
    QStringList args{ NotesBackend::syncToolPath() };
    if (a.json)
        args << QStringLiteral("--json");
    if (const auto wait = a.value("--wait"))
        args << QStringLiteral("--wait") << *wait;
    args += toolArgs;
    QList<QByteArray> bytes;
    for (const QString &arg : args)
        bytes << QFile::encodeName(arg);
    std::vector<char *> argv;
    for (QByteArray &arg : bytes)
        argv.push_back(arg.data());
    argv.push_back(nullptr);
    out().flush();
    if (!QDir::setCurrent(NotesBackend::rootPath()))
        return report(Failure{ QStringLiteral("error"), QStringLiteral("cannot enter %1").arg(NotesBackend::rootPath()),
                               kExitError, {} },
                      a.json);
    ::execv(argv.front(), argv.data());
    return report(Failure{ QStringLiteral("error"),
                           QStringLiteral("icloud-notes-sync did not run: %1").arg(QString::fromLocal8Bit(strerror(errno))),
                           kExitError, {} },
                  a.json);
}

// ---- input ------------------------------------------------------------------

std::optional<Failure> readBody(const Args &a, QString &body, bool &given)
{
    const int sources = int(a.value("--body").has_value()) + int(a.value("--file").has_value()) + int(a.has("--stdin"));
    given = sources > 0;
    if (sources > 1)
        return usageError(QStringLiteral("give the text once: --body, --file or --stdin"));
    if (const auto text = a.value("--body")) {
        body = *text;
    } else if (const auto path = a.value("--file")) {
        QFile file(*path);
        if (!file.open(QIODevice::ReadOnly))
            return Failure{ QStringLiteral("error"), QStringLiteral("cannot read %1: %2").arg(*path, file.errorString()),
                            kExitError, {} };
        body = QString::fromUtf8(file.readAll());
    } else if (a.has("--stdin")) {
        QFile in;
        if (!in.open(stdin, QIODevice::ReadOnly))
            return Failure{ QStringLiteral("error"), QStringLiteral("cannot read stdin"), kExitError, {} };
        body = QString::fromUtf8(in.readAll());
    }
    return std::nullopt;
}

// Asks on a terminal unless --yes; refuses without one.
std::optional<Failure> confirm(const Args &a, const QString &question)
{
    if (a.has("--yes"))
        return std::nullopt;
    if (!isatty(STDIN_FILENO))
        return usageError(QStringLiteral("%1 asks before it acts: pass --yes when stdin is not a terminal").arg(a.command));
    QTextStream err(stderr);
    err << question << " [y/N] ";
    err.flush();
    QTextStream in(stdin);
    const QString answer = in.readLine().trimmed().toLower();
    if (answer == u"y" || answer == u"yes")
        return std::nullopt;
    return Failure{ QStringLiteral("cancelled"), QStringLiteral("cancelled"), kExitError, {} };
}

// ---- the commands -----------------------------------------------------------

struct Done {
    QJsonValue json;
    QString text;
};

using Result = std::variant<Done, Failure>;

int finish(NotesBackend &b, const Args &a, Result result)
{
    if (std::holds_alternative<Failure>(result))
        return report(std::get<Failure>(result), a.json);
    Done done = std::get<Done>(std::move(result));
    std::optional<Failure> syncFailure;
    if (a.has("--push")) {
        SyncOutcome sync = runSync(b, QStringLiteral("sync"), a.json);
        QJsonObject json = done.json.toObject();
        json.insert(QStringLiteral("sync"), sync.failure && sync.json.isEmpty() ? QJsonValue() : QJsonValue(sync.json));
        done.json = json;
        if (!sync.text.isEmpty())
            done.text += u'\n' + sync.text;
        syncFailure = sync.failure;
        if (syncFailure)
            syncFailure->message = QStringLiteral("%1 done here, but not synced: %2").arg(a.command, syncFailure->message);
    }
    print(a.json, done.json, done.text);
    return syncFailure ? report(*syncFailure, a.json) : kExitOk;
}

Done status(NotesBackend &b, int &exitCode)
{
    b.refreshSignIn();
    waitFor([&] { return !b.signInPending(); }, 20000);
    QString holder;
    switch (b.lockVault()) {
    case VaultLock::Locked:
        b.unlockVault();
        break;
    case VaultLock::Busy:
        holder = b.lockHolder();
        break;
    case VaultLock::Failed:
        break;
    }
    const bool cloned = NotesBackend::vaultCloned();
    QJsonObject flagged;
    int count = 0;
    if (cloned) {
        const QList<Note> notes = scanNotes(b);
        count = int(notes.size());
        for (const Note &n : notes)
            for (const QString &flag : n.flags) {
                QJsonArray paths = flagged.value(flag).toArray();
                paths.append(n.path());
                flagged.insert(flag, paths);
            }
    }
    const bool appOpen = holder.startsWith(QStringLiteral("Notes (pid"));
    QJsonObject json{
        { QStringLiteral("vault"), NotesBackend::rootPath() },
        { QStringLiteral("cloned"), cloned },
        { QStringLiteral("title_mode"), b.vaultTitleMode() },
        { QStringLiteral("sync_tool"), b.syncToolAvailable() },
        { QStringLiteral("sign_in_known"), b.signInKnown() },
        { QStringLiteral("signed_in"), b.signInKnown() ? QJsonValue(b.signedIn()) : QJsonValue() },
        { QStringLiteral("apple_id"), orNull(b.appleId()) },
        { QStringLiteral("sign_in_days_left"), b.signInDaysLeft() },
        { QStringLiteral("lock"), QJsonObject{ { QStringLiteral("held_by"), orNull(holder) },
                                               { QStringLiteral("app_open"), appOpen } } },
        { QStringLiteral("notes"), count },
        { QStringLiteral("folders"), cloned ? int(b.folders().size()) - 1 : 0 },
        { QStringLiteral("flagged"), flagged },
    };
    QStringList lines;
    lines << QStringLiteral("Vault:       %1%2").arg(NotesBackend::rootPath(), cloned ? QString() : QStringLiteral(" (not cloned)"));
    lines << QStringLiteral("Signed in:   %1")
                 .arg(!b.signInKnown() ? QStringLiteral("unknown (icloud-session is not available)")
                      : b.signedIn()   ? QStringLiteral("yes, as %1").arg(b.appleId())
                                       : QStringLiteral("no (icloud-session sign-in)"));
    lines << QStringLiteral("Sync tool:   %1").arg(b.syncToolAvailable() ? NotesBackend::syncToolPath() : QStringLiteral("missing"));
    lines << QStringLiteral("Lock:        %1").arg(holder.isEmpty() ? QStringLiteral("free") : holder);
    lines << QStringLiteral("Notes:       %1 in %2 folders").arg(count).arg(json.value(QStringLiteral("folders")).toInt());
    for (auto it = flagged.constBegin(); it != flagged.constEnd(); ++it)
        lines << QStringLiteral("  %1: %2").arg(it.key(), QString::number(it.value().toArray().size()));
    exitCode = b.signInKnown() && !b.signedIn() ? kExitSignIn : kExitOk;
    return { json, lines.join(u'\n') };
}

Result folders(NotesBackend &b)
{
    QJsonArray list;
    QStringList lines;
    const QString def = b.defaultFolder();
    for (const QString &f : b.folders()) {
        const int n = b.folderNoteCounts().value(f).toInt();
        list.append(QJsonObject{ { QStringLiteral("folder"), f },
                                 { QStringLiteral("name"), f.isEmpty() ? QStringLiteral("All Notes") : f.section(u'/', -1) },
                                 { QStringLiteral("notes"), n },
                                 { QStringLiteral("default"), !f.isEmpty() && f == def } });
        lines << QStringLiteral("%1\t%2").arg(n).arg(f.isEmpty() ? QStringLiteral("(All Notes)") : f);
    }
    return Done{ list, lines.join(u'\n') };
}

Result list(NotesBackend &b, const Args &a)
{
    QStringList only;
    if (const auto f = a.value("--folder")) {
        QString folder;
        if (auto fail = resolveFolder(b, *f, folder))
            return *fail;
        only << folder;
    }
    const std::optional<QString> flag = a.value("--flag");
    QJsonArray arr;
    QStringList lines;
    for (const Note &n : scanNotes(b, only)) {
        if (flag && !n.flags.contains(*flag))
            continue;
        arr.append(noteJson(n));
        lines << QStringLiteral("%1\t%2\t%3\t%4").arg(n.path(), n.title, iso(n.modifiedMs), n.flags.join(u','));
    }
    return Done{ arr, lines.join(u'\n') };
}

Result read(NotesBackend &b, const Args &a)
{
    Note n;
    if (auto f = resolveNote(b, a.positional.at(0), n))
        return *f;
    if (auto f = openNote(b, n))
        return *f;
    QJsonArray attachments;
    for (const QVariant &v : b.noteAttachments()) {
        const QVariantMap m = v.toMap();
        attachments.append(QJsonObject{ { QStringLiteral("name"), m.value(QStringLiteral("name")).toString() },
                                        { QStringLiteral("path"), QUrl(m.value(QStringLiteral("url")).toString()).toLocalFile() },
                                        { QStringLiteral("image"), m.value(QStringLiteral("image")).toBool() } });
    }
    QJsonArray conflicts;
    for (const QVariant &v : b.noteConflicts()) {
        const QVariantMap m = v.toMap();
        auto lines = [](const QVariantList &side) {
            QJsonArray arr;
            for (const QVariant &line : side)
                arr.append(line.toMap().value(QStringLiteral("text")).toString());
            return arr;
        };
        conflicts.append(QJsonObject{ { QStringLiteral("local"), lines(m.value(QStringLiteral("local")).toList()) },
                                      { QStringLiteral("remote"), lines(m.value(QStringLiteral("remote")).toList()) },
                                      { QStringLiteral("before"), QJsonArray::fromStringList(m.value(QStringLiteral("before")).toStringList()) },
                                      { QStringLiteral("after"), QJsonArray::fromStringList(m.value(QStringLiteral("after")).toStringList()) } });
    }
    QJsonObject json = noteJson(n);
    json.remove(QStringLiteral("snippet"));
    json.insert(QStringLiteral("read_only"), orNull(b.readOnlyReason()));
    json.insert(QStringLiteral("body"), b.noteBody());
    if (a.has("--raw"))
        json.insert(QStringLiteral("text"), b.noteContent());
    json.insert(QStringLiteral("attachments"), attachments);
    json.insert(QStringLiteral("conflicts"), conflicts);
    json.insert(QStringLiteral("conflicts_unreadable"), b.noteConflictsUnreadable());
    json.insert(QStringLiteral("has_synced_copy"), b.noteHasSyncedCopy());
    return Done{ json, a.has("--raw") ? b.noteContent() : b.noteBody() };
}

Result search(NotesBackend &b, const Args &a)
{
    const QString q = a.positional.at(0).trimmed();
    if (q.size() < 2)
        return usageError(QStringLiteral("search wants at least 2 characters"));
    QJsonArray arr;
    QStringList lines;
    for (const QVariant &v : b.searchVault(q)) {
        const QVariantMap m = v.toMap();
        const QString folder = m.value(QStringLiteral("folder")).toString();
        const QString file = m.value(QStringLiteral("file")).toString();
        const QString path = folder.isEmpty() ? file : folder + u'/' + file;
        arr.append(QJsonObject{ { QStringLiteral("path"), path },
                                { QStringLiteral("folder"), folder },
                                { QStringLiteral("file"), file },
                                { QStringLiteral("title"), m.value(QStringLiteral("title")).toString() },
                                { QStringLiteral("snippet"), m.value(QStringLiteral("snippet")).toString() } });
        lines << QStringLiteral("%1\t%2\t%3").arg(path, m.value(QStringLiteral("title")).toString(),
                                                  m.value(QStringLiteral("snippet")).toString());
    }
    return Done{ arr, lines.join(u'\n') };
}

QString relPath(NotesBackend &b)
{
    return b.currentFolder().isEmpty() ? b.currentNote() : b.currentFolder() + u'/' + b.currentNote();
}

Result newNote(NotesBackend &b, const Args &a)
{
    QString folder = b.defaultFolder();
    if (!b.folders().contains(folder))
        folder.clear();
    if (const auto f = a.value("--folder"))
        if (auto fail = resolveFolder(b, *f, folder))
            return *fail;
    QString body;
    bool given = false;
    if (auto f = readBody(a, body, given))
        return *f;
    QString title = a.positional.at(0).trimmed();
    title.remove(u'/');
    title.remove(QChar(u'\0'));
    if (title.isEmpty())
        return usageError(QStringLiteral("the title is empty"));
    const QString file = title.endsWith(QStringLiteral(".md"), Qt::CaseInsensitive) ? title : title + QStringLiteral(".md");
    b.setCurrentFolder(folder);
    b.refresh();
    if (b.notes().contains(file) || QFile::exists(QDir(QDir(NotesBackend::rootPath()).filePath(folder)).filePath(file)))
        return Failure{ QStringLiteral("exists"),
                        QStringLiteral("a note named \"%1\" is already in %2").arg(file, folder.isEmpty() ? QStringLiteral("the vault root") : folder),
                        kExitError, QStringLiteral("Edit it with `icloud-notes write`, or pick another title.") };
    b.newNote(title);
    if (b.currentNote() != file)
        return Failure{ QStringLiteral("error"), QStringLiteral("could not create %1").arg(file), kExitError, {} };
    if (given) {
        const QString head = b.noteBody();
        const QString text = head.isEmpty() || head.endsWith(u'\n') ? head + body : head + u'\n' + body;
        if (!b.saveCurrentNote(text.endsWith(u'\n') ? text : text + u'\n'))
            return Failure{ QStringLiteral("error"), QStringLiteral("created %1 but could not write its text").arg(relPath(b)),
                            kExitError, {} };
    }
    return Done{ QJsonObject{ { QStringLiteral("action"), QStringLiteral("new") },
                              { QStringLiteral("path"), relPath(b) },
                              { QStringLiteral("folder"), b.currentFolder() },
                              { QStringLiteral("file"), b.currentNote() },
                              { QStringLiteral("title"), title } },
                 QStringLiteral("Created %1").arg(relPath(b)) };
}

Result write(NotesBackend &b, const Args &a)
{
    QString text;
    bool given = false;
    if (auto f = readBody(a, text, given))
        return *f;
    if (!given)
        return usageError(QStringLiteral("write needs the text: --body TEXT, --file PATH or --stdin"));
    Note n;
    if (auto f = resolveNote(b, a.positional.at(0), n))
        return *f;
    if (auto f = openNote(b, n))
        return *f;
    if (!b.readOnlyReason().isEmpty())
        return Failure{ QStringLiteral("read_only"), QStringLiteral("%1 is read-only here: this note %2").arg(n.path(), b.readOnlyReason()),
                        kExitError, QStringLiteral("Edit it in Apple Notes; the changes still sync here.") };
    QString body = text;
    if (a.has("--append")) {
        const QString old = b.noteBody();
        body = old.isEmpty() || old.endsWith(u'\n') ? old + text : old + u'\n' + text;
    }
    if (!body.isEmpty() && !body.endsWith(u'\n'))
        body += u'\n';
    const QString warning = b.saveWarning(body);
    if (!warning.isEmpty() && !a.has("--force"))
        return Failure{ QStringLiteral("guardrail"), warning, kExitError,
                        QStringLiteral("Nothing was written. Pass --force to write it anyway (as Ctrl+S does in Notes).") };
    const QString before = b.noteContent();
    if (!b.saveCurrentNote(body))
        return Failure{ QStringLiteral("error"), QStringLiteral("could not write %1").arg(n.path()), kExitError, {} };
    const bool changed = b.noteContent() != before;
    return Done{ QJsonObject{ { QStringLiteral("action"), QStringLiteral("write") },
                              { QStringLiteral("path"), relPath(b) },
                              { QStringLiteral("changed"), changed } },
                 changed ? QStringLiteral("Wrote %1").arg(relPath(b)) : QStringLiteral("%1 unchanged").arg(relPath(b)) };
}

// A backend error string as a failure with a code a caller can branch on.
Failure backendFailure(const QString &message)
{
    QString code = QStringLiteral("error");
    if (message.contains(QStringLiteral("read-only")))
        code = QStringLiteral("read_only");
    else if (message.contains(QStringLiteral("already exists")))
        code = QStringLiteral("exists");
    else if (message.contains(QStringLiteral("attachments")))
        code = QStringLiteral("has_attachments");
    return { code, message, kExitError, {} };
}

Result rename(NotesBackend &b, const Args &a)
{
    Note n;
    if (auto f = resolveNote(b, a.positional.at(0), n))
        return *f;
    if (auto f = openNote(b, n))
        return *f;
    const QString err = b.renameCurrentNote(a.positional.at(1));
    if (!err.isEmpty())
        return backendFailure(err);
    return Done{ QJsonObject{ { QStringLiteral("action"), QStringLiteral("rename") },
                              { QStringLiteral("path"), relPath(b) },
                              { QStringLiteral("from"), n.path() },
                              { QStringLiteral("title"), a.positional.at(1).trimmed() } },
                 QStringLiteral("Renamed %1 to \"%2\"").arg(n.path(), a.positional.at(1).trimmed()) };
}

Result move(NotesBackend &b, const Args &a)
{
    Note n;
    if (auto f = resolveNote(b, a.positional.at(0), n))
        return *f;
    QString folder;
    if (auto f = resolveFolder(b, a.positional.at(1), folder))
        return *f;
    if (auto f = openNote(b, n))
        return *f;
    const QString err = b.moveCurrentNote(folder);
    if (!err.isEmpty())
        return backendFailure(err);
    return Done{ QJsonObject{ { QStringLiteral("action"), QStringLiteral("move") },
                              { QStringLiteral("path"), relPath(b) },
                              { QStringLiteral("from"), n.path() } },
                 QStringLiteral("Moved %1 to %2").arg(n.path(), relPath(b)) };
}

Result deleteNote(NotesBackend &b, const Args &a)
{
    Note n;
    if (auto f = resolveNote(b, a.positional.at(0), n))
        return *f;
    if (auto f = confirm(a, QStringLiteral("Move \"%1\" to the trash? The next push moves it to Recently Deleted in iCloud.").arg(n.path())))
        return *f;
    if (auto f = openNote(b, n))
        return *f;
    const QString err = b.deleteCurrentNote();
    if (!err.isEmpty())
        return backendFailure(err);
    return Done{ QJsonObject{ { QStringLiteral("action"), QStringLiteral("delete") }, { QStringLiteral("path"), n.path() } },
                 QStringLiteral("Moved %1 to the trash").arg(n.path()) };
}

Result toggle(NotesBackend &b, const Args &a)
{
    bool ok = false;
    const int line = a.positional.at(1).toInt(&ok);
    if (!ok || line < 1)
        return usageError(QStringLiteral("LINE is a line number, 1 or more, not \"%1\"").arg(a.positional.at(1)));
    Note n;
    if (auto f = resolveNote(b, a.positional.at(0), n))
        return *f;
    if (auto f = openNote(b, n))
        return *f;
    if (!b.readOnlyReason().isEmpty())
        return backendFailure(QStringLiteral("This note is read-only here: it %1").arg(b.readOnlyReason()));
    const QString body = b.noteBody();
    const QString toggled = b.toggleCheckbox(body, line - 1);
    if (toggled == body)
        return Failure{ QStringLiteral("not_a_list_item"), QStringLiteral("line %1 of %2 is not a list item").arg(line).arg(n.path()),
                        kExitError, {} };
    if (!b.saveCurrentNote(toggled))
        return Failure{ QStringLiteral("error"), QStringLiteral("could not write %1").arg(n.path()), kExitError, {} };
    const QString text = b.noteBody().split(u'\n').value(line - 1);
    return Done{ QJsonObject{ { QStringLiteral("action"), QStringLiteral("toggle") },
                              { QStringLiteral("path"), relPath(b) },
                              { QStringLiteral("line"), line },
                              { QStringLiteral("text"), text } },
                 text };
}

Result resolve(NotesBackend &b, const Args &a)
{
    const auto all = a.value("--all");
    const auto choices = a.value("--choices");
    if (all.has_value() == choices.has_value())
        return usageError(QStringLiteral("resolve needs --all local|remote|both or --choices C1,C2,..."));
    Note n;
    if (auto f = resolveNote(b, a.positional.at(0), n))
        return *f;
    if (auto f = openNote(b, n))
        return *f;
    if (b.noteConflictsUnreadable())
        return Failure{ QStringLiteral("conflicts_unreadable"),
                        QStringLiteral("%1 has conflict markers the version picker cannot read").arg(n.path()), kExitError,
                        QStringLiteral("Use `icloud-notes recover NOTE --strip` (keep every line) or --synced (the last synced text).") };
    const qsizetype blocks = b.noteConflicts().size();
    if (blocks == 0)
        return Failure{ QStringLiteral("no_conflicts"), QStringLiteral("%1 has no conflict to resolve").arg(n.path()), kExitError, {} };
    QStringList picked = all ? QStringList(blocks, all->trimmed()) : choices->split(u',', Qt::SkipEmptyParts);
    for (QString &c : picked)
        c = c.trimmed();
    for (const QString &c : picked)
        if (c != u"local" && c != u"remote" && c != u"both")
            return usageError(QStringLiteral("a choice is local, remote or both, not \"%1\"").arg(c));
    if (picked.size() != blocks)
        return Failure{ QStringLiteral("choices_mismatch"),
                        QStringLiteral("%1 has %2 conflict blocks, and %3 choices were given").arg(n.path()).arg(blocks).arg(picked.size()),
                        kExitUsage, QStringLiteral("`icloud-notes read NOTE --json` lists the blocks.") };
    const QString err = b.resolveConflicts(picked);
    if (!err.isEmpty())
        return backendFailure(err);
    return Done{ QJsonObject{ { QStringLiteral("action"), QStringLiteral("resolve") },
                              { QStringLiteral("path"), relPath(b) },
                              { QStringLiteral("choices"), QJsonArray::fromStringList(picked) } },
                 QStringLiteral("Resolved %1 conflict block(s) in %2").arg(blocks).arg(relPath(b)) };
}

Result recover(NotesBackend &b, const Args &a)
{
    if (a.has("--strip") == a.has("--synced"))
        return usageError(QStringLiteral("recover needs --strip or --synced"));
    const QString how = a.has("--strip") ? QStringLiteral("strip") : QStringLiteral("synced");
    Note n;
    if (auto f = resolveNote(b, a.positional.at(0), n))
        return *f;
    if (auto f = openNote(b, n))
        return *f;
    const QVariantMap r = b.recoverConflictedNote(how);
    const QString message = r.value(QStringLiteral("message")).toString();
    if (!r.value(QStringLiteral("ok")).toBool()) {
        Failure f = backendFailure(message);
        if (message.contains(QStringLiteral("no unreadable")))
            f.code = QStringLiteral("no_conflicts");
        else if (message.contains(QStringLiteral("no last synced")))
            f.code = QStringLiteral("no_synced_copy");
        return f;
    }
    return Done{ QJsonObject{ { QStringLiteral("action"), QStringLiteral("recover") },
                              { QStringLiteral("path"), relPath(b) },
                              { QStringLiteral("how"), how },
                              { QStringLiteral("backup"), r.value(QStringLiteral("backup")).toString() },
                              { QStringLiteral("message"), message } },
                 message };
}

Result exportPdf(NotesBackend &b, const Args &a)
{
    Note n;
    if (auto f = resolveNote(b, a.positional.at(0), n))
        return *f;
    if (auto f = openNote(b, n))
        return *f;
    const QString err = b.exportPdf();
    if (!err.isEmpty())
        return Failure{ err.startsWith(QStringLiteral("Already exists")) ? QStringLiteral("exists") : QStringLiteral("error"), err,
                        kExitError, {} };
    const QString pdf = QDir(QDir(NotesBackend::rootPath()).filePath(n.folder)).filePath(n.file.chopped(3) + QStringLiteral(".pdf"));
    return Done{ QJsonObject{ { QStringLiteral("action"), QStringLiteral("export-pdf") },
                              { QStringLiteral("path"), n.path() },
                              { QStringLiteral("pdf"), pdf } },
                 pdf };
}

Result newFolder(NotesBackend &b, const Args &a)
{
    QString parent;
    if (const auto p = a.value("--in"))
        if (auto f = resolveFolder(b, *p, parent))
            return *f;
    QString name = a.positional.at(0).trimmed();
    name.remove(u'/');
    if (name.isEmpty())
        return usageError(QStringLiteral("the folder name is empty"));
    const QString folder = parent.isEmpty() ? name : parent + u'/' + name;
    if (b.folders().contains(folder))
        return Failure{ QStringLiteral("exists"), QStringLiteral("folder \"%1\" already exists").arg(folder), kExitError, {} };
    b.setCurrentFolder(parent);
    b.newFolder(name);
    if (!b.folders().contains(folder))
        return Failure{ QStringLiteral("error"), QStringLiteral("could not create folder \"%1\"").arg(folder), kExitError, {} };
    return Done{ QJsonObject{ { QStringLiteral("action"), QStringLiteral("new-folder") }, { QStringLiteral("folder"), folder } },
                 QStringLiteral("Created folder %1").arg(folder) };
}

Result renameFolder(NotesBackend &b, const Args &a)
{
    QString folder;
    if (auto f = resolveFolder(b, a.positional.at(0), folder))
        return *f;
    b.setCurrentFolder(folder);
    const QString err = b.renameCurrentFolder(a.positional.at(1));
    if (!err.isEmpty())
        return backendFailure(err);
    return Done{ QJsonObject{ { QStringLiteral("action"), QStringLiteral("rename-folder") },
                              { QStringLiteral("folder"), b.currentFolder() },
                              { QStringLiteral("from"), folder } },
                 QStringLiteral("Renamed folder %1 to %2").arg(folder, b.currentFolder()) };
}

Result deleteFolder(NotesBackend &b, const Args &a)
{
    QString folder;
    if (auto f = resolveFolder(b, a.positional.at(0), folder))
        return *f;
    if (folder.isEmpty())
        return backendFailure(QStringLiteral("All Notes cannot be deleted."));
    const int count = b.folderNoteCounts().value(folder).toInt();
    if (auto f = confirm(a, QStringLiteral("Move \"%1\" and its %2 note(s) to the trash? The next push moves the notes to "
                                           "Recently Deleted in iCloud.")
                                .arg(folder)
                                .arg(count)))
        return *f;
    b.setCurrentFolder(folder);
    const QString err = b.deleteCurrentFolder();
    if (!err.isEmpty())
        return backendFailure(err);
    return Done{ QJsonObject{ { QStringLiteral("action"), QStringLiteral("delete-folder") }, { QStringLiteral("folder"), folder } },
                 QStringLiteral("Moved folder %1 to the trash").arg(folder) };
}

int syncCommand(NotesBackend &b, const Args &a, const QString &what)
{
    if (what == u"clone") {
        if (NotesBackend::vaultCloned())
            return report(Failure{ QStringLiteral("already_cloned"),
                                   QStringLiteral("%1 is already cloned").arg(NotesBackend::rootPath()), kExitError,
                                   QStringLiteral("Run `icloud-notes pull` to fetch what changed.") },
                          a.json);
        QDir().mkpath(NotesBackend::rootPath());
    } else if (auto f = needCloned()) {
        return report(*f, a.json);
    }
    if (auto f = needSyncTool(b))
        return report(*f, a.json);
    if (auto f = takeLock(b, a))
        return report(*f, a.json);
    const SyncOutcome sync = runSync(b, what, a.json);
    if (sync.json.isEmpty() && sync.failure)
        return report(*sync.failure, a.json);
    print(a.json, sync.json, sync.text);
    return sync.failure ? report(*sync.failure, a.json) : kExitOk;
}

int runCommand(const Args &a, const Spec &spec)
{
    if (const auto vault = a.value("--vault")) {
        if (vault->trimmed().isEmpty())
            return report(usageError(QStringLiteral("--vault needs a directory")), a.json);
        qputenv("ICLOUD_NOTES_VAULT", QFileInfo(*vault).absoluteFilePath().toUtf8());
    }
    NotesBackend b(nullptr, NotesBackend::Role::Cli);
    const QString cmd = QLatin1StringView(spec.name);

    if (cmd == u"status") {
        int code = kExitOk;
        const Done d = status(b, code);
        print(a.json, d.json, d.text);
        return code;
    }
    if (cmd == u"sync" || cmd == u"pull" || cmd == u"clone" || (cmd == u"push" && !a.has("--dry-run")))
        return syncCommand(b, a, cmd);
    if (auto f = needCloned())
        return report(*f, a.json);

    // icloud-notes-sync's own commands, on a resolved note.
    if (cmd == u"push")
        return execEngine(b, a, { QStringLiteral("push"), QStringLiteral("--dry-run") });
    if (cmd == u"history" || cmd == u"diff" || cmd == u"restore") {
        Note n;
        if (auto f = resolveNote(b, a.positional.at(0), n))
            return report(*f, a.json);
        if (cmd == u"restore")
            if (auto f = confirm(a, QStringLiteral("Throw away the local edits to \"%1\" and go back to the last synced copy?").arg(n.path())))
                return report(*f, a.json);
        QStringList args{ cmd, n.path() };
        if (cmd == u"diff")
            args << a.positional.at(1);
        if (cmd == u"history" && a.has("--records"))
            args << QStringLiteral("--records");
        return execEngine(b, a, args);
    }

    // Reading needs no lock.
    auto show = [&](Result r) -> int {
        if (std::holds_alternative<Failure>(r))
            return report(std::get<Failure>(r), a.json);
        const Done &d = std::get<Done>(r);
        print(a.json, d.json, d.text);
        return kExitOk;
    };
    if (cmd == u"folders")
        return show(folders(b));
    if (cmd == u"list")
        return show(list(b, a));
    if (cmd == u"read")
        return show(read(b, a));
    if (cmd == u"search")
        return show(search(b, a));

    // Changing the vault: under its lock, so no sync runs meanwhile.
    if (auto f = takeLock(b, a))
        return report(*f, a.json);
    b.refresh();
    if (cmd == u"new")
        return finish(b, a, newNote(b, a));
    if (cmd == u"write")
        return finish(b, a, write(b, a));
    if (cmd == u"rename")
        return finish(b, a, rename(b, a));
    if (cmd == u"move")
        return finish(b, a, move(b, a));
    if (cmd == u"delete")
        return finish(b, a, deleteNote(b, a));
    if (cmd == u"toggle")
        return finish(b, a, toggle(b, a));
    if (cmd == u"resolve")
        return finish(b, a, resolve(b, a));
    if (cmd == u"recover")
        return finish(b, a, recover(b, a));
    if (cmd == u"export-pdf")
        return finish(b, a, exportPdf(b, a));
    if (cmd == u"new-folder")
        return finish(b, a, newFolder(b, a));
    if (cmd == u"rename-folder")
        return finish(b, a, renameFolder(b, a));
    if (cmd == u"delete-folder")
        return finish(b, a, deleteFolder(b, a));
    return report(usageError(QStringLiteral("unknown command \"%1\"").arg(cmd)), a.json);
}

} // namespace

bool isCliInvocation(const char *arg)
{
    const QString a = QString::fromLocal8Bit(arg);
    if (a == u"--sync")
        return false;
    static const QSet<QString> starts{ QStringLiteral("help"),    QStringLiteral("-h"),     QStringLiteral("--help"),
                                       QStringLiteral("-V"),      QStringLiteral("--version"), QStringLiteral("--json"),
                                       QStringLiteral("--vault"), QStringLiteral("--wait") };
    return starts.contains(a) || a.startsWith(QStringLiteral("--vault=")) || a.startsWith(QStringLiteral("--wait="))
        || findSpec(a) != nullptr;
}

int cliMain(int argc, char *argv[])
{
    QStringList argvList;
    for (int i = 1; i < argc; ++i)
        argvList << QString::fromLocal8Bit(argv[i]);
    Args a;
    const std::optional<Failure> parseFailure = parseArgs(argvList, a);
    const bool json = argvList.contains(QStringLiteral("--json"));

    // export-pdf lays out text, which needs a GUI application; on the
    // offscreen platform it needs no display.
    std::unique_ptr<QCoreApplication> app;
    if (a.command == u"export-pdf") {
        if (qEnvironmentVariableIsEmpty("QT_QPA_PLATFORM"))
            qputenv("QT_QPA_PLATFORM", "offscreen");
        app = std::make_unique<QGuiApplication>(argc, argv);
    } else {
        app = std::make_unique<QCoreApplication>(argc, argv);
    }
    app->setOrganizationName(QStringLiteral("icloud-notes"));
    app->setApplicationName(QStringLiteral("icloud-notes"));

    if (parseFailure)
        return report(*parseFailure, json);
    if (a.command == u"help" || (a.command.isEmpty() && a.has("--help"))) {
        const Spec *spec = a.positional.isEmpty() ? nullptr : findSpec(a.positional.first());
        if (!a.positional.isEmpty() && !spec)
            return report(usageError(QStringLiteral("unknown command \"%1\"").arg(a.positional.first())), a.json);
        out() << (spec ? commandHelp(*spec) : usageText()) << '\n';
        return kExitOk;
    }
    if (a.command.isEmpty() && a.has("--version")) {
        const QString version = QStringLiteral(ICLOUD_NOTES_VERSION);
        if (a.json)
            printJson(QJsonObject{ { QStringLiteral("version"), version } });
        else
            out() << "icloud-notes " << version << '\n';
        return kExitOk;
    }
    if (a.command.isEmpty())
        return report(usageError(QStringLiteral("no command given (`icloud-notes help` lists them)")), a.json);
    const Spec *spec = findSpec(a.command);
    if (!spec)
        return report(usageError(QStringLiteral("unknown command \"%1\" (`icloud-notes help` lists them)").arg(a.command)), a.json);
    if (a.has("--help")) {
        out() << commandHelp(*spec) << '\n';
        return kExitOk;
    }
    if (auto f = checkArgs(*spec, a))
        return report(*f, a.json);
    return runCommand(a, *spec);
}

int runBackgroundSync(QTextStream &log)
{
    const QString vault = NotesBackend::rootPath();
    if (!NotesBackend::vaultCloned()) {
        log << "No notes cloned at " << vault << "; nothing to sync.\n";
        return kExitOk;
    }
    // Never waits for the lock: whoever holds it syncs this vault already.
    NotesBackend b(nullptr, NotesBackend::Role::Background);
    switch (b.lockVault()) {
    case VaultLock::Locked:
        break;
    case VaultLock::Busy:
        log << "Notes is open (it syncs on its own) or another sync is running (" << b.lockHolder() << "); skipped.\n";
        return kExitOk;
    case VaultLock::Failed:
        log << "Could not open the sync lock " << NotesBackend::lockPath() << "; not syncing.\n";
        return kExitError;
    }
    const SyncOutcome sync = runSync(b, QStringLiteral("sync"), false);
    // Nothing ran: signed out or the sign-in unknown is a skip (the timer
    // tries again), a missing engine an error.
    if (sync.failure && sync.json.isEmpty()) {
        const Failure &f = *sync.failure;
        const bool skip = f.code == u"sign_in_required" || f.code == u"session_unavailable";
        log << f.message << (skip ? QStringLiteral(" Skipped.") : u' ' + f.hint) << '\n';
        return skip ? kExitOk : kExitError;
    }
    log << sync.text << '\n';
    if (sync.failure && sync.failure->code == u"sign_in_required") {
        log << "iCloud refused the sign-in; icloud-session was told. Sync paused until you sign in.\n";
        return kExitError;
    }
    log << (sync.failure ? "Sync failed for " : "Synced ") << vault << ".\n";
    return sync.failure ? kExitError : kExitOk;
}
