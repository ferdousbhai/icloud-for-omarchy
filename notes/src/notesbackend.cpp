#include "notesbackend.h"
#include "syncmodel.h"

#include <QClipboard>
#include <QCryptographicHash>
#include <QDBusConnection>
#include <QDBusMessage>
#include <QDBusPendingCallWatcher>
#include <QDBusPendingReply>
#include <QDBusServiceWatcher>
#include <QDateTime>
#include <QDir>
#include <QDirIterator>
#include <QElapsedTimer>
#include <QFile>
#include <QFileInfo>
#include <QFontDatabase>
#include <QGuiApplication>
#include <QImage>
#include <QJsonArray>
#include <QJsonDocument>
#include <QJsonObject>
#include <QMimeData>
#include <QPrinter>
#include <QRegularExpression>
#include <QSaveFile>
#include <QScopeGuard>
#include <QScopedValueRollback>
#include <QStandardPaths>
#include <QTextDocument>
#include <QTextStream>
#include <QThread>
#include <QUrl>
#include <algorithm>
#include <cerrno>
#include <cstdio>
#include <cstring>
#include <fcntl.h>
#include <optional>
#include <utility>

namespace {

// icloud-session's D-Bus contract: the daemon owns the Apple account for
// every iCloud app, and announces changes with PropertiesChanged.
const QString kSessionService = QStringLiteral("io.github.ferdousbhai.ICloudSession");
const QString kSessionPath = QStringLiteral("/io/github/ferdousbhai/ICloudSession");
const QString kPropertiesInterface = QStringLiteral("org.freedesktop.DBus.Properties");
// Long enough for D-Bus activation to start the daemon; never blocks the UI.
constexpr int kSessionTimeoutMs = 10000;
// ReportSignInRequired checks the session with Apple before it answers.
constexpr int kReportTimeoutMs = 60000;

const QString kPausedMessage = QStringLiteral("Sync paused. Sign in to iCloud to resume.");
// No network (or iCloud unreachable): not a failure to act on. The next
// sync (the poll, a focus, the timer) tries again.
const QString kOfflineMessage = QStringLiteral("Offline. Notes syncs again once iCloud can be reached.");

// The sync engine: icloud-notes-sync (originally derived from icloud-md); it takes
// the sign-in from icloud-session. The icloud-notes package installs it
// off PATH, in kSyncToolPath; see NotesBackend::syncToolPath.
constexpr char kSyncTool[] = "icloud-notes-sync";
constexpr char kSyncToolPath[] = "/usr/lib/icloud-notes/icloud-notes-sync";
const QString kChangedOnDiskMessage = QStringLiteral("The note changed on disk. Look it over, then try again.");
const QString kNotInstalledMessage =
    QStringLiteral("The sync engine (icloud-notes-sync) is missing. Reinstall icloud-notes to sync.");

// A note larger than this is not a note anymore; the guardrail scans
// stop here so a stray huge file cannot stall the list.
constexpr qsizetype kScanLimit = 2 * 1024 * 1024;

// How long an engine that failed to answer vault-info is left alone before
// it is asked again (the answer is asked for on every re-read of the vault).
constexpr int kVaultInfoRetryMs = 2000;

QString readText(const QString &path, qsizetype limit = -1)
{
    QFile file(path);
    if (!file.open(QIODevice::ReadOnly | QIODevice::Text))
        return {};
    QTextStream in(&file);
    in.setEncoding(QStringConverter::Utf8);
    return limit < 0 ? in.readAll() : in.read(limit);
}

// Whole or not at all: the text goes to a temporary file that replaces the
// note only once every byte is written, so a full disk never truncates it.
bool writeText(const QString &path, const QString &text)
{
    QSaveFile file(path);
    if (!file.open(QIODevice::WriteOnly | QIODevice::Text))
        return false;
    QTextStream out(&file);
    out.setEncoding(QStringConverter::Utf8);
    out << text;
    out.flush();
    if (out.status() != QTextStream::Ok) {
        file.cancelWriting();
        return false;
    }
    return file.commit();
}

// A file's mtime and size, "-" when there is none: what tells a changed
// state file from the one last read.
QString fileStamp(const QString &path)
{
    const QFileInfo file(path);
    return file.exists() ? QString::number(file.lastModified().toMSecsSinceEpoch()) + u':' + QString::number(file.size())
                         : QStringLiteral("-");
}

// Dot-directories (the sync tool's bookkeeping, .git) hold no notes.
bool isHidden(const QString &relativePath)
{
    for (const QStringView part : QStringView(relativePath).split(u'/')) {
        if (part.startsWith(u'.'))
            return true;
    }
    return false;
}

// Attachment bundles are data owned by notes, even when a file ends in .md.
bool isNotePath(const QString &relativePath)
{
    return !isHidden(relativePath)
        && !relativePath.split(u'/').contains(QStringLiteral("attachments"), Qt::CaseInsensitive);
}

// Every note file under `dirPath`, subfolders included: fn(its path relative
// to dirPath, its absolute path), until fn returns false.
template <typename Fn>
void forEachNoteFile(const QString &dirPath, Fn fn)
{
    const QDir dir(dirPath);
    QDirIterator files(dirPath, { QStringLiteral("*.md") }, QDir::Files, QDirIterator::Subdirectories);
    while (files.hasNext()) {
        const QString path = files.next();
        const QString rel = dir.relativeFilePath(path);
        if (isNotePath(rel) && !fn(rel, path))
            return;
    }
}

// Follow the desktop text size like Omawrite does; 1.0 off GNOME
// (Omarchy sets text-scaling-factor from its display panel).
double readUiScale()
{
    QProcess gsettings;
    gsettings.start(QStringLiteral("gsettings"),
                    { QStringLiteral("get"), QStringLiteral("org.gnome.desktop.interface"),
                      QStringLiteral("text-scaling-factor") });
    bool ok = false;
    double v = 1.0;
    if (gsettings.waitForFinished(2000) && gsettings.exitCode() == 0)
        v = QString::fromUtf8(gsettings.readAllStandardOutput()).trimmed().toDouble(&ok);
    return ok && v >= 0.5 && v <= 3.0 ? v : 1.0;
}

// The files a note links to under attachments/. The sync tool keeps one
// attachments/ directory per folder, shared by every note in it and named
// after the attachment, and always rewrites each attachment into the note
// text as a link (an embed for images), so the links are the full list.
// The links themselves must stay as written: each stands for its attachment,
// and one gone from the text removes the attachment at the next push.
// A link: its text (brackets in it escaped), then (attachments/...).
const QRegularExpression &attachmentLinkRe()
{
    static const QRegularExpression re(QStringLiteral(R"(!?\[(?:\\.|[^\]\\\n])*\]\((attachments/(?:[^()\s]|\((?:[^()\s]|\([^()\s]*\))*\))+)\))"));
    return re;
}

QVariantList attachmentsFor(const QString &notePath, const QString &text)
{
    static const QStringList imageSuffixes{ QStringLiteral("png"), QStringLiteral("jpg"),
                                            QStringLiteral("jpeg"), QStringLiteral("gif"),
                                            QStringLiteral("webp"), QStringLiteral("svg") };
    const QDir dir = QFileInfo(notePath).dir();
    // Only files really inside that directory: a link's `..` or a symlink
    // there could otherwise point the preview (and `read`) anywhere.
    const QString inside = QFileInfo(dir.filePath(QStringLiteral("attachments"))).canonicalFilePath() + u'/';
    QVariantList found;
    QSet<QString> seen;
    for (const QRegularExpressionMatch &match : attachmentLinkRe().globalMatch(text)) {
        // Links are URL-encoded.
        const QString abs = QDir::cleanPath(dir.absoluteFilePath(QUrl(match.captured(1)).path()));
        const QString real = QFileInfo(abs).canonicalFilePath(); // empty when it does not exist
        if (inside.size() == 1 || !real.startsWith(inside) || seen.contains(abs))
            continue;
        seen.insert(abs);
        const QFileInfo info(abs);
        found << QVariantMap{ { QStringLiteral("name"), info.fileName() },
                              { QStringLiteral("url"), QUrl::fromLocalFile(abs).toString() },
                              { QStringLiteral("image"), imageSuffixes.contains(info.suffix().toLower()) } };
    }
    return found;
}

// Omarchy resolves the active theme's palette into this file on every
// theme change: simple `key = "#rrggbb"` lines, plus mode = "dark"|"light".
QString themeColorsPath()
{
    return QDir::homePath() + QStringLiteral("/.local/state/omarchy/current/theme/colors.toml");
}

// Toolbar glyphs come from a Nerd Font; Omarchy ships several. Prefer the
// application's own font when it is one, so icons and text match.
QString findIconFont()
{
    const QString appFont = QGuiApplication::font().family();
    if (appFont.contains(QStringLiteral("Nerd Font")))
        return appFont;
    // "Nerd Font Mono" squeezes glyphs to one cell and "Propo" is the
    // proportional cut; the plain family draws icons at their full width.
    for (const QString &family : QFontDatabase::families()) {
        if (family.endsWith(QStringLiteral("Nerd Font")))
            return family;
    }
    return {};
}

// The contract error object `{"code","message","hint"?}` when `text` (the
// engine's whole stdout, pretty-printed over several lines, or one line) is
// the document `{"ok":false,"error":{...}}`, else empty. Parsed as JSON: key
// order, layout and surrounding whitespace are not part of the contract.
// The `{` check keeps the per-line cost negligible for plain log lines.
QJsonObject contractError(QByteArrayView text)
{
    const QByteArray trimmed = text.toByteArray().trimmed();
    if (!trimmed.startsWith('{'))
        return {};
    const QJsonObject object = QJsonDocument::fromJson(trimmed).object();
    const QJsonValue ok = object.value(QStringLiteral("ok"));
    if (!ok.isBool() || ok.toBool())
        return {};
    return object.value(QStringLiteral("error")).toObject();
}

// The engine's error for a failed run: in its machine modes (--json,
// --quiet) the engine prints it on stdout as the contract error document (a
// pull, push, clone, diff, status or history, or a sync that gave no
// answer); a half of runSync carries it as {"error":{...}}. Empty when
// neither is there (a crash).
QJsonObject engineError(const QByteArray &output)
{
    const QJsonObject error = contractError(output);
    return error.isEmpty() ? QJsonDocument::fromJson(output).object().value(QStringLiteral("error")).toObject() : error;
}

// The JSON envelope {"ok":true,"data","summary","meta"?} a `--json` run
// prints on success, else empty.
QJsonObject successEnvelope(const QByteArray &output)
{
    const QJsonObject object = QJsonDocument::fromJson(output.trimmed()).object();
    const QJsonValue ok = object.value(QStringLiteral("ok"));
    return ok.isBool() && ok.toBool() ? object : QJsonObject();
}

// The log's lines for a failed run or half (`name`: "push", "sync"): the
// error's message, then its hint, when it has one, on a line of its own.
QString failedLog(const QString &name, const QJsonObject &error)
{
    const QString hint = error.value(QStringLiteral("hint")).toString();
    return name + QStringLiteral(" failed: ") + error.value(QStringLiteral("message")).toString()
        + (hint.isEmpty() ? QString() : u'\n' + hint);
}

// The strings of a JSON array (a report's lines).
QStringList strings(const QJsonValue &array)
{
    QStringList out;
    for (const QJsonValue &value : array.toArray())
        out << value.toString();
    return out;
}

// The error of a push whose result holds a change that failed (iCloud
// refused it, or something it depends on failed): `push_rejected`, with the
// first one's message. Empty when the result has none.
QJsonObject refusedChange(const QJsonObject &pushResult)
{
    QStringList messages;
    for (const QJsonValue &entry : pushResult.value(QStringLiteral("entries")).toArray()) {
        const QJsonValue outcome = entry.toObject().value(QStringLiteral("outcome"));
        if (outcome.isObject() && !outcome.toObject().value(QStringLiteral("succeeded")).toBool())
            messages << outcome.toObject().value(QStringLiteral("message")).toString();
    }
    if (messages.isEmpty())
        return {};
    return QJsonObject{ { QStringLiteral("code"), QStringLiteral("push_rejected") },
                        { QStringLiteral("message"), messages.size() == 1
                                                         ? messages.first()
                                                         : QStringLiteral("%1 (and %2 more; see the log)")
                                                               .arg(messages.first())
                                                               .arg(messages.size() - 1) } };
}

// Only a sign-in fixes it: the engine's error code says so. Exit 3 also
// means "not cloned", "another account's vault" and the like, so the exit
// code counts only when no error code came through at all.
bool isSignInRequired(const QString &code, int exitCode)
{
    return code.isEmpty() ? exitCode == 3 : code == u"sign_in_required";
}

// Apple refused the session icloud-session still holds (a 401/421 that
// persisted after icloud-session revalidated it): only a sign-in fixes it
// too, but icloud-session was told already, by the session library.
bool isSessionRejected(const QString &code)
{
    return code == u"session_rejected";
}

// Either: the engine's sync skips its pull after it, and the app pauses.
bool needsSignIn(const QString &code, int exitCode)
{
    return isSignInRequired(code, exitCode) || isSessionRejected(code);
}

// iCloud out of reach: the engine's network errors (icloud-session's
// `offline` comes at once when there is no network at all).
bool isNetworkError(const QString &code)
{
    return code == u"offline" || code == u"network";
}

// The log's lines for a failed run or half: iCloud out of reach is one line
// and no failure to act on, anything else failedLog's.
QString runFailedLog(const QString &name, const QJsonObject &error)
{
    if (isNetworkError(error.value(QStringLiteral("code")).toString()))
        return QStringLiteral("%1: iCloud could not be reached (%2); the next sync tries again.")
            .arg(name, error.value(QStringLiteral("message")).toString());
    return failedLog(name, error);
}

} // namespace

