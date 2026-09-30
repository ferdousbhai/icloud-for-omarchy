#include "notesbackend.h"
#include "markdownhighlighter.h"
#include "syncmodel.h"

#include <QDBusConnection>
#include <QDBusMessage>
#include <QDBusPendingCallWatcher>
#include <QDBusPendingReply>
#include <QDBusServiceWatcher>
#include <QDateTime>
#include <QDir>
#include <QDirIterator>
#include <QFile>
#include <QFileInfo>
#include <QFontDatabase>
#include <QGuiApplication>
#include <QJsonArray>
#include <QJsonDocument>
#include <QJsonObject>
#include <QPrinter>
#include <QRegularExpression>
#include <QStandardPaths>
#include <QTextDocument>
#include <QTextStream>
#include <QUrl>
#include <algorithm>
#include <fcntl.h>
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

// The sync engine: icloud-notes-sync, a Rust port of icloud-md that takes
// the sign-in from icloud-session. The icloud-notes package installs it
// off PATH, in kSyncToolPath; see NotesBackend::syncToolPath.
constexpr char kSyncTool[] = "icloud-notes-sync";
constexpr char kSyncToolPath[] = "/usr/lib/icloud-notes/icloud-notes-sync";
const QString kNotInstalledMessage =
    QStringLiteral("The sync engine (icloud-notes-sync) is missing. Reinstall icloud-notes to sync.");

// A note larger than this is not a note anymore; the guardrail scans
// stop here so a stray huge file cannot stall the list.
constexpr qsizetype kScanLimit = 2 * 1024 * 1024;

QString readText(const QString &path, qsizetype limit = -1)
{
    QFile file(path);
    if (!file.open(QIODevice::ReadOnly | QIODevice::Text))
        return {};
    QTextStream in(&file);
    in.setEncoding(QStringConverter::Utf8);
    return limit < 0 ? in.readAll() : in.read(limit);
}