NotesBackend::NotesBackend(QObject *parent, Role role)
    : QObject(parent), m_role(role), m_lock(lockPath()), m_uiScale(role == Role::App ? readUiScale() : 1.0),
      m_iconFont(role == Role::App ? findIconFont() : QString())
{
    if (m_role == Role::App) {
        QDir().mkpath(rootPath());

        // A theme change rewrites colors.toml (or the directory holding it);
        // re-read and re-arm the watch, since a replaced file drops out of it.
        loadTheme();
        connect(&m_themeWatcher, &QFileSystemWatcher::fileChanged, this, &NotesBackend::loadTheme);
        connect(&m_themeWatcher, &QFileSystemWatcher::directoryChanged, this, &NotesBackend::loadTheme);

        // The vault's lock, held until the app exits so a background sync
        // never runs icloud-notes-sync beside it. While one holds it, the window
        // opens anyway and its syncs wait for it (see startProcess).
        switch (lockVault()) {
        case VaultLock::Locked:
            break;
        case VaultLock::Busy:
            appendLog(QStringLiteral("%1 has the notes; syncing here waits for it.").arg(lockHolder()));
            m_lockRetry.start();
            break;
        case VaultLock::Failed:
            appendLog(QStringLiteral("Could not open the sync lock %1; nothing syncs until it opens.").arg(m_lock.path()));
            break;
        }
    }
    connect(&m_lockRetry, &QTimer::timeout, this, &NotesBackend::retryLock);
    m_lockRetry.setInterval(500);
    m_semanticDelay.setSingleShot(true);

    // External changes (an icloud-notes-sync pull in a terminal, say) re-list;
    // a change to the open note is reported so unsaved edits are kept.
    connect(&m_watcher, &QFileSystemWatcher::directoryChanged, this, [this] {
        // runSync re-reads the vault once, after it, rather than on every
        // file its pull writes.
        if (m_syncRunning && m_mode == Mode::Sync) {
            m_vaultTouched = true;
            return;
        }
        rebuildFolders();
        rebuildNotes();
        rewatch();
        if (!m_syncRunning) // a pull's own writes are not local changes
            emit vaultChanged();
    });
    connect(&m_watcher, &QFileSystemWatcher::fileChanged, this, [this](const QString &path) {
        // Our own saves come back through here too; only a real difference counts.
        if (!m_currentNote.isEmpty() && path == noteAbsolutePath() && readText(path) != m_noteContent)
            emit currentNoteChangedOnDisk();
        if (!m_syncRunning)
            emit vaultChanged();
    });

    // Separate channels: every run is in a machine mode (--json or --quiet),
    // so stdout carries only JSON (parsed, never logged as it is), and
    // progress and warnings go to stderr (logged). The run's text report is
    // logged once it is done: pull, push and clone's meta.lines, diff's
    // meta.text (finishRun), each half's lines of runSync
    // (finishSyncHalves), or the error's message and hint. Stderr is read a
    // line at a time in every mode, leaving out the progress lines (one per
    // fetched page and processed note in a machine mode: thousands in a
    // clone) and empty lines.
    connect(&m_syncProcess, &QProcess::readyReadStandardOutput, this,
            [this] { m_captured += m_syncProcess.readAllStandardOutput(); });
    connect(&m_syncProcess, &QProcess::readyReadStandardError, this, [this] {
        m_errLine += m_syncProcess.readAllStandardError();
        QStringList lines;
        for (qsizetype nl; (nl = m_errLine.indexOf('\n')) >= 0;) {
            const QByteArray line = m_errLine.left(nl);
            m_errLine.remove(0, nl + 1);
            if (isLoggedStderrLine(line))
                lines << QString::fromUtf8(line);
        }
        if (!lines.isEmpty())
            appendLog(lines.join(u'\n'));
    });
    connect(&m_syncProcess, &QProcess::finished, this, [this](int exitCode) { finishSync(exitCode); });
    connect(&m_syncProcess, &QProcess::errorOccurred, this, [this](QProcess::ProcessError error) {
        if (error != QProcess::FailedToStart)
            return; // finished() follows for every other error
        appendLog(QStringLiteral("Failed to start %1: ").arg(QLatin1StringView(kSyncTool)) + m_syncProcess.errorString());
        finishSync(-1);
    });

    // Sign-in state comes from icloud-session over the session bus: one
    // read now (which also starts the daemon), then its PropertiesChanged
    // signals. Without the daemon the sign-in is simply unknown.
    QDBusConnection bus = QDBusConnection::sessionBus();
    if (bus.isConnected()) {
        bus.connect(kSessionService, kSessionPath, kPropertiesInterface, QStringLiteral("PropertiesChanged"), this,
                    SLOT(sessionPropertiesChanged(QString, QVariantMap, QStringList)));
        m_sessionWatcher = new QDBusServiceWatcher(kSessionService, bus,
                                                   QDBusServiceWatcher::WatchForRegistration, this);
        // Started later (installed, or restarted): read it afresh. When it
        // exits while idle, what it last said still holds.
        connect(m_sessionWatcher, &QDBusServiceWatcher::serviceRegistered, this, &NotesBackend::refreshSignIn);
    }
    if (m_role != Role::Cli) // a command that needs the sign-in reads it itself
        refreshSignIn();
    refresh();
    setSyncMessage(!syncToolAvailable() ? kNotInstalledMessage
                   : cloned()           ? QStringLiteral("Ready.")
                                        : QStringLiteral("Not linked to iCloud yet. Press Clone."));
}

void NotesBackend::loadTheme()
{
    QVariantMap theme;
    for (const QString &line : readText(themeColorsPath()).split(u'\n')) {
        const qsizetype eq = line.indexOf(u'=');
        if (eq < 0 || line.trimmed().startsWith(u'#'))
            continue;
        QString value = line.mid(eq + 1).trimmed();
        if (value.size() >= 2 && value.front() == u'"' && value.back() == u'"')
            value = value.mid(1, value.size() - 2);
        theme.insert(line.left(eq).trimmed(), value);
    }
    if (theme != m_theme) {
        m_theme = theme;
        emit themeChanged();
    }
    const QString dir = QFileInfo(themeColorsPath()).absolutePath();
    if (QDir(dir).exists() && !m_themeWatcher.directories().contains(dir))
        m_themeWatcher.addPath(dir);
    if (QFile::exists(themeColorsPath()) && !m_themeWatcher.files().contains(themeColorsPath()))
        m_themeWatcher.addPath(themeColorsPath());
}

// The vault. ICLOUD_NOTES_VAULT overrides it, which is how the tests and the
// screenshot tool work on a scratch directory: Qt's test mode leaves
// DocumentsLocation alone, so without this they would hit the real notes.
// Otherwise <Documents>/Notes, resolved once per process: the window, the
// command line and `icloud-notes --sync` all come through here before
// anything opens the vault (main() asks for lockPath() first), which makes
// this the one place a vault an earlier release left behind is moved.
QString NotesBackend::rootPath()
{
    const QString override = qEnvironmentVariable("ICLOUD_NOTES_VAULT");
    if (!override.isEmpty())
        return override;
    static const QString vault =
        moveOldVault(QStandardPaths::writableLocation(QStandardPaths::DocumentsLocation));
    return vault;
}

// Earlier releases kept the vault in <Documents>/icloud-notes. It moves by
// one rename() within <Documents> (one filesystem: nothing is copied), and
// from then on only <Documents>/Notes is read. Nothing in or about the
// vault records where it is (the engine's state is vault-relative, the lock
// and the one-window key are derived from the path each start), so the
// rename is all there is to it. The move holds the old vault's lock, so it
// never pulls the vault from under a run that has it open: a Notes window
// of the earlier release still running after the update, its engine, or a
// second process moving it at the same moment (which, once it has the
// lock, finds the old folder gone). "Notes" is a common folder name, so one
// already there is looked at: empty, it is removed and the vault takes its
// place; a folder of the user's own (not a vault) while the old folder is
// one, the vault stays where it is, and the window shows why (the
// vaultNotice banner); both vaults, Notes wins and the old one is named for
// merging by hand. While the old vault stays busy past busyWaitMs, Notes
// is someone else's folder, or the rename fails, this process keeps to the
// old folder and the next start tries again: the move is put off, never
// half done. RENAME_NOREPLACE leaves a Notes folder that appeared meanwhile
// alone.
QString NotesBackend::moveOldVault(const QString &documents, int busyWaitMs)
{
    s_vaultNotice.clear();
    const QString oldVault = documents + QStringLiteral("/icloud-notes");
    const QString newVault = documents + QStringLiteral("/Notes");
    const auto isVault = [](const QString &dir) {
        return QFileInfo(dir + QStringLiteral("/.icloud-notes")).isDir()
            || QFileInfo(dir + QStringLiteral("/.icloud-md")).isDir(); // layout 3
    };
    // What is at Notes: nothing (or an empty directory, which the move
    // replaces), or a folder that stops it. For the latter, the vault to use.
    const auto blockedBy = [&]() -> std::optional<QString> {
        const QFileInfo info(newVault);
        if (!info.exists() && !info.isSymLink())
            return std::nullopt;
        if (info.isDir() && !info.isSymLink()
            && QDir(newVault).isEmpty(QDir::AllEntries | QDir::NoDotAndDotDot | QDir::Hidden | QDir::System))
            return std::nullopt;
        if (!isVault(newVault) && isVault(oldVault)) {
            s_vaultNotice = QStringLiteral("%1 already exists and isn't a Notes vault; keeping %2. Move or rename "
                                           "that folder and restart to finish the move.")
                                .arg(newVault, oldVault);
            qWarning("icloud-notes: %s", qPrintable(s_vaultNotice));
            return oldVault;
        }
        qWarning("icloud-notes: both %s and %s exist; using %s. %s is the notes folder of an earlier "
                 "release, left untouched: merge it into %s by hand, then remove it.",
                 qPrintable(oldVault), qPrintable(newVault), qPrintable(newVault), qPrintable(oldVault),
                 qPrintable(newVault));
        return newVault;
    };
    if (!QFileInfo(oldVault).isDir())
        return newVault;
    if (const auto use = blockedBy())
        return *use;

    VaultLock lock(lockPathFor(oldVault));
    const QString owner = QStringLiteral("Notes moving its folder (pid %1)").arg(QCoreApplication::applicationPid());
    QElapsedTimer waited;
    waited.start();
    VaultLock::Result taken = lock.tryLock(owner);
    while (taken == VaultLock::Busy && waited.elapsed() < busyWaitMs) {
        QThread::msleep(50);
        taken = lock.tryLock(owner);
    }
    if (taken != VaultLock::Locked) {
        const QString holder = lock.holder();
        const QString why = taken == VaultLock::Failed ? QStringLiteral("its lock %1 cannot be opened").arg(lock.path())
                            : holder.isEmpty()         ? QStringLiteral("another run has it open")
                                                       : QStringLiteral("%1 has it open").arg(holder);
        qWarning("icloud-notes: not moving %s to %s yet: %s. Using %s until a later start moves it.",
                 qPrintable(oldVault), qPrintable(newVault), qPrintable(why), qPrintable(oldVault));
        return oldVault;
    }
    // Whoever held the lock may have moved it, or made Notes, meanwhile.
    if (!QFileInfo(oldVault).isDir())
        return newVault;
    if (const auto use = blockedBy())
        return *use;
    const QByteArray from = QFile::encodeName(oldVault);
    const QByteArray to = QFile::encodeName(newVault);
    ::rmdir(to.constData()); // an empty Notes; one that filled meanwhile stays, and stops the rename
    int moved = ::renameat2(AT_FDCWD, from.constData(), AT_FDCWD, to.constData(), RENAME_NOREPLACE);
    if (moved != 0 && errno == EINVAL) // a filesystem without RENAME_NOREPLACE
        moved = ::rename(from.constData(), to.constData());
    if (moved != 0) {
        if (errno == EEXIST || errno == ENOTEMPTY)
            if (const auto use = blockedBy())
                return *use;
        qWarning("icloud-notes: cannot move %s to %s (%s). Using %s until a later start moves it.",
                 qPrintable(oldVault), qPrintable(newVault), strerror(errno), qPrintable(oldVault));
        return oldVault;
    }
    qInfo("icloud-notes: moved the notes folder from %s to %s, once, after the update: Notes keeps "
          "its notes in %s from now on.",
          qPrintable(oldVault), qPrintable(newVault), qPrintable(newVault));
    return newVault;
}

QString NotesBackend::lockPath()
{
    return lockPathFor(rootPath());
}

// In the runtime directory, or beside the vault without one: never inside
// it, which is meant to hold only the vault (icloud-notes-sync clones beside
// files already there, never over them, but a clone wants a fresh or empty
// directory). One per
// vault, named by a hash of its canonical path (its cleaned absolute path
// until it exists), so tests never share the real vault's. icloud-notes-sync
// computes the same path (notes-sync/src/cmd/lock.rs): change both or
// neither.
QString NotesBackend::lockPathFor(const QString &vaultPath)
{
    const QFileInfo vault(vaultPath);
    QString key = vault.canonicalFilePath();
    if (key.isEmpty())
        key = QDir::cleanPath(vault.absoluteFilePath());
    quint64 hash = 0xcbf29ce484222325ULL; // 64-bit FNV-1a
    for (const char byte : QFile::encodeName(key)) {
        hash ^= quint8(byte);
        hash *= 0x100000001b3ULL;
    }
    const QString name = QStringLiteral("icloud-notes-%1.lock").arg(hash, 16, 16, QLatin1Char('0'));
    const QString runtime = qEnvironmentVariable("XDG_RUNTIME_DIR");
    return runtime.isEmpty() ? QFileInfo(key).absolutePath() + QStringLiteral("/.") + name : runtime + u'/' + name;
}

VaultLock::Result NotesBackend::lockVault()
{
    const QString owner = m_role == Role::App      ? QStringLiteral("Notes (pid %1)")
                        : m_role == Role::Cli ? QStringLiteral("the icloud-notes command line (pid %1)")
                                              : QStringLiteral("a background sync (icloud-notes --sync, pid %1)");
    return m_lock.tryLock(owner.arg(QCoreApplication::applicationPid()));
}

QString NotesBackend::lockHolder() const
{
    const QString holder = m_lock.holder();
    return holder.isEmpty() ? QStringLiteral("another sync") : holder;
}

// icloud-notes-sync runs with the vault's lock held here. Someone else
// holding it (a background sync) means waiting for as long as that takes; a
// lock that cannot be opened at all means no sync.
void NotesBackend::startProcess()
{
    switch (lockVault()) {
    case VaultLock::Locked:
        startEngine();
        return;
    case VaultLock::Busy:
        showLockWait();
        if (!m_lockRetry.isActive())
            m_lockRetry.start();
        return;
    case VaultLock::Failed:
        failWithoutLock();
        return;
    }
}

void NotesBackend::showLockWait()
{
    setSyncMessage(QStringLiteral("Waiting for %1 to finish…").arg(lockHolder()));
}

void NotesBackend::failWithoutLock()
{
    appendLog(QStringLiteral("Could not open the sync lock %1; not running icloud-notes-sync without it.")
                  .arg(m_lock.path()));
    finishSync(-1);
}

// Whoever held the vault let go: show what it pulled, then run the sync
// that waited for it.
void NotesBackend::retryLock()
{
    switch (lockVault()) {
    case VaultLock::Busy:
        if (m_syncRunning) // the holder may have changed
            showLockWait();
        return;
    case VaultLock::Failed:
        m_lockRetry.stop();
        if (m_syncRunning)
            failWithoutLock();
        return;
    case VaultLock::Locked:
        break;
    }
    m_lockRetry.stop();
    refresh();
    if (m_syncRunning) {
        setSyncMessage(m_syncLabel + QStringLiteral("…"));
        startEngine();
    }
}

// icloud-notes-sync takes the vault's lock itself (clone, pull, push,
// restore), which this backend already holds: it gets the locked
// descriptor as ICLOUD_NOTES_LOCK_FD, which it checks is open on this
// vault's lock file and holds it before trusting it. The descriptor is
// close-on-exec everywhere else; only this child keeps it.
void NotesBackend::startEngine()
{
    const int fd = m_lock.fd();
    QProcessEnvironment env = QProcessEnvironment::systemEnvironment();
    env.insert(QStringLiteral("ICLOUD_NOTES_LOCK_FD"), QString::number(fd));
    m_syncProcess.setProcessEnvironment(env);
    m_syncProcess.setChildProcessModifier([fd] { ::fcntl(fd, F_SETFD, 0); });
    m_syncProcess.start();
}

QString NotesBackend::folderAbsolutePath(const QString &folder) const
{
    return QDir(rootPath()).filePath(folder);
}

QString NotesBackend::noteAbsolutePath() const
{
    return m_currentNote.isEmpty() ? QString()
                                   : QDir(folderAbsolutePath(m_currentFolder)).filePath(m_currentNote);
}

QString NotesBackend::vaultRelative(const QString &name) const
{
    return m_currentFolder.isEmpty() ? name : m_currentFolder + u'/' + name;
}

// The engine's state directory, as vault-info's stateDir names it:
// .icloud-notes (layout 4), or .icloud-md for a vault an older engine left,
// which the next sync moves (.icloud-md then keeps only a tombstone
// state.json that an older engine refuses as written by a newer version).
QString NotesBackend::stateDir()
{
    const QString current = rootPath() + QStringLiteral("/.icloud-notes");
    const QString legacy = rootPath() + QStringLiteral("/.icloud-md");
    if (!QFile::exists(current + QStringLiteral("/state.json")) && QFile::exists(legacy + QStringLiteral("/state.json")))
        return legacy;
    return current;
}

// A clone leaves a state.json in the engine's state directory.
bool NotesBackend::vaultCloned()
{
    return QFile::exists(stateDir() + QStringLiteral("/state.json"));
}

bool NotesBackend::isLoggedStderrLine(const QByteArray &line)
{
    return !line.trimmed().isEmpty() && !line.startsWith("icloud-notes:progress:");
}

QString NotesBackend::vaultInfoKey() const
{
    return syncToolPath() + u'\n' + QFileInfo(rootPath()).absoluteFilePath() + u'\n' + stateFileStamp();
}

QString NotesBackend::stateFileStamp()
{
    return fileStamp(stateDir() + QStringLiteral("/state.json"));
}

void NotesBackend::adoptVaultInfo(const QJsonObject &o) const
{
    VaultInfo info;
    if (o.value(QStringLiteral("titleMode")).toString() == u"filename")
        info.titleMode = QStringLiteral("filename");
    info.defaultFolderDir = o.value(QStringLiteral("defaultFolderDir")).toString();
    const QString vault = o.value(QStringLiteral("vault")).toString();
    for (const QJsonValue &v : o.value(QStringLiteral("notes")).toArray()) {
        const QJsonObject note = v.toObject();
        const QString file = note.value(QStringLiteral("file")).toString();
        const QString reason = note.value(QStringLiteral("readOnlyReason")).toString();
        const QString base = note.value(QStringLiteral("baseFile")).toString();
        if (file.isEmpty())
            continue;
        info.tracked.insert(file);
        if (!reason.isEmpty())
            info.readOnly.insert(file, reason);
        if (!base.isEmpty())
            info.baseFiles.insert(note.value(QStringLiteral("id")).toString(), vault + u'/' + base);
    }
    m_vaultInfoKey = vaultInfoKey();
    m_vaultInfo = info;
    m_vaultInfoFailed.invalidate();
}

const NotesBackend::VaultInfo &NotesBackend::vaultInfo() const
{
    // Taken before asking, so a change while the engine answers asks again.
    const QString key = vaultInfoKey();
    if (!m_vaultInfoKey.isEmpty() && key == m_vaultInfoKey)
        return m_vaultInfo;

    const QString tool = syncToolPath();
    if (tool.isEmpty()) {
        adoptVaultInfo({}); // no engine: nothing is known
        return m_vaultInfo;
    }
    if (m_vaultInfoFailed.isValid() && m_vaultInfoFailed.elapsed() < kVaultInfoRetryMs)
        return m_vaultInfo;
    QProcess engine;
    engine.start(tool, { QStringLiteral("--quiet"), QStringLiteral("vault-info"), QFileInfo(rootPath()).absoluteFilePath() });
    if (engine.waitForFinished(10000) && engine.exitStatus() == QProcess::NormalExit && engine.exitCode() == 0) {
        adoptVaultInfo(QJsonDocument::fromJson(engine.readAllStandardOutput()).object());
        m_vaultInfoKey = key;
        return m_vaultInfo;
    }
    if (engine.state() != QProcess::NotRunning) {
        engine.kill();
        engine.waitForFinished();
    }
    // No answer: what was known stands, and the engine is asked again later.
    m_vaultInfoFailed.start();
    return m_vaultInfo;
}

QString NotesBackend::syncToolPath()
{
    const auto executable = [](const QString &path) {
        const QFileInfo info(path);
        return info.isFile() && info.isExecutable() ? info.absoluteFilePath() : QString();
    };
    // Tests and development name the engine; then nothing else is tried.
    const QString named = qEnvironmentVariable("ICLOUD_NOTES_SYNC_BIN");
    if (!named.isEmpty())
        return executable(named);
    const QString packaged = executable(QString::fromLatin1(kSyncToolPath));
    if (!packaged.isEmpty())
        return packaged;
    // A development build of the engine on PATH (cargo install, say).
    return QStandardPaths::findExecutable(QString::fromLatin1(kSyncTool));
}

bool NotesBackend::syncToolAvailable() const
{
    return !syncToolPath().isEmpty();
}

QString NotesBackend::vaultTitleMode() const
{
    return vaultInfo().titleMode;
}

QString NotesBackend::defaultFolder() const
{
    return vaultInfo().defaultFolderDir;
}

QString NotesBackend::noteBody() const
{
    return SyncModel::splitEnvelope(m_noteContent).body;
}

// The file as it is written for an editor body: the stored envelope first,
// untouched, so the id cannot be edited away.
QString NotesBackend::assembleNote(const QString &body) const
{
    return SyncModel::splitEnvelope(m_noteContent).envelope + body;
}

MarkdownHighlighter::Colors NotesBackend::highlighterColors() const
{
    auto color = [this](const char *key, const QColor &fallback) {
        const QString value = m_theme.value(QLatin1StringView(key)).toString();
        return QColor::isValidColorName(value) ? QColor::fromString(value) : fallback;
    };
    return { color("accent", QColor(0x7a, 0xa2, 0xf7)), color("dark_foreground", QColor(0x80, 0x80, 0x80)),
             color("light_foreground", QColor(0xb0, 0xb0, 0xb0)), color("lighter_background", QColor(0x30, 0x30, 0x30)) };
}

void NotesBackend::attachEditor(QQuickTextDocument *document)
{
    if (!document || m_highlighter)
        return;
    m_highlighter = new MarkdownHighlighter(document->textDocument(), highlighterColors());
    connect(this, &NotesBackend::themeChanged, m_highlighter,
            [this] { m_highlighter->setColors(highlighterColors()); });
}

void NotesBackend::setEditorCursor(int position)
{
    if (m_highlighter)
        m_highlighter->setActivePosition(position);
}

void NotesBackend::rebuildFolders()
{
    const QDir root(rootPath());
    QStringList folders{ QString() }; // the vault root itself
    QDirIterator dirs(rootPath(), QDir::Dirs | QDir::NoDotAndDotDot, QDirIterator::Subdirectories);
    while (dirs.hasNext()) {
        const QString rel = root.relativeFilePath(dirs.next());
        // Downloaded attachment bundles are not folders either.
        if (isNotePath(rel))
            folders << rel;
    }
    SyncModel::sortFolders(folders, defaultFolder());

    // Every note counts toward each folder above it, the root included.
    QVariantMap counts;
    for (const QString &folder : folders)
        counts.insert(folder, 0);
    forEachNoteFile(rootPath(), [&](QString rel, const QString &) {
        do {
            rel = rel.section(u'/', 0, -2);
            if (counts.contains(rel))
                counts[rel] = counts[rel].toInt() + 1;
        } while (!rel.isEmpty());
        return true;
    });

    m_folders = folders;
    m_folderNoteCounts = counts;
    if (!m_folders.contains(m_currentFolder)) {
        // The open note went with its folder: follow it, or remember it.
        if (m_currentNote.isEmpty() || !followNote(SyncModel::extractNoteId(m_noteContent))) {
            if (!m_currentNote.isEmpty())
                loseCurrentNote();
            setCurrentFolder({});
        }
    }
    emit foldersChanged(); // also refreshes cloned/vaultTitleMode after a clone
}

void NotesBackend::rebuildNotes()
{
    const QDir dir(folderAbsolutePath(m_currentFolder));
    QFileInfoList entries;
    forEachNoteFile(dir.absolutePath(), [&](const QString &, const QString &path) {
        entries << QFileInfo(path);
        return true;
    });
    // The sync tool syncs note mtimes, so newest-first matches Notes.app ordering.
    std::sort(entries.begin(), entries.end(),
              [](const QFileInfo &a, const QFileInfo &b) { return a.lastModified() > b.lastModified(); });
    QStringList found;
    for (const QFileInfo &info : entries)
        found << dir.relativeFilePath(info.filePath());
    // The open note's file is gone (a pull moved, renamed or deleted it):
    // follow it by its id, which rebuilds the list where it went.
    if (!m_currentNote.isEmpty() && !found.contains(m_currentNote)
        && followNote(SyncModel::extractNoteId(m_noteContent)))
        return;

    const QStringList oldNotes = m_notes;
    const QVariantMap oldStates = m_noteStates;
    const QVariantMap oldDetails = m_noteDetails;
    m_notes = found;
    if (!m_currentNote.isEmpty() && !m_notes.contains(m_currentNote))
        loseCurrentNote();
    classifyNotes();
    // Only a real change re-renders the list (and drops its scroll position).
    if (m_notes != oldNotes || m_noteStates != oldStates || m_noteDetails != oldDetails)
        emit notesChanged();
}