bool writeText(const QString &path, const QString &text)
{
    QFile file(path);
    if (!file.open(QIODevice::WriteOnly | QIODevice::Text | QIODevice::Truncate))
        return false;
    QTextStream out(&file);
    out.setEncoding(QStringConverter::Utf8);
    out << text;
    return true;
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

QString sanitized(const QString &name)
{
    QString clean = name.trimmed();
    clean.remove(u'/');
    clean.remove(u'\0');
    return clean;
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
// Preview-only: attachment-bearing notes are read-only upstream.
QVariantList attachmentsFor(const QString &notePath, const QString &text)
{
    static const QStringList imageSuffixes{ QStringLiteral("png"), QStringLiteral("jpg"),
                                            QStringLiteral("jpeg"), QStringLiteral("gif"),
                                            QStringLiteral("webp"), QStringLiteral("svg") };
    static const QRegularExpression linkRe(QStringLiteral(R"(\[[^\]]*\]\((attachments/[^)\s]+)\))"));
    const QDir dir = QFileInfo(notePath).dir();
    QVariantList found;
    QSet<QString> seen;
    for (const QRegularExpressionMatch &match : linkRe.globalMatch(text)) {
        const QString abs = dir.absoluteFilePath(QUrl(match.captured(1)).path()); // links are URL-encoded
        if (seen.contains(abs) || !QFile::exists(abs))
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

    // External changes (an icloud-notes-sync pull in a terminal, say) re-list;
    // a change to the open note is reported so unsaved edits are kept.
    connect(&m_watcher, &QFileSystemWatcher::directoryChanged, this, [this] {
        rebuildFolders();
        rebuildNotes();
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

    // Separate channels: with --json, stdout carries only the JSON result
    // (parsed), and progress, warnings and errors go to stderr (logged).
    connect(&m_syncProcess, &QProcess::readyReadStandardOutput, this, [this] {
        const QByteArray out = m_syncProcess.readAllStandardOutput();
        m_captured += out;
        appendLog(QString::fromUtf8(out));
    });
    connect(&m_syncProcess, &QProcess::readyReadStandardError, this, [this] {
        const QByteArray err = m_syncProcess.readAllStandardError();
        m_capturedErr += err;
        appendLog(QString::fromUtf8(err));
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
QString NotesBackend::rootPath()
{
    const QString override = qEnvironmentVariable("ICLOUD_NOTES_VAULT");
    if (!override.isEmpty())
        return override;
    return QStandardPaths::writableLocation(QStandardPaths::DocumentsLocation)
        + QStringLiteral("/icloud-notes");
}

// In the runtime directory, or beside the vault without one: never inside
// it, since icloud-notes-sync clones only into an empty directory. One per
// vault, named by a hash of its canonical path (its cleaned absolute path
// until it exists), so tests never share the real vault's. icloud-notes-sync
// computes the same path (notes-sync/src/cmd/lock.rs): change both or
// neither.
QString NotesBackend::lockPath()
{
    const QFileInfo vault(rootPath());
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
        setSyncMessage(QStringLiteral("Waiting for %1 to finish…").arg(lockHolder()));
        if (!m_lockRetry.isActive())
            m_lockRetry.start();
        return;
    case VaultLock::Failed:
        appendLog(QStringLiteral("Could not open the sync lock %1; not running icloud-notes-sync without it.")
                      .arg(m_lock.path()));
        finishSync(-1);
        return;
    }
}

// Whoever held the vault let go: show what it pulled, then run the sync
// that waited for it.
void NotesBackend::retryLock()
{
    switch (lockVault()) {
    case VaultLock::Busy:
        if (m_syncRunning) // the holder may have changed
            setSyncMessage(QStringLiteral("Waiting for %1 to finish…").arg(lockHolder()));
        return;
    case VaultLock::Failed:
        m_lockRetry.stop();
        if (m_syncRunning) {
            appendLog(QStringLiteral("Could not open the sync lock %1; not running icloud-notes-sync without it.")
                          .arg(m_lock.path()));
            finishSync(-1);
        }
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

// A clone leaves the engine's state directory (icloud-md's .icloud-md,
// which icloud-notes-sync keeps) in the vault.
bool NotesBackend::vaultCloned()
{
    return QDir(rootPath() + QStringLiteral("/.icloud-md")).exists();
}

const NotesBackend::VaultInfo &NotesBackend::vaultInfo() const
{
    const QString tool = syncToolPath();
    const QString root = QFileInfo(rootPath()).absoluteFilePath();
    auto keyFor = [&](const QString &stateFile) {
        const QFileInfo state(stateFile);
        return tool + u'\n' + root + u'\n'
            + (!stateFile.isEmpty() && state.exists()
                   ? QString::number(state.lastModified().toMSecsSinceEpoch()) + u':' + QString::number(state.size())
                   : QStringLiteral("-"));
    };
    // Taken before asking, so a change while the engine answers asks again.
    const QString key = keyFor(m_vaultInfo.stateFile);
    if (!m_vaultInfoKey.isEmpty() && key == m_vaultInfoKey)
        return m_vaultInfo;

    VaultInfo info;
    if (!tool.isEmpty()) {
        QProcess engine;
        engine.start(tool, { QStringLiteral("--json"), QStringLiteral("vault-info"), root });
        if (engine.waitForFinished(10000) && engine.exitStatus() == QProcess::NormalExit && engine.exitCode() == 0) {
            const QJsonObject o = QJsonDocument::fromJson(engine.readAllStandardOutput()).object();
            if (o.value(QStringLiteral("titleMode")).toString() == u"filename")
                info.titleMode = QStringLiteral("filename");
            info.defaultFolderDir = o.value(QStringLiteral("defaultFolderDir")).toString();
            info.stateFile = o.value(QStringLiteral("stateFile")).toString();
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
        } else if (engine.state() != QProcess::NotRunning) {
            engine.kill();
            engine.waitForFinished();
        }
    }
    // The first answer names the state file: only then can its change be seen.
    m_vaultInfoKey = info.stateFile == m_vaultInfo.stateFile ? key : keyFor(info.stateFile);
    m_vaultInfo = info;
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
        if (!isHidden(rel) && !rel.split(u'/').contains(QStringLiteral("attachments")))
            folders << rel;
    }
    SyncModel::sortFolders(folders, defaultFolder());

    // Every note counts toward each folder above it, the root included.
    QVariantMap counts;
    for (const QString &folder : folders)
        counts.insert(folder, 0);
    QDirIterator files(rootPath(), { QStringLiteral("*.md") }, QDir::Files, QDirIterator::Subdirectories);
    while (files.hasNext()) {
        QString rel = root.relativeFilePath(files.next());
        if (isHidden(rel))
            continue;
        do {
            rel = rel.section(u'/', 0, -2);
            if (counts.contains(rel))
                counts[rel] = counts[rel].toInt() + 1;
        } while (!rel.isEmpty());
    }

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
    QFileInfoList entries =
        QDir(folderAbsolutePath(m_currentFolder)).entryInfoList({ QStringLiteral("*.md") }, QDir::Files);
    // The sync tool syncs note mtimes, so newest-first matches Notes.app ordering.
    std::sort(entries.begin(), entries.end(),
              [](const QFileInfo &a, const QFileInfo &b) { return a.lastModified() > b.lastModified(); });
    QStringList found;
    for (const QFileInfo &info : entries)
        found << info.fileName();
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
// untracked notes carrying an id from elsewhere, conflicts and tables.
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
            const SyncModel::NotePreview preview = SyncModel::previewNote(text, name.chopped(3), mode);
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
        if (!tracked.contains(vaultRelative(name)))
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
    const QDir root(rootPath());
    QDirIterator it(rootPath(), { QStringLiteral("*.md") }, QDir::Files, QDirIterator::Subdirectories);
    while (it.hasNext()) {
        const QString rel = root.relativeFilePath(it.next());
        if (isHidden(rel) || rel.split(u'/').contains(QStringLiteral("attachments")))
            continue;
        if (SyncModel::extractNoteId(readText(it.filePath(), 64 * 1024)) == id)
            return rel;
    }
    return {};
}

// Open the note with this id where it now is. False when there is none.
bool NotesBackend::followNote(const QString &id)
{
    const QString rel = m_following ? QString() : findNoteById(id);
    if (rel.isEmpty())
        return false;
    m_following = true;
    const QString folder = rel.section(u'/', 0, -2);
    if (folder != m_currentFolder) {
        m_currentFolder = folder;
        emit currentFolderChanged();
    }
    m_currentNote = rel.section(u'/', -1);
    m_lostNote = {};
    emit currentNoteChanged();
    rebuildNotes();
    loadCurrentNote(); // unsaved edits merge here (keepEditsAsConflict)
    rewatch();
    m_following = false;
    return true;
}

// The open note's file is gone and no file carries its id: close it, but
// remember it, so unsaved edits can still be kept (keepEditsAsNewNote).
void NotesBackend::loseCurrentNote()
{
    m_lostNote = { m_currentFolder,
                   SyncModel::previewNote(m_noteContent, m_currentNote.chopped(3), vaultTitleMode()).title,
                   m_noteContent, true };
    closeNote();
}

// Unsaved edits that cannot go into their note (deleted elsewhere, or
// carrying conflict markers a merge would nest): a new note beside where
// it was, never the note's id (that would bring a deleted one back as a
// copy). `why` gets the note's title and the new note's as %1 and %2.
bool NotesBackend::keepEditsAsNewNote(const QString &mine, const QString &why)
{
    const QString folder = QDir(folderAbsolutePath(m_lostNote.folder)).exists() ? m_lostNote.folder : QString();
    QString title = sanitized(m_lostNote.title);
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
    if (folder != m_currentFolder) {
        m_currentFolder = folder;
        emit currentFolderChanged();
    }
    m_currentNote = name;
    emit currentNoteChanged();
    rebuildNotes();
    loadCurrentNote();
    rewatch();
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
    m_lostNote = {};
    if (m_currentNote != name) {
        m_currentNote = name;
        emit currentNoteChanged();
    }
    loadCurrentNote();
    rewatch();
}

bool NotesBackend::saveCurrentNote(const QString &body)
{
    const QString path = noteAbsolutePath();
    if (path.isEmpty()) // the note went away under these edits: keepEditsAsConflict keeps them
        return body.isEmpty();
    const QString text = assembleNote(SyncModel::restoreEditorChars(noteBody(), body));
    if (!m_readOnlyReason.isEmpty() || text == m_noteContent)
        return true;
    // Never under a running sync: a pull may be rewriting this very file.
    // Written once it is done and its changes are loaded (writeQueuedSave).
    if (m_syncRunning) {
        m_queuedSave = { m_noteContent, body, true };
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

bool NotesBackend::keepEditsAsConflict(const QString &base, const QString &mine)
{
    if (m_merging)
        return false;
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
        m_lostNote = { m_currentFolder,
                       SyncModel::previewNote(disk, m_currentNote.chopped(3), vaultTitleMode()).title, disk, true };
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
    // The reload reaches the window, whose own unsaved text is what was
    // just merged: never merge it into the result a second time.
    const bool wasMerging = std::exchange(m_merging, true);
    loadCurrentNote();
    m_merging = wasMerging;
    rebuildNotes();
    if (!SyncModel::hasConflictMarkers(merged))
        emit vaultChanged(); // a clean merge syncs as any edit
    return true;
}

QString NotesBackend::saveWarning(const QString &body)
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
        if (id.isEmpty() || name == m_currentNote || m_scans.value(dir.filePath(name)).id != id)
            continue;
        warnings << QStringLiteral("Another note in this folder (%1) carries the same apple-note-id. "
                                   "Pushing two files with one id is ambiguous, so keep only one.")
                        .arg(name);
        break;
    }
    return warnings.join(QStringLiteral("\n\n"));
}

void NotesBackend::newNote(const QString &name)
{
    QString clean = sanitized(name);
    if (clean.isEmpty())
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
    const QString clean = sanitized(title);
    if (m_currentNote.isEmpty())
        return QStringLiteral("No note selected.");
    if (!m_readOnlyReason.isEmpty())
        return QStringLiteral("This note is read-only here. Rename it in Apple Notes.");
    if (clean.isEmpty())
        return QStringLiteral("Title is empty.");
    if (vaultTitleMode() == u"filename") {
        // The file name IS the title: renaming the file retitles the note.
        const QString target = clean.endsWith(QStringLiteral(".md"), Qt::CaseInsensitive)
            ? clean
            : clean + QStringLiteral(".md");
        if (target == m_currentNote)
            return {};
        const QString path = QDir(folderAbsolutePath(m_currentFolder)).filePath(target);
        if (QFile::exists(path))
            return QStringLiteral("A note with that name already exists.");
        if (!QFile::rename(noteAbsolutePath(), path))
            return QStringLiteral("Could not rename the file.");
        m_currentNote = target;
        emit currentNoteChanged();
    } else {
        // In-body vaults carry the title in the first line: retitle that line.
        const QString updated = SyncModel::retitleInBody(m_noteContent, clean);
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
        return QStringLiteral("This note is read-only here. Move it in Apple Notes.");
    if (!m_noteAttachments.isEmpty())
        return QStringLiteral("This note has attachments, whose links are relative to its folder. Move it in Apple Notes.");
    if (!m_folders.contains(folder))
        return QStringLiteral("No folder \"%1\".").arg(folder);
    if (folder == m_currentFolder)
        return {};
    const QString target = QDir(folderAbsolutePath(folder)).filePath(m_currentNote);
    if (QFile::exists(target))
        return QStringLiteral("A note with that name already exists in that folder.");
    if (!QFile::rename(noteAbsolutePath(), target))
        return QStringLiteral("Could not move the file.");
    const QString name = m_currentNote;
    m_lostNote = {};
    m_currentFolder = folder;
    emit currentFolderChanged();
    m_currentNote = name;
    emit currentNoteChanged();
    rebuildFolders();
    rebuildNotes();
    loadCurrentNote();
    rewatch();
    emit vaultChanged();
    return {};
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

// Where recoverConflictedNote copies a note before replacing it: the sync
// tool's own dot-directory, which neither it nor this app reads as notes.
QString NotesBackend::conflictBackupDir()
{
    return rootPath() + QStringLiteral("/.icloud-md/conflict-backups");
}

QVariantMap NotesBackend::recoverConflictedNote(const QString &how)
{
    auto fail = [](const QString &message) {
        return QVariantMap{ { QStringLiteral("ok"), false }, { QStringLiteral("message"), message } };
    };
    const QString path = noteAbsolutePath();
    if (path.isEmpty())
        return fail(QStringLiteral("No note selected."));
    if (!m_readOnlyReason.isEmpty())
        return fail(QStringLiteral("This note is read-only here. Resolve it in Apple Notes."));
    if (m_syncRunning)
        return fail(QStringLiteral("Wait for the sync to finish, then try again."));
    if (!noteConflictsUnreadable())
        return fail(QStringLiteral("This note has no unreadable conflict markers."));
    // Only the version on screen is replaced: a newer one is shown first.
    if (readText(path) != m_noteContent) {
        refresh();
        return fail(QStringLiteral("The note changed on disk. Look it over, then try again."));
    }
    const SyncModel::EnvelopeSplit split = SyncModel::splitEnvelope(m_noteContent);
    QString body;
    if (how == u"strip") {
        body = SyncModel::stripConflictMarkers(split.body);
    } else if (how == u"synced") {
        if (!noteHasSyncedCopy())
            return fail(QStringLiteral("There is no last synced version of this note."));
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
        return QStringLiteral("This note is read-only here. Resolve it in Apple Notes.");
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
    const QString clean = sanitized(name);
    if (clean.isEmpty())
        return;
    QDir().mkpath(QDir(folderAbsolutePath(m_currentFolder)).filePath(clean));
    rebuildFolders();
    rewatch();
}

QString NotesBackend::renameCurrentFolder(const QString &name)
{
    const QString clean = sanitized(name);
    if (m_currentFolder.isEmpty())
        return QStringLiteral("All Notes cannot be renamed.");
    if (clean.isEmpty())
        return QStringLiteral("Name is empty.");
    const QString parent = m_currentFolder.section(u'/', 0, -2);
    const QString target = parent.isEmpty() ? clean : parent + u'/' + clean;
    if (target == m_currentFolder)
        return {};
    if (QDir(folderAbsolutePath(target)).exists())
        return QStringLiteral("A folder with that name already exists.");
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

QVariantList NotesBackend::searchVault(const QString &query)
{
    QVariantList out;
    const QString q = query.trimmed();
    if (q.size() < 2)
        return out;
    const QString mode = vaultTitleMode();
    const QDir root(rootPath());
    QDirIterator it(rootPath(), { QStringLiteral("*.md") }, QDir::Files, QDirIterator::Subdirectories);
    for (int scanned = 0; it.hasNext() && out.size() < 100 && scanned < 2000; ++scanned) {
        const QString rel = root.relativeFilePath(it.next());
        if (isHidden(rel))
            continue;
        const QString text = readText(it.filePath(), 1024 * 1024);
        if (!text.contains(q, Qt::CaseInsensitive))
            continue;
        QString snippet;
        for (const QString &line : text.split(u'\n')) {
            if (line.contains(q, Qt::CaseInsensitive))
                snippet = SyncModel::stripMarkdownLead(line).left(120);
            if (!snippet.isEmpty())
                break;
        }
        const QString folder = rel.section(u'/', 0, -2);
        const QString name = rel.section(u'/', -1);
        const SyncModel::NotePreview preview = SyncModel::previewNote(text, name.chopped(3), mode);
        out << QVariantMap{ { QStringLiteral("folder"), folder },
                            { QStringLiteral("file"), name },
                            { QStringLiteral("title"), preview.title },
                            { QStringLiteral("snippet"), snippet.isEmpty() ? preview.snippet : snippet } };
    }
    return out;
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
        return QStringLiteral("Already exists (not overwritten): %1").arg(name);
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
                  { QStringLiteral("clone"), QStringLiteral("--account"), m_dsid,
                    QStringLiteral("--non-interactive"), rootPath() },
                  QStringLiteral("Clone"));
        return;
    }
    if (!m_cloneSignInAsked) {
        m_cloneSignInAsked = true;
        signIn();
    }
}

void NotesBackend::runPull()
{
    const QStringList json = m_toolJson ? QStringList{ QStringLiteral("--json") } : QStringList();
    startSync(Mode::Plain, json + QStringList{ QStringLiteral("pull") }, QStringLiteral("Pull"));
}

void NotesBackend::runPush()
{
    const QStringList json = m_toolJson ? QStringList{ QStringLiteral("--json") } : QStringList();
    startSync(Mode::Plain, json + QStringList{ QStringLiteral("push") }, QStringLiteral("Push"));
}

void NotesBackend::runSync()
{
    if (m_syncRunning)
        return;
    m_pullAfterPush = true;
    runPush();
}

void NotesBackend::refreshPushPreview()
{
    startSync(Mode::Preview, { QStringLiteral("--json"), QStringLiteral("status") },
              QStringLiteral("Push preview"));
}

void NotesBackend::runHistory()
{
    if (m_currentNote.isEmpty())
        return;
    startSync(Mode::History,
              { QStringLiteral("--json"), QStringLiteral("history"), vaultRelative(m_currentNote) },
              QStringLiteral("History"));
}

void NotesBackend::runDiff(const QString &ref)
{
    if (m_currentNote.isEmpty() || ref.trimmed().isEmpty())
        return;
    startSync(Mode::Diff, { QStringLiteral("diff"), vaultRelative(m_currentNote), ref.trimmed() },
              QStringLiteral("Diff"));
}

void NotesBackend::startSync(Mode mode, const QStringList &args, const QString &label)
{
    if (m_syncRunning)
        return;
    m_mode = mode;
    m_syncLabel = label;
    m_captured.clear();
    m_capturedErr.clear();
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
    m_lastExit = exitCode;
    m_syncRunning = false;
    emit syncRunningChanged();
    // Exit 3 is an answer, not a failure: status has entries to push, or
    // diff found differences.
    const bool ok = exitCode == 0 || (exitCode == 3 && (m_mode == Mode::Preview || m_mode == Mode::Diff));
    appendLog(QStringLiteral("(exit %1)").arg(exitCode));

    QVariantMap parsed;
    if (ok && m_mode == Mode::Preview)
        parsed = SyncModel::parseStatusJson(m_captured);
    else if (ok && m_mode == Mode::History)
        parsed = SyncModel::parseHistoryJson(m_captured);
    const QString error = ok ? parsed.value(QStringLiteral("error")).toString()
                             : QStringLiteral("%1 failed (exit %2). See log.").arg(m_syncLabel).arg(exitCode);

    // Every failure that only a sign-in fixes exits 2, the code every iCloud
    // tool uses (the app's own arguments never make a usage error). Notes
    // never signs in by itself: icloud-session owns the sign-in, so it is
    // told (it confirms with Apple before signing everyone out) and syncing
    // pauses until it reports a sign-in, since every further sync would
    // fail the same way.
    const bool sessionExpired = !ok && exitCode == 2;
    if (sessionExpired)
        callSession(QStringLiteral("ReportSignInRequired"));
    // A pull or clone always talks to iCloud, so one that worked proves the
    // session; a push with nothing to send never checks it.
    // Only that re-arms the retry: a push that sent nothing proves nothing,
    // and clearing it there let push-ok, pull-refused retry forever.
    const bool sessionWorks = ok && (m_syncLabel == u"Pull" || m_syncLabel == u"Clone");
    if (sessionWorks)
        m_retriedAfterReport = false;
    setAuthExpired(sessionExpired || (m_authExpired && !sessionWorks));
    emit syncFinished(m_syncLabel, ok);

    switch (m_mode) {
    case Mode::Plain:
        setPushPreview({}, {}); // a pull or push makes the last preview stale
        refresh(); // a pull or clone changes files behind our back
        if (m_syncLabel == u"Clone")
            emit cloneFinished(ok);
        if (m_pullAfterPush) {
            m_pullAfterPush = false;
            if (!m_authExpired) {
                setSyncMessage(m_syncLabel + (ok ? QStringLiteral(" done.") : QStringLiteral(" failed. See log.")));
                runPull(); // the second half of runSync, whatever the push did
                return;
            }
        }
        break;
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
        m_diffText = ok ? QString::fromUtf8(m_captured) : QString();
        m_historyError = error;
        emit historyChanged();
        emit historyReady(error.isEmpty());
        break;
    }
    // The sign-in banner already says what went wrong and how to fix it.
    setSyncMessage(sessionExpired ? kPausedMessage
                   : m_syncLabel + (error.isEmpty() ? QStringLiteral(" done.") : QStringLiteral(" failed. See log.")));
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

// A save refused while the sync ran, now that it is done and its changes
// are loaded: a note the sync left alone is saved as asked; one it changed
// gets the edits merged in, as any change on disk under edits does. (The
// window may already have merged them on the refresh, which clears this.)
void NotesBackend::writeQueuedSave()
{
    const QueuedSave queued = std::exchange(m_queuedSave, {});
    if (!queued.valid || m_currentNote.isEmpty()) // a note gone keeps its edits through keepEditsAsConflict
        return;
    const QString base = SyncModel::editorForm(SyncModel::splitEnvelope(queued.base).body);
    const bool changed = m_noteContent != queued.base;
    bool written = changed && keepEditsAsConflict(base, queued.body);
    // Saving over the note is only for one the sync left as the edits
    // started, or that already holds them: never over a change a merge
    // could not take (a failed write), which would drop it.
    const QString theirs = SyncModel::editorForm(noteBody());
    if (!written && (!changed || theirs == base || theirs == queued.body))
        written = saveCurrentNote(queued.body);
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

NotesBackend::~NotesBackend() = default;

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
        if (signedInAnew)
            m_retriedAfterReport = false;
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