// Per-note list details plus the guardrail flags the badges show: notes
// the sync tool does not track ("new"), tracked notes that lost their id,
// untracked notes carrying an id from elsewhere, Syncthing's conflict
// copies (which it never syncs), conflicts and tables.
void NotesBackend::classifyNotes()
{
    const VaultInfo &info = vaultInfo();
    const QSet<QString> &tracked = info.tracked;
    const QHash<QString, QString> &readOnly = info.readOnly;
    const QString mode = info.titleMode;
    const QDir dir(folderAbsolutePath(m_currentFolder));
    QHash<QString, NoteScan> scans;
    QVariantMap states;
    QVariantMap details;
    for (const QString &name : m_notes) {
        const QString path = dir.filePath(name);
        const QFileInfo info(path);
        NoteScan scan = m_scans.value(path);
        if (scan.modifiedMs != info.lastModified().toMSecsSinceEpoch() || scan.size != info.size()
            || scan.mode != mode) {
            const QString text = readText(path, kScanLimit + 1);
            const bool huge = text.size() > kScanLimit;
            const SyncModel::NotePreview preview = SyncModel::previewNote(text, QFileInfo(name).completeBaseName(), mode);
            scan = { info.lastModified().toMSecsSinceEpoch(), info.size(), mode, preview.title, preview.snippet,
                     SyncModel::extractNoteId(text), !huge && SyncModel::hasConflictMarkers(text),
                     !huge && SyncModel::hasTable(text) };
        }
        scans.insert(path, scan);
        details.insert(name, QVariantMap{ { QStringLiteral("title"), scan.title },
                                          { QStringLiteral("snippet"), scan.snippet },
                                          { QStringLiteral("modifiedMs"), scan.modifiedMs },
                                          { QStringLiteral("id"), scan.id } });
        QStringList flags;
        if (scan.conflict)
            flags << QStringLiteral("conflict");
        if (scan.table)
            flags << QStringLiteral("tables");
        // A Syncthing conflict copy is no note to the engine: never "new"
        // (it will not be created) nor a note with someone else's id.
        if (SyncModel::isSyncConflictName(name))
            flags << QStringLiteral("conflict-copy");
        else if (!tracked.contains(vaultRelative(name)))
            flags << (scan.id.isEmpty() ? QStringLiteral("new") : QStringLiteral("foreign-id"));
        else if (scan.id.isEmpty())
            flags << QStringLiteral("missing-id");
        if (readOnly.contains(vaultRelative(name)))
            flags << QStringLiteral("read-only");
        if (!flags.isEmpty())
            states.insert(name, flags);
    }
    m_scans = scans;
    m_noteStates = states;
    m_noteDetails = details;
}

void NotesBackend::loadCurrentNote()
{
    const QString path = noteAbsolutePath();
    m_noteContent = path.isEmpty() ? QString() : readText(path);
    m_noteAttachments = path.isEmpty() ? QVariantList() : attachmentsFor(path, m_noteContent);
    m_readOnlyReason = path.isEmpty() ? QString()
                                      : vaultInfo().readOnly.value(vaultRelative(m_currentNote));
    emit noteContentChanged();
}

void NotesBackend::closeNote()
{
    m_currentNote.clear();
    emit currentNoteChanged();
    loadCurrentNote();
}

// Where the note with this apple-note-id lives now, vault-relative, or
// empty. Only when the open note's file went away, so a full scan is fine.
QString NotesBackend::findNoteById(const QString &id) const
{
    if (id.isEmpty())
        return {};
    QString found;
    forEachNoteFile(rootPath(), [&](const QString &rel, const QString &path) {
        // A Syncthing conflict copy carries the id too, and is not the note.
        if (!SyncModel::isSyncConflictName(rel) && SyncModel::extractNoteId(readText(path, 64 * 1024)) == id)
            found = rel;
        return found.isEmpty();
    });
    return found;
}

// Open the note with this id where it now is. False when there is none.
bool NotesBackend::followNote(const QString &id)
{
    const QString rel = m_following ? QString() : findNoteById(id);
    if (rel.isEmpty())
        return false;
    m_following = true;
    m_lostNote = {};
    if (m_queuedSave.valid)
        m_queuedSave.note = rel; // its edits go where the note went
    showNote(rel.section(u'/', 0, -2), rel.section(u'/', -1)); // unsaved edits merge on its load (keepEditsAsConflict)
    m_following = false;
    return true;
}

// Makes `name` in `folder` the open note, wherever the backend was: the
// folder's list, the note's content and the watches follow.
void NotesBackend::showNote(const QString &folder, const QString &name)
{
    if (folder != m_currentFolder) {
        m_currentFolder = folder;
        emit currentFolderChanged();
    }
    m_currentNote = name;
    emit currentNoteChanged();
    rebuildNotes();
    loadCurrentNote();
    rewatch();
}

// The open note, with `content`, as keepEditsAsNewNote wants it remembered.
NotesBackend::LostNote NotesBackend::lostCopy(const QString &content) const
{
    return { m_currentFolder, SyncModel::previewNote(content, m_currentNote.chopped(3), vaultTitleMode()).title, content,
             true };
}

// The open note's file is gone and no file carries its id: close it, but
// remember it, so unsaved edits can still be kept (keepEditsAsNewNote).
void NotesBackend::loseCurrentNote()
{
    m_lostNote = lostCopy(m_noteContent);
    closeNote();
}

// Unsaved edits that cannot go into their note (deleted elsewhere, or
// carrying conflict markers a merge would nest): a new note beside where
// it was, never the note's id (that would bring a deleted one back as a
// copy). `why` gets the note's title and the new note's as %1 and %2.
bool NotesBackend::keepEditsAsNewNote(const QString &mine, const QString &why)
{
    const QString folder = QDir(folderAbsolutePath(m_lostNote.folder)).exists() ? m_lostNote.folder : QString();
    QString title = cleanName(m_lostNote.title);
    if (title.isEmpty())
        title = QStringLiteral("Untitled");
    const QDir dir(folderAbsolutePath(folder));
    QString name = title + QStringLiteral(" (unsaved edits).md");
    for (int n = 2; QFile::exists(dir.filePath(name)); ++n)
        name = title + QStringLiteral(" (unsaved edits %1).md").arg(n);
    const QString body = SyncModel::restoreEditorChars(SyncModel::splitEnvelope(m_lostNote.content).body, mine);
    if (!writeText(dir.filePath(name), body))
        return false;
    m_lostNote = {};
    showNote(folder, name);
    const QString message = why.arg(title, name.chopped(3));
    appendLog(message);
    emit editsKeptAsNote(message);
    emit vaultChanged();
    return true;
}

void NotesBackend::rewatch()
{
    const QStringList watched = m_watcher.directories() + m_watcher.files();
    if (!watched.isEmpty())
        m_watcher.removePaths(watched);
    m_watcher.addPath(rootPath());
    for (const QString &folder : m_folders) {
        if (!folder.isEmpty() && (m_currentFolder.isEmpty() || folder.startsWith(m_currentFolder + u'/')))
            m_watcher.addPath(folderAbsolutePath(folder));
    }
    if (QDir(folderAbsolutePath(m_currentFolder)).exists())
        m_watcher.addPath(folderAbsolutePath(m_currentFolder));
    if (QFile::exists(noteAbsolutePath()))
        m_watcher.addPath(noteAbsolutePath());
}

void NotesBackend::setCurrentFolder(const QString &folder)
{
    if (m_currentFolder == folder)
        return;
    m_currentFolder = folder;
    emit currentFolderChanged();
    closeNote();
    rebuildNotes();
    rewatch();
}

void NotesBackend::refresh()
{
    rebuildFolders();
    rebuildNotes();
    loadCurrentNote();
    rewatch();
}

void NotesBackend::openNote(const QString &name)
{
    if (!m_notes.contains(name))
        return;
    // All Notes uses vault-relative paths so identical filenames in different
    // folders remain distinct. Open the note in its actual folder.
    if (name.contains(u'/')) {
        const QString folder = vaultRelative(name).section(u'/', 0, -2);
        setCurrentFolder(folder);
        openNote(name.section(u'/', -1));
        return;
    }
    m_lostNote = {};
    if (m_currentNote != name) {
        m_queuedSave = {}; // a save waiting on a sync was the other note's
        m_currentNote = name;
        emit currentNoteChanged();
    }
    loadCurrentNote();
    rewatch();
}

bool NotesBackend::saveNote(const QString &body, const QString *editorBase)
{
    const QString path = noteAbsolutePath();
    if (path.isEmpty()) {
        // The note went away under these edits: keepEditsAsConflict keeps
        // them, which the window asks for once no sync runs. Under one they
        // wait like any save, so that its end, or Notes closing first,
        // still puts them somewhere (writeQueuedSave).
        if (m_syncRunning && m_lostNote.valid && !body.isEmpty())
            m_queuedSave = { QString(),
                             editorBase ? *editorBase
                                        : SyncModel::editorForm(SyncModel::splitEnvelope(m_lostNote.content).body),
                             body, true };
        return body.isEmpty();
    }
    const QString text = assembleNote(SyncModel::restoreEditorChars(noteBody(), body));
    if (!m_readOnlyReason.isEmpty() || text == m_noteContent) {
        m_queuedSave = {}; // an edit undone since it was queued is not written later
        return true;
    }
    // Never under a running sync: a pull may be rewriting this very file.
    // Written once it is done and its changes are loaded (writeQueuedSave).
    // Its base is the text the edits started from: the editor's own, when
    // the caller has one. The note may have been loaded again under the
    // sync (the lock coming free, Refresh, a note followed to where it
    // moved) while the editor kept its edits, and taking that newer content
    // for the base would write the edits over what changed in between.
    // Without an editor's, a save already waiting keeps the base it has,
    // and a first one takes the note as loaded.
    if (m_syncRunning) {
        const QString note = vaultRelative(m_currentNote);
        const bool waiting = m_queuedSave.valid && m_queuedSave.note == note;
        m_queuedSave = { note,
                         editorBase ? *editorBase : waiting ? m_queuedSave.base : SyncModel::editorForm(noteBody()),
                         body, true };
        return false;
    }
    // A pull (or another program) rewrote the note since it was loaded:
    // writing now would silently drop that change. The reload that follows
    // hands the edits to keepEditsAsConflict instead.
    if (QFile::exists(path) && readText(path) != m_noteContent) {
        emit currentNoteChangedOnDisk();
        return false;
    }
    if (!writeText(path, text))
        return false;
    m_queuedSave = {};
    loadCurrentNote();
    rebuildNotes(); // a save bumps mtime, which reorders the list
    emit vaultChanged();
    return true;
}

// The link the sync engine writes for an attachment file, and so the one
// it takes for a file to add (notes-sync `format_attachment_markdown`): an
// embed for the image types it knows, the file's name as the link text with
// its brackets and backslashes escaped, and the path percent-encoded as
// JavaScript's encodeURIComponent does.
QString NotesBackend::attachmentLink(const QString &name)
{
    static const QStringList embedded{ QStringLiteral("png"),  QStringLiteral("jpg"),  QStringLiteral("jpeg"), QStringLiteral("heic"),
                                       QStringLiteral("tiff"), QStringLiteral("tif"),  QStringLiteral("gif"),  QStringLiteral("webp") };
    QString text;
    for (const QChar c : name) {
        if (c == u'[' || c == u']' || c == u'\\')
            text += u'\\';
        text += c;
    }
    const int dot = name.lastIndexOf(u'.');
    const bool image = dot > 0 && embedded.contains(name.mid(dot + 1).toLower());
    // Parentheses that pair up stay as they are ("photo (1).png"); ones
    // that don't would end the link early, so they are encoded.
    int depth = 0;
    bool paired = true;
    for (const QChar c : name) {
        if (c == u'(')
            ++depth;
        else if (c == u')' && --depth < 0)
            paired = false;
    }
    paired = paired && depth == 0;
    return QStringLiteral("%1[%2](attachments/%3)")
        .arg(image ? QStringLiteral("!") : QString(), text,
             QString::fromLatin1(QUrl::toPercentEncoding(name, paired ? QByteArrayLiteral("!*'()") : QByteArrayLiteral("!*'"))));
}

QVariantMap NotesBackend::addAttachment(const QString &source)
{
    // The most the sync engine uploads as one attachment (notes-sync
    // MAX_NEW_ATTACHMENT_BYTES).
    constexpr qint64 kMaxAttachmentBytes = 100LL * 1024 * 1024;
    const auto refuse = [](const char *code, const QString &message, const QString &hint = {}) {
        QVariantMap out{ { QStringLiteral("ok"), false }, { QStringLiteral("code"), QLatin1String(code) }, { QStringLiteral("message"), message } };
        if (!hint.isEmpty())
            out.insert(QStringLiteral("hint"), hint);
        return out;
    };
    const QString notePath = noteAbsolutePath();
    if (notePath.isEmpty())
        return refuse("error", QStringLiteral("no note is open"));
    if (!m_readOnlyReason.isEmpty())
        return refuse("read_only",
                      QStringLiteral("%1 is read-only here: this note %2").arg(QDir(rootPath()).relativeFilePath(notePath), m_readOnlyReason),
                      QStringLiteral("Add the attachment in Apple Notes; it still syncs here."));
    const QFileInfo info(source);
    if (!info.exists())
        return refuse("not_found", QStringLiteral("no file \"%1\"").arg(source));
    QFile in(info.absoluteFilePath());
    if (!info.isFile() || !in.open(QIODevice::ReadOnly))
        return refuse("error", QStringLiteral("cannot read %1: %2").arg(source, info.isFile() ? in.errorString() : QStringLiteral("not a regular file")));
    in.close();
    if (info.size() == 0)
        return refuse("error", QStringLiteral("%1 is empty: there is nothing to attach").arg(source));
    if (info.size() > kMaxAttachmentBytes)
        return refuse("error", QStringLiteral("%1 is larger than 100 MB, the most that can be added as an attachment").arg(source),
                      QStringLiteral("Add it in Apple Notes instead."));
    // One plain file name, as the engine wants an attachment's.
    const QString name = info.fileName();
    bool plain = !name.isEmpty() && name.toUtf8().size() <= 190;
    for (const QChar c : name)
        plain = plain && c.unicode() >= 0x20 && c.unicode() != 0x7f;
    if (!plain)
        return refuse("usage", QStringLiteral("\"%1\" cannot be an attachment's name: rename the file first (no control "
                                              "characters, at most 190 bytes)")
                                   .arg(name));

    const QDir folder = QFileInfo(notePath).dir();
    if (!folder.mkpath(QStringLiteral("attachments")))
        return refuse("error", QStringLiteral("could not create %1").arg(folder.filePath(QStringLiteral("attachments"))));
    const QDir dir(folder.filePath(QStringLiteral("attachments")));
    // Never over a file that is there: "name 2.ext", as a pull names a
    // second attachment of the same name.
    const int dot = name.lastIndexOf(u'.');
    const QString stem = dot > 0 ? name.left(dot) : name;
    const QString ext = dot > 0 ? name.mid(dot) : QString();
    QString stored = name;
    for (int i = 2; QFileInfo::exists(dir.filePath(stored)) || QFileInfo(dir.filePath(stored)).isSymLink(); ++i)
        stored = QStringLiteral("%1 %2%3").arg(stem).arg(i).arg(ext);
    // QFile::copy refuses to replace a file, should one appear meanwhile.
    QFile copy(info.absoluteFilePath());
    if (!copy.copy(dir.filePath(stored)))
        return refuse("error", QStringLiteral("could not copy %1 to %2: %3").arg(source, dir.filePath(stored), copy.errorString()));
    QFile::setPermissions(dir.filePath(stored), QFile::permissions(dir.filePath(stored)) | QFileDevice::ReadOwner | QFileDevice::WriteOwner);
    return { { QStringLiteral("ok"), true },
             { QStringLiteral("name"), stored },
             { QStringLiteral("path"), dir.filePath(stored) },
             { QStringLiteral("link"), attachmentLink(stored) } };
}

QStringList NotesBackend::removedAttachments(const QString &body) const
{
    const QString path = noteAbsolutePath();
    if (path.isEmpty() || !m_readOnlyReason.isEmpty() || m_noteAttachments.isEmpty())
        return {};
    QSet<QString> kept;
    for (const QVariant &v : attachmentsFor(path, body))
        kept.insert(v.toMap().value(QStringLiteral("name")).toString());
    QStringList removed;
    for (const QVariant &v : m_noteAttachments) {
        const QString name = v.toMap().value(QStringLiteral("name")).toString();
        if (!kept.contains(name))
            removed << name;
    }
    return removed;
}

QString NotesBackend::withoutAttachmentLink(const QString &body, const QString &name)
{
    const QString target = QStringLiteral("attachments/") + name;
    QString out;
    qsizetype from = 0;
    for (const QRegularExpressionMatch &match : attachmentLinkRe().globalMatch(body)) {
        // Links are URL-encoded.
        if (QUrl(match.captured(1)).path() != target && QUrl::fromPercentEncoding(match.captured(1).toUtf8()) != target)
            continue;
        qsizetype start = match.capturedStart(), end = match.capturedEnd();
        // Alone on its line: the line goes too.
        const qsizetype lineStart = start == 0 ? 0 : body.lastIndexOf(u'\n', start - 1) + 1;
        qsizetype lineEnd = body.indexOf(u'\n', end);
        if (lineEnd < 0)
            lineEnd = body.size();
        if (body.mid(lineStart, start - lineStart).trimmed().isEmpty() && body.mid(end, lineEnd - end).trimmed().isEmpty()) {
            start = lineStart;
            end = qMin(lineEnd + 1, body.size());
        }
        if (start < from)
            continue;
        out += body.mid(from, start - from);
        from = end;
    }
    return from == 0 ? body : out + body.mid(from);
}

QStringList NotesBackend::clipboardFiles()
{
    const QMimeData *mime = QGuiApplication::clipboard()->mimeData();
    if (!mime)
        return {};
    QStringList files;
    for (const QUrl &url : mime->urls()) {
        if (url.isLocalFile() && QFileInfo(url.toLocalFile()).isFile())
            files.append(url.toLocalFile());
    }
    if (!files.isEmpty() || !mime->hasImage() || mime->hasText())
        return files;
    // A copied picture (a screenshot): attached as a PNG file.
    const QImage image = qvariant_cast<QImage>(mime->imageData());
    if (image.isNull())
        return {};
    if (!m_pasteDir)
        m_pasteDir = std::make_unique<QTemporaryDir>();
    const QString path = m_pasteDir->filePath(QStringLiteral("Pasted image.png"));
    if (!m_pasteDir->isValid() || !image.save(path, "PNG"))
        return {};
    return { path };
}

bool NotesBackend::keepEditsAsConflict(const QString &base, const QString &mine)
{
    // The reloads below reach the window, whose own unsaved text is what is
    // being merged: asked again from there, never a second time.
    if (m_merging)
        return false;
    const QScopedValueRollback<bool> merging(m_merging, true);
    // Its file went away: a pull may have written the new one only after
    // removing the old, so look for its id once more before giving up on it.
    if (m_currentNote.isEmpty() && m_lostNote.valid
        && !followNote(SyncModel::extractNoteId(m_lostNote.content)))
        return keepEditsAsNewNote(
            mine, QStringLiteral("\"%1\" was deleted elsewhere, so your unsaved edits are in a new note, \"%2\"."));
    const QString path = noteAbsolutePath();
    if (path.isEmpty() || !m_readOnlyReason.isEmpty() || !QFile::exists(path))
        return false;
    const QString disk = readText(path);
    const SyncModel::EnvelopeSplit split = SyncModel::splitEnvelope(disk);
    const QString theirs = SyncModel::editorForm(split.body);
    if (theirs == base || theirs == mine)
        return false;
    // A conflict still unresolved on either side would nest one block in
    // another, which nothing can read back (see conflictBody). The note is
    // left as it is and the edits become a note of their own.
    if (SyncModel::hasConflictMarkers(theirs) || SyncModel::hasConflictMarkers(base)
        || SyncModel::hasConflictMarkers(mine)) {
        m_lostNote = lostCopy(disk);
        return keepEditsAsNewNote(
            mine, QStringLiteral("\"%1\" has an unresolved conflict, so your unsaved edits were not merged into it. "
                                 "They are in a new note, \"%2\", and \"%1\" is unchanged."));
    }
    // Edits to different lines merge on their own; only lines both sides
    // changed differently become blocks to pick from.
    const QString merged = SyncModel::conflictBody(split.body, base, mine);
    if (!writeText(path, split.envelope + merged))
        return false;
    m_queuedSave = {}; // these edits are in
    loadCurrentNote();
    rebuildNotes();
    if (!SyncModel::hasConflictMarkers(merged))
        emit vaultChanged(); // a clean merge syncs as any edit
    return true;
}

QString NotesBackend::saveWarning(const QString &body)
{
    QStringList warnings;
    if (const QString guardrails = guardrailWarning(body); !guardrails.isEmpty())
        warnings << guardrails;
    const QStringList removed = removedAttachments(body);
    if (removed.size() == 1)
        warnings << QStringLiteral("This removes the attachment %1 from this note on all your devices.").arg(removed.first());
    else if (!removed.isEmpty())
        warnings << QStringLiteral("This removes the attachments %1 from this note on all your devices.")
                        .arg(removed.join(QStringLiteral(", ")));
    return warnings.join(QStringLiteral("\n\n"));
}

QString NotesBackend::guardrailWarning(const QString &body)
{
    // Checks run against the full file as it would be written.
    const QString text = assembleNote(body);
    QStringList warnings;
    if (SyncModel::hasConflictMarkers(text) && !SyncModel::hasConflictMarkers(m_noteContent))
        warnings << QStringLiteral("Unresolved conflict markers present. Push will refuse this note "
                                   "until they are resolved.");
    const QString id = SyncModel::extractNoteId(text);
    const QDir dir(folderAbsolutePath(m_currentFolder));
    for (const QString &name : m_notes) {
        // A Syncthing conflict copy of a note carries its id, but the
        // engine never syncs one: nothing ambiguous about it, whether it
        // is the other file or the one being saved.
        if (id.isEmpty() || name == m_currentNote || m_scans.value(dir.filePath(name)).id != id
            || SyncModel::isSyncConflictName(name) || SyncModel::isSyncConflictName(m_currentNote))
            continue;
        warnings << QStringLiteral("Another note in this folder (%1) carries the same apple-note-id. "
                                   "Pushing two files with one id is ambiguous, so keep only one.")
                        .arg(name);
        break;
    }
    return warnings.join(QStringLiteral("\n\n"));
}

QString NotesBackend::cleanName(const QString &name)
{
    QString clean = name.trimmed();
    clean.remove(u'/');
    clean.remove(u'\0');
    return clean;
}

QString NotesBackend::unsyncedNameReason(const QString &name, bool folder)
{
    if (name.startsWith(u'.'))
        return QStringLiteral("A name cannot start with a dot: those are skipped by sync.");
    if (folder && name.compare(QStringLiteral("attachments"), Qt::CaseInsensitive) == 0)
        return QStringLiteral("That folder name holds note attachments: it is skipped by sync.");
    return {};
}

void NotesBackend::newNote(const QString &name)
{
    QString clean = cleanName(name);
    if (clean.isEmpty() || !unsyncedNameReason(clean, false).isEmpty())
        return;
    if (!clean.endsWith(QStringLiteral(".md"), Qt::CaseInsensitive))
        clean += QStringLiteral(".md");
    const QString path = QDir(folderAbsolutePath(m_currentFolder)).filePath(clean);
    // In-body vaults carry the title as the first line; filename vaults
    // carry it in the name and start with an empty body.
    const QString body = vaultTitleMode() == u"filename" ? QString() : u"# " + clean.chopped(3) + u'\n';
    if (!QFile::exists(path) && !writeText(path, body))
        return;
    rebuildNotes();
    openNote(clean);
}

QString NotesBackend::deleteCurrentNote()
{
    const QString path = noteAbsolutePath();
    if (path.isEmpty())
        return QStringLiteral("No note selected.");
    if (!QFile::moveToTrash(path)) // the next push moves the note to Recently Deleted
        return QStringLiteral("Could not move the note to the trash.");
    m_lostNote = {}; // deleted here on purpose: its edits go with it
    closeNote();
    rebuildNotes();
    rewatch();
    return {};
}

QString NotesBackend::renameCurrentNote(const QString &title)
{
    const bool filename = vaultTitleMode() == u"filename";
    // A file name cannot hold a slash; a title line can ("Q1/Q2 plan").
    const QString clean = filename ? cleanName(title) : title.trimmed().remove(u'\0');
    if (m_currentNote.isEmpty())
        return QStringLiteral("No note selected.");
    if (!m_readOnlyReason.isEmpty())
        return refuse("read_only", QStringLiteral("This note is read-only here. Rename it in Apple Notes."));
    if (clean.isEmpty())
        return QStringLiteral("Title is empty.");
    if (filename) {
        // The file name IS the title: renaming the file retitles the note.
        const QString target = clean.endsWith(QStringLiteral(".md"), Qt::CaseInsensitive)
            ? clean
            : clean + QStringLiteral(".md");
        if (const QString reason = unsyncedNameReason(target, false); !reason.isEmpty())
            return refuse("usage", reason);
        if (target == m_currentNote)
            return {};
        const QString path = QDir(folderAbsolutePath(m_currentFolder)).filePath(target);
        if (QFile::exists(path))
            return refuse("exists", QStringLiteral("A note with that name already exists."));
        if (!QFile::rename(noteAbsolutePath(), path))
            return QStringLiteral("Could not rename the file.");
        m_currentNote = target;
        emit currentNoteChanged();
    } else {
        // In-body vaults carry the title in the first line: retitle that line.
        if (noteChangedOnDisk())
            return kChangedOnDiskMessage;
        const QString updated = SyncModel::retitleInBody(m_noteContent, clean);
        // A title line that holds an attachment's link: replacing it would
        // remove the attachment in iCloud, which nothing does unasked.
        if (!removedAttachments(SyncModel::editorForm(SyncModel::splitEnvelope(updated).body)).isEmpty())
            return QStringLiteral("The title line holds an attachment. Edit the note's text instead.");
        if (updated != m_noteContent && !writeText(noteAbsolutePath(), updated))
            return QStringLiteral("Could not write the note.");
    }
    rebuildNotes();
    openNote(m_currentNote);
    return {};
}

QString NotesBackend::moveCurrentNote(const QString &folder)
{
    if (m_currentNote.isEmpty())
        return QStringLiteral("No note selected.");
    if (!m_readOnlyReason.isEmpty())
        return refuse("read_only", QStringLiteral("This note is read-only here. Move it in Apple Notes."));
    // A note's attachments are files in its folder's attachments/: the next
    // push moves them after the note and rewrites its links.
    if (!m_folders.contains(folder))
        return QStringLiteral("No folder \"%1\".").arg(folder);
    if (folder == m_currentFolder)
        return {};
    const QString target = QDir(folderAbsolutePath(folder)).filePath(m_currentNote);
    if (QFile::exists(target))
        return refuse("exists", QStringLiteral("A note with that name already exists in that folder."));
    if (!QFile::rename(noteAbsolutePath(), target))
        return QStringLiteral("Could not move the file.");
    m_lostNote = {};
    rebuildFolders(); // the counts
    showNote(folder, m_currentNote);
    emit vaultChanged();
    return {};
}

// Only the version on screen is replaced: when the open note's file holds
// something else by now (another program, a pull), that is shown first and
// the caller refuses with kChangedOnDiskMessage.
bool NotesBackend::noteChangedOnDisk()
{
    if (readText(noteAbsolutePath()) == m_noteContent)
        return false;
    refresh();
    return true;
}

QVariantList NotesBackend::noteConflicts() const
{
    const QList<SyncModel::ConflictHunk> hunks = SyncModel::parseConflicts(m_noteContent);
    const QStringList lines = m_noteContent.split(u'\n');
    const qsizetype bodyStart = SyncModel::splitEnvelope(m_noteContent).envelope.count(u'\n');
    auto side = [](const QStringList &mine, const QStringList &other) {
        const QList<bool> changed = SyncModel::linesMissingFrom(mine, other);
        QVariantList out;
        for (qsizetype i = 0; i < mine.size(); ++i)
            out << QVariantMap{ { QStringLiteral("text"), mine.at(i) }, { QStringLiteral("changed"), changed.at(i) } };
        return out;
    };
    // Up to two lines of the untouched text around a block, blank edges dropped.
    auto context = [&lines](qsizetype from, qsizetype to, bool fromEnd) {
        QStringList out = lines.mid(from, qMax<qsizetype>(0, to - from));
        while (!out.isEmpty() && out.first().trimmed().isEmpty())
            out.removeFirst();
        while (!out.isEmpty() && out.last().trimmed().isEmpty())
            out.removeLast();
        return fromEnd ? out.mid(qMax<qsizetype>(0, out.size() - 2)) : out.mid(0, 2);
    };
    QVariantList result;
    for (qsizetype h = 0; h < hunks.size(); ++h) {
        const SyncModel::ConflictHunk &hunk = hunks.at(h);
        const qsizetype prevEnd = h > 0 ? hunks.at(h - 1).last + 1 : bodyStart;
        const qsizetype nextStart = h + 1 < hunks.size() ? hunks.at(h + 1).first : lines.size();
        result << QVariantMap{ { QStringLiteral("local"), side(hunk.local, hunk.remote) },
                               { QStringLiteral("remote"), side(hunk.remote, hunk.local) },
                               { QStringLiteral("before"), context(prevEnd, hunk.first, true) },
                               { QStringLiteral("after"), context(hunk.last + 1, nextStart, false) } };
    }
    return result;
}

bool NotesBackend::noteConflictsUnreadable() const
{
    return SyncModel::hasUnreadableConflicts(m_noteContent);
}

// icloud-notes-sync's base copy of the open note: its body as last synced.
QString NotesBackend::syncedCopyPath() const
{
    const QString id = SyncModel::extractNoteId(m_noteContent);
    return id.isEmpty() ? QString() : vaultInfo().baseFiles.value(id);
}

bool NotesBackend::noteHasSyncedCopy() const
{
    const QString path = syncedCopyPath();
    return !path.isEmpty() && QFile::exists(path);
}

// Where recoverConflictedNote copies a note before replacing it: the
// engine's own state directory, which neither it nor this app reads as
// notes (the engine takes conflict-backups/ along when it moves the state).
QString NotesBackend::conflictBackupDir()
{
    return stateDir() + QStringLiteral("/conflict-backups");
}

QVariantMap NotesBackend::recoverConflictedNote(const QString &how)
{
    auto fail = [](const QString &message, const char *code = "error") {
        return QVariantMap{ { QStringLiteral("ok"), false }, { QStringLiteral("code"), QLatin1StringView(code) },
                            { QStringLiteral("message"), message } };
    };
    const QString path = noteAbsolutePath();
    if (path.isEmpty())
        return fail(QStringLiteral("No note selected."));
    if (!m_readOnlyReason.isEmpty())
        return fail(QStringLiteral("This note is read-only here. Resolve it in Apple Notes."), "read_only");
    if (m_syncRunning)
        return fail(QStringLiteral("Wait for the sync to finish, then try again."));
    if (!noteConflictsUnreadable())
        return fail(QStringLiteral("This note has no unreadable conflict markers."), "no_conflicts");
    if (noteChangedOnDisk())
        return fail(kChangedOnDiskMessage);
    const SyncModel::EnvelopeSplit split = SyncModel::splitEnvelope(m_noteContent);
    QString body;
    if (how == u"strip") {
        body = SyncModel::stripConflictMarkers(split.body);
    } else if (how == u"synced") {
        if (!noteHasSyncedCopy())
            return fail(QStringLiteral("There is no last synced version of this note."), "no_synced_copy");
        body = readText(syncedCopyPath());
    } else {
        return fail(QStringLiteral("Unknown choice."));
    }
    // A byte-exact copy first; nothing is replaced unless it is there.
    const QString dir = conflictBackupDir() + (m_currentFolder.isEmpty() ? QString() : u'/' + m_currentFolder);
    const QString stem = m_currentNote.chopped(3) + QStringLiteral(" (conflict backup ")
        + QDateTime::currentDateTime().toString(QStringLiteral("yyyy-MM-dd HHmmss"));
    QString backup = dir + u'/' + stem + QStringLiteral(").md");
    for (int n = 2; QFile::exists(backup); ++n)
        backup = dir + u'/' + stem + QStringLiteral(" %1).md").arg(n);
    if (!QDir().mkpath(dir) || !QFile::copy(path, backup) || readText(backup) != m_noteContent)
        return fail(QStringLiteral("Could not back up the note, so it was left as it is."));
    if (!writeText(path, split.envelope + body))
        return fail(QStringLiteral("Could not write the note. It is unchanged."));
    loadCurrentNote();
    rebuildNotes();
    if (!SyncModel::hasConflictMarkers(m_noteContent))
        emit vaultChanged();
    const QString shown = QDir(rootPath()).relativeFilePath(backup);
    appendLog(QStringLiteral("%1: replaced unreadable conflict markers; the note as it was is in %2")
                  .arg(vaultRelative(m_currentNote), shown));
    return { { QStringLiteral("ok"), true },
             { QStringLiteral("message"),
               (how == u"strip" ? QStringLiteral("Conflict markers removed. Read the note over: both versions' lines are kept. ")
                                : QStringLiteral("Back to the last synced version. "))
                   + QStringLiteral("The note as it was is saved in %1.").arg(shown) },
             { QStringLiteral("backup"), backup } };
}

QString NotesBackend::resolveConflicts(const QStringList &choices)
{
    if (m_currentNote.isEmpty())
        return QStringLiteral("No note selected.");
    if (!m_readOnlyReason.isEmpty())
        return refuse("read_only", QStringLiteral("This note is read-only here. Resolve it in Apple Notes."));
    if (noteChangedOnDisk())
        return kChangedOnDiskMessage;
    const QString resolved = SyncModel::resolveConflicts(m_noteContent, choices);
    if (resolved == m_noteContent)
        return QStringLiteral("Choose a version for every change first.");
    if (!writeText(noteAbsolutePath(), resolved))
        return QStringLiteral("Could not write the note.");
    loadCurrentNote();
    rebuildNotes();
    emit vaultChanged();
    return {};
}

void NotesBackend::newFolder(const QString &name)
{
    const QString clean = cleanName(name);
    if (clean.isEmpty() || !unsyncedNameReason(clean, true).isEmpty())
        return;
    QDir().mkpath(QDir(folderAbsolutePath(m_currentFolder)).filePath(clean));
    rebuildFolders();
    rewatch();
}

QString NotesBackend::renameCurrentFolder(const QString &name)
{
    const QString clean = cleanName(name);
    if (m_currentFolder.isEmpty())
        return QStringLiteral("All Notes cannot be renamed.");
    if (clean.isEmpty())
        return QStringLiteral("Name is empty.");
    if (const QString reason = unsyncedNameReason(clean, true); !reason.isEmpty())
        return refuse("usage", reason);
    const QString parent = m_currentFolder.section(u'/', 0, -2);
    const QString target = parent.isEmpty() ? clean : parent + u'/' + clean;
    if (target == m_currentFolder)
        return {};
    if (QDir(folderAbsolutePath(target)).exists())
        return refuse("exists", QStringLiteral("A folder with that name already exists."));
    if (!QDir().rename(folderAbsolutePath(m_currentFolder), folderAbsolutePath(target)))
        return QStringLiteral("Could not rename the folder.");
    m_currentFolder = target;
    emit currentFolderChanged();
    refresh();
    return {};
}

QString NotesBackend::deleteCurrentFolder()
{
    if (m_currentFolder.isEmpty())
        return QStringLiteral("All Notes cannot be deleted.");
    if (!QFile::moveToTrash(folderAbsolutePath(m_currentFolder))) // its notes go to Recently Deleted on push
        return QStringLiteral("Could not move the folder to the trash.");
    setCurrentFolder({});
    rebuildFolders();
    return {};
}

// Text matches at once. With `withMeaning` (the window), local Recall then
// ranks the same notes by meaning once typing pauses, if it is installed:
// semanticSearchFinished brings its hits. "~ query" asks for those alone.
// The window searches on every keystroke, so it reads at most the first
// kWindowSearchNotes notes and says so (searchTruncated); the CLI reads all.
QVariantList NotesBackend::searchVault(const QString &query, bool withMeaning)
{
    QVariantList out;
    bool truncated = false;
    const auto finish = qScopeGuard([&] {
        if (truncated != m_searchTruncated) {
            m_searchTruncated = truncated;
            emit searchTruncatedChanged();
        }
    });
    const quint64 generation = ++m_semanticGeneration;
    m_semanticDelay.stop();
    if (m_semanticProcess) {
        m_semanticProcess->kill();
        m_semanticProcess = nullptr;
    }
    const bool meaningOnly = query.trimmed().startsWith(u'~');
    const QString q = meaningOnly ? query.trimmed().mid(1).trimmed() : query.trimmed();
    if (q.size() < 2)
        return out;
    const QString recall = withMeaning ? QStandardPaths::findExecutable(QStringLiteral("recall")) : QString();
    const bool rank = !recall.isEmpty();
    QJsonArray records;
    QMap<QString, QVariantMap> candidates;
    const QString mode = vaultTitleMode();
    int scanned = 0;
    forEachNoteFile(rootPath(), [&](const QString &rel, const QString &path) {
        if (!rank && out.size() >= 100)
            return false;
        if (withMeaning && scanned++ == kWindowSearchNotes) {
            truncated = true;
            return false;
        }
        const QString text = readText(path, 1024 * 1024);
        const QString name = rel.section(u'/', -1);
        const QString folder = rel.section(u'/', 0, -2);
        const SyncModel::NotePreview preview = SyncModel::previewNote(text, QFileInfo(name).completeBaseName(), mode);
        if (rank) {
            records.append(QJsonObject{ { "id", rel }, { "title", preview.title.left(1000) }, { "text", text.left(4000) } });
            candidates.insert(rel, QVariantMap{ { "folder", folder }, { "file", name }, { "title", preview.title },
                                                { "snippet", preview.snippet } });
        }
        if (out.size() >= 100 || (!text.contains(q, Qt::CaseInsensitive) && !preview.title.contains(q, Qt::CaseInsensitive)))
            return true;
        QString snippet;
        for (const QString &line : text.split(u'\n')) {
            if (line.contains(q, Qt::CaseInsensitive))
                snippet = SyncModel::stripMarkdownLead(line).left(120);
            if (!snippet.isEmpty())
                break;
        }
        out << QVariantMap{ { QStringLiteral("folder"), folder },
                            { QStringLiteral("file"), name },
                            { QStringLiteral("title"), preview.title },
                            { QStringLiteral("snippet"), snippet.isEmpty() ? preview.snippet : snippet } };
        return true;
    });
    if (withMeaning && !rank && meaningOnly) {
        // Asked for meaning without Recall: say so once the caller has the text matches.
        QTimer::singleShot(0, this, [this, query, generation] {
            if (generation == m_semanticGeneration)
                emit semanticSearchFinished(query, {}, false);
        });
    } else if (rank) {
        m_semanticDelay.disconnect();
        connect(&m_semanticDelay, &QTimer::timeout, this, [=, this] {
            if (generation == m_semanticGeneration)
                rankByMeaning(recall, query, q, records, candidates, generation);
        });
        m_semanticDelay.start(meaningOnly ? 0 : 400);
    }
    return out;
}

void NotesBackend::rankByMeaning(const QString &recall, const QString &query, const QString &q,
                                 const QJsonArray &records, const QMap<QString, QVariantMap> &candidates,
                                 quint64 generation)
{
    auto *process = new QProcess(this);
    m_semanticProcess = process;
    const QString collection = "icloud-notes:"
        + QString::fromLatin1(QCryptographicHash::hash(rootPath().toUtf8(), QCryptographicHash::Sha256).toHex().left(24));
    connect(process, &QProcess::finished, this,
            [this, process, query, generation, candidates](int code, QProcess::ExitStatus status) {
                if (m_semanticProcess == process)
                    m_semanticProcess = nullptr;
                const QJsonObject reply = QJsonDocument::fromJson(process->readAllStandardOutput()).object();
                bool available = code == 0 && status == QProcess::NormalExit && reply.value("api").toInt() == 1
                    && reply.value("meaning").toBool() && !reply.contains("error");
                QVariantList hits;
                for (const auto &value : reply.value("hits").toArray()) {
                    const QString id = value.toObject().value("id").toString();
                    if (!candidates.contains(id)) {
                        available = false;
                        break;
                    }
                    hits << candidates.value(id);
                }
                if (generation == m_semanticGeneration)
                    emit semanticSearchFinished(query, hits, available);
                process->deleteLater();
            });
    connect(process, &QProcess::errorOccurred, this, [this, process, query, generation](QProcess::ProcessError error) {
        if (error == QProcess::FailedToStart) {
            if (m_semanticProcess == process)
                m_semanticProcess = nullptr;
            if (generation == m_semanticGeneration)
                emit semanticSearchFinished(query, {}, false);
            process->deleteLater();
        }
    });
    const QByteArray input = QJsonDocument(records).toJson(QJsonDocument::Compact);
    connect(process, &QProcess::started, process, [process, input] {
        process->write(input);
        process->closeWriteChannel();
    });
    process->start(recall, { "rank", collection, q });
    QTimer::singleShot(6000, process, [process] {
        if (process->state() != QProcess::NotRunning)
            process->kill();
    });
}

QString NotesBackend::toggleCheckbox(const QString &text, int line)
{
    return SyncModel::toggleCheckbox(text, line);
}

QString NotesBackend::exportPdf()
{
    if (m_currentNote.isEmpty())
        return QStringLiteral("No note selected.");
    const QString pdf = noteAbsolutePath().chopped(3) + QStringLiteral(".pdf");
    const QString name = QFileInfo(pdf).fileName();
    if (QFile::exists(pdf))
        return refuse("exists", QStringLiteral("Already exists (not overwritten): %1").arg(name));
    // In-body notes open with their title line; filename notes gain the
    // title as a heading, the way the editor shows it above the body.
    const QString title = m_noteDetails.value(m_currentNote).toMap().value(QStringLiteral("title")).toString();
    QTextDocument doc;
    doc.setMarkdown(vaultTitleMode() == u"filename" ? u"# " + title + u"\n\n" + noteBody() : noteBody());
    QPrinter printer;
    printer.setOutputFormat(QPrinter::PdfFormat);
    printer.setOutputFileName(pdf);
    doc.print(&printer);
    if (!QFile::exists(pdf))
        return QStringLiteral("Could not write the PDF.");
    setSyncMessage(QStringLiteral("Exported %1 next to the note.").arg(name));
    return {};
}

void NotesBackend::runClone()
{
    if (m_syncRunning)
        return;
    m_cloneWanted = true;
    m_cloneSignInAsked = false;
    continueClone();
}

// Where a clone waits for the sign-in: icloud-session's first answer, then
// its sign-in window. icloud-notes-sync clones the daemon's account (named
// by dsid) with the session it takes from the daemon, and never opens a
// window itself.
void NotesBackend::continueClone()
{
    if (!m_cloneWanted || m_syncRunning)
        return;
    if (!m_signInKnown) {
        if (signInPending())
            return; // the answer is on its way
        m_cloneWanted = false;
        setSyncMessage(QStringLiteral("icloud-session is not available, so Notes cannot sign in to iCloud."));
        emit cloneFinished(false);
        return;
    }
    if (m_signedIn && !m_dsid.isEmpty()) {
        m_cloneWanted = false;
        // Clone targets a fresh directory; the root doubles as that directory.
        // Titles stay the first line of each note, as in Notes.app and as
        // icloud-notes-sync defaults to; a vault cloned with --filename-as-title from
        // the CLI is still read correctly (see vaultTitleMode).
        startSync(Mode::Plain,
                  { QStringLiteral("--json"), QStringLiteral("clone"), QStringLiteral("--account"), m_dsid,
                    QStringLiteral("--non-interactive"), rootPath() },
                  QStringLiteral("Clone"));
        return;
    }
    if (!m_cloneSignInAsked) {
        m_cloneSignInAsked = true;
        signIn();
    }
}

// Pull, push, clone and diff run with --json: the envelope carries the
// result and the text report for the log (meta), and a failure is the error
// object, so its code always comes through (exit 3 is not only a sign-in).
void NotesBackend::runPull()
{
    startSync(Mode::Plain, { QStringLiteral("--json"), QStringLiteral("pull") }, QStringLiteral("Pull"));
}

void NotesBackend::runPush()
{
    startSync(Mode::Plain, { QStringLiteral("--json"), QStringLiteral("push") }, QStringLiteral("Push"));
}

void NotesBackend::runSync()
{
    startSync(Mode::Sync, { QStringLiteral("--quiet"), QStringLiteral("sync") }, QStringLiteral("Sync"));
}

void NotesBackend::refreshPushPreview()
{
    startSync(Mode::Preview, { QStringLiteral("--quiet"), QStringLiteral("status") },
              QStringLiteral("Push preview"));
}

void NotesBackend::runHistory()
{
    if (m_currentNote.isEmpty())
        return;
    startSync(Mode::History,
              { QStringLiteral("--quiet"), QStringLiteral("history"), vaultRelative(m_currentNote) },
              QStringLiteral("History"));
}

void NotesBackend::runDiff(const QString &ref)
{
    if (m_currentNote.isEmpty() || ref.trimmed().isEmpty())
        return;
    startSync(Mode::Diff, { QStringLiteral("--json"), QStringLiteral("diff"), vaultRelative(m_currentNote), ref.trimmed() },
              QStringLiteral("Diff"));
}

void NotesBackend::startSync(Mode mode, const QStringList &args, const QString &label)
{
    if (m_syncRunning)
        return;
    m_mode = mode;
    m_syncLabel = label;
    m_captured.clear();
    m_errLine.clear();
    m_runSignIns = m_signIns;
    if (mode == Mode::Sync) {
        m_stateStampBefore = stateFileStamp();
        m_vaultTouched = false;
    }
    m_syncRunning = true;
    emit syncRunningChanged();
    appendLog(QStringLiteral("$ %1 ").arg(QLatin1StringView(kSyncTool)) + args.join(u' '));
    setSyncMessage(label + QStringLiteral("…"));
    const QString tool = syncToolPath();
    if (tool.isEmpty()) {
        appendLog(kNotInstalledMessage);
        finishSync(-1);
        return;
    }
    m_syncProcess.setProgram(tool);
    m_syncProcess.setWorkingDirectory(rootPath());
    m_syncProcess.setArguments(args);
    startProcess();
}

void NotesBackend::finishSync(int exitCode)
{
    m_syncRunning = false;
    emit syncRunningChanged();
    if (isLoggedStderrLine(m_errLine))
        appendLog(QString::fromUtf8(m_errLine));
    m_errLine.clear();
    appendLog(QStringLiteral("(exit %1)").arg(exitCode));
    if (m_mode == Mode::Sync)
        finishSyncHalves(exitCode);
    else
        finishRun(m_syncLabel, exitCode, m_captured);

    if (m_syncWhenIdle && !m_authExpired) {
        m_syncWhenIdle = false;
        runSync(); // a sign-in arrived while this ran
    }
    continueClone(); // a clone asked for while something else ran
    if (!m_syncRunning) {
        writeQueuedSave();
        emit syncChainFinished();
    }
}

void NotesBackend::finishRun(const QString &label, int exitCode, const QByteArray &output)
{
    m_syncLabel = label;
    m_lastExit = exitCode;
    m_captured = output;
    // status, push --dry-run and diff report their changes in the result and
    // exit 0; only a non-zero exit is a failure.
    const bool ok = exitCode == 0;

    // A --json run (pull, push, clone, diff): the envelope's data is the
    // result, its meta the text report, logged here.
    const bool enveloped = m_mode == Mode::Plain || m_mode == Mode::Diff;
    QString diffText;
    // A push that had a change refused exits 1 with its result all the same.
    const bool refused = !ok && enveloped && !successEnvelope(output).isEmpty();
    if ((ok || refused) && enveloped) {
        const QJsonObject envelope = successEnvelope(output);
        const QJsonValue data = envelope.value(QStringLiteral("data"));
        const QJsonObject meta = envelope.value(QStringLiteral("meta")).toObject();
        m_captured = data.isObject()  ? QJsonDocument(data.toObject()).toJson(QJsonDocument::Compact)
                     : data.isArray() ? QJsonDocument(data.toArray()).toJson(QJsonDocument::Compact)
                                      : QByteArray();
        if (m_mode == Mode::Diff) {
            diffText = meta.value(QStringLiteral("text")).toString();
        } else {
            const QStringList lines = strings(meta.value(QStringLiteral("lines")));
            const QString report = lines.isEmpty() ? envelope.value(QStringLiteral("summary")).toString() : lines.join(u'\n');
            if (!report.isEmpty())
                appendLog(report);
        }
    }

    QVariantMap parsed;
    if (ok && m_mode == Mode::Preview)
        parsed = SyncModel::parseStatusJson(m_captured);
    else if (ok && m_mode == Mode::History)
        parsed = SyncModel::parseHistoryJson(m_captured);
    // iCloud out of reach is not worth a failure: the next sync tries again.
    m_lastError = ok        ? QJsonObject()
                  : refused ? refusedChange(QJsonDocument::fromJson(m_captured).object())
                            : engineError(output);
    // A failure's output is its error, as a failed half's is: {"error":{...}}.
    if (!m_lastError.isEmpty())
        m_captured = QJsonDocument(QJsonObject{ { QStringLiteral("error"), m_lastError } }).toJson(QJsonDocument::Compact);
    const QString code = m_lastError.value(QStringLiteral("code")).toString();
    const QString message = m_lastError.value(QStringLiteral("message")).toString();
    const bool offline = !ok && isNetworkError(code);
    // A run's error is only on stdout (every run is in a machine mode): log
    // it as a sync half's is (finishSyncHalves logs the halves'), and say it
    // in the preview's or history's error rather than "see log".
    if (m_mode != Mode::Sync && !m_lastError.isEmpty()) {
        appendLog(runFailedLog(label.toLower(), m_lastError));
    }
    const QString error = ok                    ? parsed.value(QStringLiteral("error")).toString()
                          : !message.isEmpty() ? QStringLiteral("%1 failed: %2").arg(label, message)
                                               : QStringLiteral("%1 failed (exit %2). See log.").arg(label).arg(exitCode);

    // A failure that only a sign-in fixes says `sign_in_required` (exit 3,
    // which it shares with "not cloned", "another account's vault" and the
    // like; the app's own arguments never make a usage error). Notes never
    // signs in by itself: icloud-session owns the sign-in, so it is told (it
    // confirms with Apple before signing everyone out) and syncing pauses
    // until it reports a sign-in, since every further sync would fail the
    // same way. `session_rejected` (Apple refused a session icloud-session
    // still holds) pauses the same way, but is not reported: the session
    // library reported it already when it got the refusal.
    // A sign-in that arrived while this ran supersedes the session it was
    // refused: no report and no pause, but one more sync with the new one.
    const bool staleRefusal = !ok && needsSignIn(code, exitCode) && m_runSignIns != m_signIns;
    const bool sessionExpired = !ok && !staleRefusal && isSignInRequired(code, exitCode);
    const bool sessionRejected = !ok && !staleRefusal && isSessionRejected(code);
    if (staleRefusal && cloned())
        m_syncWhenIdle = true;
    if (sessionExpired)
        callSession(QStringLiteral("ReportSignInRequired"));
    // A pull or clone always talks to iCloud, so one that worked proves the
    // session; a push with nothing to send never checks it.
    // Only that re-arms the retry: a push that sent nothing proves nothing,
    // and clearing it there let push-ok, pull-refused retry forever.
    const bool sessionWorks = ok && (label == u"Pull" || label == u"Clone");
    if (sessionWorks)
        m_retriedAfterReport = false;
    setAuthExpired(sessionExpired || sessionRejected || (m_authExpired && !sessionWorks));
    emit syncFinished(label, ok);

    switch (m_mode) {
    case Mode::Plain:
        setPushPreview({}, {}); // a pull or push makes the last preview stale
        refresh(); // a pull or clone changes files behind our back
        if (label == u"Clone") {
            // A clone that failed part way records what it wrote, and the
            // vault counts as cloned from then on: a pull finishes it, so
            // the window syncs at once (unless the sign-in is what failed)
            // and does not wait for its next poll.
            if (!ok && m_role == Role::App && cloned())
                m_syncWhenIdle = true;
            emit cloneFinished(ok);
        }
        break;
    case Mode::Sync:
        break; // re-read once both halves are in (finishSyncHalves)
    case Mode::Preview:
        setPushPreview(parsed, error);
        emit pushPreviewReady(error.isEmpty());
        break;
    case Mode::History:
        m_historyEntries = parsed.value(QStringLiteral("epochs")).toList();
        m_diffText.clear();
        m_historyError = error;
        emit historyChanged();
        emit historyReady(error.isEmpty());
        break;
    case Mode::Diff:
        m_diffText = diffText;
        m_historyError = error;
        emit historyChanged();
        emit historyReady(error.isEmpty());
        break;
    }
    // The sign-in banner already says what went wrong and how to fix it.
    setSyncMessage(sessionExpired || sessionRejected ? kPausedMessage
                   : offline                         ? kOfflineMessage
                                                     : label + (error.isEmpty() ? QStringLiteral(" done.")
                                                                                : QStringLiteral(" failed. See log.")));
}

// `icloud-notes-sync --quiet sync` answers {push, pull, vault_info}, each half
// {ok, exit_code, result, lines} or {ok, exit_code, error}, pull null when it
// did not run (the push found the sign-in gone or the session rejected, or
// iCloud out of reach).
// Each half is logged and finished as a run of its own was, so syncFinished
// says "Push", then "Pull"; then the vault is re-read, once, and only when
// the run may have changed it.
void NotesBackend::finishSyncHalves(int exitCode)
{
    const QJsonObject answer = QJsonDocument::fromJson(m_captured).object();
    struct Half {
        QString label;
        int exitCode;
        QByteArray output;
    };
    QList<Half> halves;
    // Nothing a re-read would show: no half failed but for a sign-in or the network.
    bool quiet = true;
    bool pulledChanges = false;
    auto take = [&](const QString &label, const QJsonObject &half) {
        const QString name = label.toLower();
        const bool ok = half.value(QStringLiteral("ok")).toBool();
        QJsonObject output;
        // A failed push that ran but had a change refused has lines too:
        // what it did do, which goes to the log ahead of the failure.
        const QStringList lines = strings(half.value(QStringLiteral("lines")));
        if (ok || !lines.isEmpty())
            appendLog(name + u':' + (lines.isEmpty() ? QString() : u'\n' + lines.join(u'\n')));
        if (ok) {
            output = half.value(QStringLiteral("result")).toObject();
        } else {
            const QJsonObject error = half.value(QStringLiteral("error")).toObject();
            output.insert(QStringLiteral("error"), error);
            appendLog(runFailedLog(name, error));
            const QString code = error.value(QStringLiteral("code")).toString();
            if (!isNetworkError(code))
                quiet = quiet && (code == u"sign_in_required" || isSessionRejected(code));
        }
        const int code = half.value(QStringLiteral("exit_code")).toInt(ok ? 0 : 1);
        halves << Half{ label, code, QJsonDocument(output).toJson(QJsonDocument::Compact) };
        return output;
    };

    const QJsonObject push = answer.value(QStringLiteral("push")).toObject();
    if (push.isEmpty()) {
        // No answer (no engine, no lock, the vault busy, a crash): both halves
        // failed as the run did, as two runs would have; a sign-in skips the
        // pull. The engine's error object, when it printed one, is on stdout.
        const int code = exitCode == 0 ? -1 : exitCode;
        const QJsonObject error = engineError(m_captured);
        QByteArray output;
        if (!error.isEmpty()) {
            appendLog(failedLog(QStringLiteral("sync"), error));
            output = QJsonDocument(QJsonObject{ { QStringLiteral("error"), error } }).toJson(QJsonDocument::Compact);
        }
        halves << Half{ QStringLiteral("Push"), code, output };
        if (!needsSignIn(error.value(QStringLiteral("code")).toString(), code))
            halves << Half{ QStringLiteral("Pull"), code, output };
        quiet = false;
    } else {
        take(QStringLiteral("Push"), push);
        if (answer.value(QStringLiteral("pull")).isObject()) {
            const QJsonObject pulled = take(QStringLiteral("Pull"), answer.value(QStringLiteral("pull")).toObject());
            for (const char *count : { "added", "updated", "merged", "removed" })
                pulledChanges = pulledChanges || pulled.value(QLatin1StringView(count)).toInt() > 0;
            pulledChanges = pulledChanges || !pulled.value(QStringLiteral("changes")).toArray().isEmpty()
                || !pulled.value(QStringLiteral("conflicts")).toArray().isEmpty();
        } else if (!needsSignIn(push.value(QStringLiteral("error")).toObject().value(QStringLiteral("code")).toString(),
                                halves.first().exitCode)) {
            // Not run, and not for a sign-in (sign_in_required or
            // session_rejected, which pause instead): the push found iCloud
            // out of reach. It failed as the push did.
            halves << Half{ QStringLiteral("Pull"), halves.first().exitCode, halves.first().output };
        }
    }
    for (const Half &half : halves)
        finishRun(half.label, half.exitCode, half.output);

    setPushPreview({}, {}); // a sync makes the last preview stale
    // The answer carries what vault-info would say now: no second engine run.
    if (answer.value(QStringLiteral("vault_info")).isObject())
        adoptVaultInfo(answer.value(QStringLiteral("vault_info")).toObject());
    const QString notePath = noteAbsolutePath();
    if (!quiet || pulledChanges || m_vaultTouched || stateFileStamp() != m_stateStampBefore)
        refresh(); // the pull (or something beside it) changed files
    else if (!notePath.isEmpty() && (!QFile::exists(notePath) || readText(notePath) != m_noteContent))
        refresh(); // the open note changed under the sync, from elsewhere
}

// A save refused while the sync ran, now that it is done and its changes
// are loaded: a note the sync left alone is saved as asked; one it changed
// gets the edits merged in, as any change on disk under edits does; one it
// deleted gets them as a new note. (The window may already have merged
// them on the refresh, which clears this.)
void NotesBackend::writeQueuedSave()
{
    const QueuedSave queued = std::exchange(m_queuedSave, {});
    if (!queued.valid)
        return;
    const QString &base = queued.base;
    // Its note is gone (and nobody deleted it here): the edits follow it to
    // where it moved, or become a note of their own.
    if (m_currentNote.isEmpty()) {
        if (m_lostNote.valid && keepEditsAsConflict(base, queued.body))
            emit queuedSaveWritten(queued.body);
        return;
    }
    if (vaultRelative(m_currentNote) != queued.note) // another note is not theirs
        return;
    const bool changed = SyncModel::editorForm(noteBody()) != base;
    bool written = changed && keepEditsAsConflict(base, queued.body);
    // Saving over the note is only for one the sync left as the edits
    // started, or that already holds them: never over a change a merge
    // could not take (a failed write), which would drop it.
    const QString theirs = SyncModel::editorForm(noteBody());
    if (!written && (!changed || theirs == base || theirs == queued.body)) {
        // The window may hold later typing by now: its reload must not take
        // the text written here for a change from elsewhere and merge the
        // two into a conflict (it rebases on queuedSaveWritten instead).
        const bool wasMerging = std::exchange(m_merging, true);
        written = saveCurrentNote(queued.body);
        m_merging = wasMerging;
    }
    if (written)
        emit queuedSaveWritten(queued.body);
}

void NotesBackend::setPushPreview(const QVariantMap &parsed, const QString &error)
{
    m_statusEntries = parsed.value(QStringLiteral("entries")).toList();
    m_statusUnchanged = parsed.value(QStringLiteral("unchanged")).toInt();
    m_statusNotices = parsed.value(QStringLiteral("notices")).toStringList();
    m_statusError = error;
    emit pushPreviewChanged();
}

void NotesBackend::appendLog(const QString &text)
{
    if (!m_syncLog.isEmpty())
        m_syncLog += u'\n';
    m_syncLog += text;
    // Keep the log bounded; it is a readout, not history.
    if (m_syncLog.size() > 200000)
        m_syncLog = m_syncLog.right(200000);
    emit syncLogChanged();
}

void NotesBackend::clearLog()
{
    m_syncLog.clear();
    emit syncLogChanged();
}

// Closing while icloud-notes-sync runs: ~QProcess stops it and waits, and
// that wait delivers finished(). By then the members declared after
// m_syncProcess are gone, so finishSync ran on them (a crash on quit).
// Nothing a run reports matters once Notes closes.
NotesBackend::~NotesBackend()
{
    m_syncProcess.disconnect(this);
    writeQueuedSaveOnClose();
}

void NotesBackend::writeQueuedSaveOnClose()
{
    if (!closeWritesQueuedSave())
        return;
    m_syncProcess.kill();
    m_syncProcess.waitForFinished();
    m_syncRunning = false;
    refresh();
    writeQueuedSave();
}

QDBusMessage NotesBackend::sessionCall(const QString &method) const
{
    return QDBusMessage::createMethodCall(kSessionService, kSessionPath, kSessionService, method);
}

// SignIn() answers through property changes. ReportSignInRequired() answers
// whether the session still works: icloud-session checked it with Apple and
// refreshed the sync tool's copy, so its refusal came from a stale copy
// and one retry should go through (once, until a pull works or a new
// sign-in arrives). An answer that comes while something else runs retries
// once that is done. A failure (no daemon, or no answer in time) is logged;
// SignIn's is named, and a report's leaves the sign-in unknown, not paused.
void NotesBackend::callSession(const QString &method)
{
    QDBusConnection bus = QDBusConnection::sessionBus();
    if (!bus.isConnected()) {
        appendLog(QStringLiteral("icloud-session: no D-Bus session bus"));
        return;
    }
    ++m_sessionCalls;
    const bool report = method == u"ReportSignInRequired";
    auto *watcher = new QDBusPendingCallWatcher(
        bus.asyncCall(sessionCall(method), report ? kReportTimeoutMs : kSessionTimeoutMs), this);
    connect(watcher, &QDBusPendingCallWatcher::finished, this, [this, method, report](QDBusPendingCallWatcher *call) {
        call->deleteLater();
        --m_sessionCalls;
        if (!call->isError()) {
            const QDBusPendingReply<bool> reply = *call;
            if (report && reply.argumentAt<0>() && !m_retriedAfterReport) {
                m_retriedAfterReport = true;
                setAuthExpired(false);
                resumeSync();
            }
            return;
        }
        appendLog(QStringLiteral("icloud-session %1 failed: %2").arg(method, call->error().message()));
        if (report && !(m_signInKnown && !m_signedIn)) {
            // Nobody could say whether the sign-in still works: unknown,
            // which never pauses; the next sync finds out again.
            setAuthExpired(false);
            if (!m_syncRunning)
                setSyncMessage(QStringLiteral("Sync failed: iCloud refused the sign-in, and icloud-session "
                                              "could not check it. See log."));
        }
        if (method == u"SignIn") {
            setSyncMessage(QStringLiteral("Could not open the iCloud sign-in: icloud-session is not available."));
            if (m_cloneWanted) {
                m_cloneWanted = false;
                emit cloneFinished(false);
            }
        }
    });
}

void NotesBackend::signIn()
{
    callSession(QStringLiteral("SignIn"));
    setSyncMessage(QStringLiteral("Sign in to iCloud in the window that opened. Syncing resumes on its own."));
}

void NotesBackend::refreshSignIn()
{
    QDBusConnection bus = QDBusConnection::sessionBus();
    if (!bus.isConnected()) {
        emit signInChanged(); // stays unknown
        return;
    }
    QDBusMessage getAll = QDBusMessage::createMethodCall(kSessionService, kSessionPath, kPropertiesInterface,
                                                         QStringLiteral("GetAll"));
    getAll << kSessionService;
    ++m_signInReads;
    auto *watcher = new QDBusPendingCallWatcher(bus.asyncCall(getAll, kSessionTimeoutMs), this);
    connect(watcher, &QDBusPendingCallWatcher::finished, this, [this](QDBusPendingCallWatcher *call) {
        call->deleteLater();
        --m_signInReads;
        const QDBusPendingReply<QVariantMap> reply = *call;
        if (reply.isError()) {
            // No daemon (not installed, or it failed to start): unknown,
            // which shows no banner and never pauses syncing.
            m_signInKnown = false;
            m_signingIn = false;
            emit signInChanged();
            continueClone();
            return;
        }
        applySignIn(reply.value());
    });
}

void NotesBackend::sessionPropertiesChanged(const QString &interface, const QVariantMap &changed,
                                            const QStringList &invalidated)
{
    if (interface != kSessionService)
        return;
    // Before a full read has answered, a few changed properties are not the
    // whole picture (a signed-out default would pause syncing): read it all.
    if (!invalidated.isEmpty() || !m_signInKnown)
        refreshSignIn();
    else if (!changed.isEmpty())
        applySignIn(changed);
}

// Takes a full read or just the changed properties. A sign-in (SignedIn
// turning true, a fresh expiry while signed in, or the sign-in window
// closing while signed in, as a re-sign-in with the same expiry does)
// resumes paused syncing and a waiting clone, and re-arms the one retry
// after a report; signed out pauses syncing without trying first.
void NotesBackend::applySignIn(const QVariantMap &properties)
{
    const bool wasSignedIn = m_signInKnown && m_signedIn;
    const bool wasSigningIn = m_signingIn;
    const QString dsidBefore = m_dsid;
    m_signInKnown = true;
    if (properties.contains(QStringLiteral("SignedIn")))
        m_signedIn = properties.value(QStringLiteral("SignedIn")).toBool();
    if (properties.contains(QStringLiteral("AppleId")))
        m_appleId = properties.value(QStringLiteral("AppleId")).toString();
    if (properties.contains(QStringLiteral("Dsid")))
        m_dsid = properties.value(QStringLiteral("Dsid")).toString();
    if (properties.contains(QStringLiteral("ExpiresAt")))
        m_expiresAt = properties.value(QStringLiteral("ExpiresAt")).toULongLong();
    if (properties.contains(QStringLiteral("SigningIn")))
        m_signingIn = properties.value(QStringLiteral("SigningIn")).toBool();

    if (m_signedIn) {
        // A sign-in, not a validate: ExpiresAt moves with every token
        // rotation, so it says nothing about whether anyone signed in.
        const bool signedInAnew = !wasSignedIn || m_dsid != dsidBefore || (wasSigningIn && !m_signingIn);
        if (signedInAnew) {
            m_retriedAfterReport = false;
            ++m_signIns;
        }
        if (m_authExpired && signedInAnew) {
            setAuthExpired(false);
            setSyncMessage(QStringLiteral("Signed in."));
            resumeSync();
        }
    } else {
        if (cloned() && !m_authExpired) {
            setAuthExpired(true);
            if (!m_syncRunning)
                setSyncMessage(kPausedMessage);
        }
        if (m_cloneWanted && m_cloneSignInAsked && wasSigningIn && !m_signingIn) {
            // The sign-in window closed without a sign-in.
            m_cloneWanted = false;
            setSyncMessage(QStringLiteral("Not signed in, so nothing was cloned."));
            emit cloneFinished(false);
        }
    }
    emit signInChanged();
    continueClone();
}

void NotesBackend::resumeSync()
{
    if (!cloned())
        return;
    if (m_syncRunning)
        m_syncWhenIdle = true;
    else
        runSync(); // what was waiting on the session
}

int NotesBackend::signInDaysLeft() const
{
    if (!signedIn())
        return -2;
    const qint64 now = QDateTime::currentSecsSinceEpoch();
    if (m_expiresAt == 0 || qint64(m_expiresAt) <= now)
        return -1;
    return int((qint64(m_expiresAt) - now) / 86400);
}

void NotesBackend::setAuthExpired(bool expired)
{
    if (m_authExpired == expired)
        return;
    m_authExpired = expired;
    emit authExpiredChanged();
}

void NotesBackend::setSyncMessage(const QString &text)
{
    if (m_syncMessage == text)
        return;
    m_syncMessage = text;
    emit syncMessageChanged();
}
