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
#include <QPrinter>
#include <QRegularExpression>
#include <QStandardPaths>
#include <QTextDocument>
#include <QCryptographicHash>
#include <QTextStream>
#include <QUrl>
#include <algorithm>

namespace {

// icloud-session's D-Bus contract: the daemon owns the Apple account for
// every iCloud app, and announces changes with PropertiesChanged.
const QString kSessionService = QStringLiteral("io.github.ferdousbhai.ICloudSession");
const QString kSessionPath = QStringLiteral("/io/github/ferdousbhai/ICloudSession");
const QString kPropertiesInterface = QStringLiteral("org.freedesktop.DBus.Properties");
// Long enough for D-Bus activation to start the daemon; never blocks the UI.
constexpr int kSessionTimeoutMs = 10000;

const QString kPausedMessage = QStringLiteral("Sync paused. Sign in to iCloud to resume.");

// How long the app waits for a background sync to let go of the vault
// before syncing anyway. The timer's unit stops a run after 3 minutes.
constexpr qint64 kLockWaitMs = 4 * 60 * 1000;

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

// Dot-directories (icloud-md bookkeeping, .git) hold no notes.
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

// The files a note links to under attachments/. icloud-md keeps one
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
        // never runs icloud-md beside it. While one holds it, the window
        // opens anyway and its syncs wait (see startSync).
        connect(&m_lockRetry, &QTimer::timeout, this, &NotesBackend::retryLock);
        m_lockRetry.setInterval(500);
        switch (m_lock.tryLock()) {
        case VaultLock::Locked:
            break;
        case VaultLock::Busy:
            appendLog(QStringLiteral("A background sync is running; syncing here waits for it."));
            m_lockWait.start();
            m_lockRetry.start();
            break;
        case VaultLock::Failed:
            appendLog(QStringLiteral("Could not open the sync lock %1; syncing without it.").arg(m_lock.path()));
            break;
        }
    }

    // External changes (an icloud-md pull in a terminal, say) re-list;
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

    m_syncProcess.setProgram(QStringLiteral("icloud-md"));
    m_syncProcess.setProcessChannelMode(QProcess::MergedChannels);
    connect(&m_syncProcess, &QProcess::readyReadStandardOutput, this, [this] {
        const QByteArray out = m_syncProcess.readAllStandardOutput();
        m_captured += out;
        appendLog(QString::fromUtf8(out));
        // icloud-md spends up to 90 s quietly trying to renew an expired
        // session before it gives up; say so instead of a bare "Push…".
        if (out.contains("attempting silent re-authentication"))
            setSyncMessage(QStringLiteral("iCloud sign-in expired. Trying to renew it in the background (up to 90 s)…"));
    });
    connect(&m_syncProcess, &QProcess::finished, this, [this](int exitCode) { finishSync(exitCode); });
    connect(&m_syncProcess, &QProcess::errorOccurred, this, [this](QProcess::ProcessError error) {
        if (error != QProcess::FailedToStart)
            return; // finished() follows for every other error
        appendLog(QStringLiteral("Failed to start icloud-md: ") + m_syncProcess.errorString());
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
    refreshSignIn();
    refresh();
    setSyncMessage(!icloudMdAvailable() ? QStringLiteral("icloud-md not found on PATH. Install it to sync.")
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
// it, since icloud-md clones only into an empty directory. A vault named
// by ICLOUD_NOTES_VAULT gets a lock of its own, so tests never share the
// real vault's.
QString NotesBackend::lockPath()
{
    const QString runtime = qEnvironmentVariable("XDG_RUNTIME_DIR");
    const QString dir = runtime.isEmpty() ? QFileInfo(rootPath()).absolutePath() : runtime;
    const QString name = runtime.isEmpty() ? QStringLiteral(".icloud-notes") : QStringLiteral("icloud-notes");
    if (qEnvironmentVariableIsEmpty("ICLOUD_NOTES_VAULT"))
        return dir + u'/' + name + QStringLiteral(".lock");
    const QByteArray hash = QCryptographicHash::hash(QFileInfo(rootPath()).absoluteFilePath().toUtf8(),
                                                     QCryptographicHash::Sha1).toHex().left(12);
    return dir + u'/' + name + u'-' + QString::fromLatin1(hash) + QStringLiteral(".lock");
}

// A background sync let go of the vault (or waiting has gone on too long):
// show what it pulled, then run the sync that waited for it.
void NotesBackend::retryLock()
{
    const bool gaveUp = m_lock.tryLock() != VaultLock::Locked && m_lockWait.elapsed() > kLockWaitMs;
    if (!m_lock.held() && !gaveUp)
        return;
    m_lockRetry.stop();
    if (gaveUp)
        appendLog(QStringLiteral("The background sync is still running after %1 minutes; syncing anyway.")
                      .arg(kLockWaitMs / 60000));
    refresh();
    if (m_syncRunning)
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

// icloud-md's state directory: the current name, or the one it used to use.
QString NotesBackend::stateDir()
{
    for (const char *name : { ".icloud-md", ".icloud-notes-sync" }) {
        const QString dir = rootPath() + u'/' + QLatin1StringView(name);
        if (QDir(dir).exists())
            return dir;
    }
    return {};
}

QByteArray NotesBackend::stateJson() const
{
    QFile file(stateDir() + QStringLiteral("/state.json"));
    return file.open(QIODevice::ReadOnly) ? file.readAll() : QByteArray();
}

bool NotesBackend::icloudMdAvailable() const
{
    return !QStandardPaths::findExecutable(QStringLiteral("icloud-md")).isEmpty();
}

QString NotesBackend::vaultTitleMode() const
{
    return SyncModel::readTitleMode(stateJson());
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
    SyncModel::sortFolders(folders, SyncModel::defaultFolderDir(stateJson()));

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
    if (!m_folders.contains(m_currentFolder))
        setCurrentFolder({});
    emit foldersChanged(); // also refreshes cloned/vaultTitleMode after a clone
}

void NotesBackend::rebuildNotes()
{
    QFileInfoList entries =
        QDir(folderAbsolutePath(m_currentFolder)).entryInfoList({ QStringLiteral("*.md") }, QDir::Files);
    // icloud-md syncs note mtimes, so newest-first matches Notes.app ordering.
    std::sort(entries.begin(), entries.end(),
              [](const QFileInfo &a, const QFileInfo &b) { return a.lastModified() > b.lastModified(); });
    QStringList found;
    for (const QFileInfo &info : entries)
        found << info.fileName();

    const QStringList oldNotes = m_notes;
    const QVariantMap oldStates = m_noteStates;
    const QVariantMap oldDetails = m_noteDetails;
    m_notes = found;
    if (!m_currentNote.isEmpty() && !m_notes.contains(m_currentNote))
        closeNote();
    classifyNotes();
    // Only a real change re-renders the list (and drops its scroll position).
    if (m_notes != oldNotes || m_noteStates != oldStates || m_noteDetails != oldDetails)
        emit notesChanged();
}

// Per-note list details plus the guardrail flags the badges show: notes
// icloud-md does not track ("new"), tracked notes that lost their id,
// untracked notes carrying an id from elsewhere, conflicts and tables.
void NotesBackend::classifyNotes()
{
    const QByteArray state = stateJson();
    const QSet<QString> tracked = SyncModel::trackedFiles(state);
    const QHash<QString, QString> readOnly = SyncModel::readOnlyReasons(state);
    const QString mode = SyncModel::readTitleMode(state);
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
                                          { QStringLiteral("modifiedMs"), scan.modifiedMs } });
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
                                      : SyncModel::readOnlyReasons(stateJson()).value(vaultRelative(m_currentNote));
    emit noteContentChanged();
}

void NotesBackend::closeNote()
{
    m_currentNote.clear();
    emit currentNoteChanged();
    loadCurrentNote();
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
    const QString text = assembleNote(SyncModel::restoreEditorChars(noteBody(), body));
    if (path.isEmpty() || !m_readOnlyReason.isEmpty() || text == m_noteContent)
        return true;
    // A pull (or another program) rewrote the note since it was loaded:
    // writing now would silently drop that change. The reload that follows
    // hands the edits to keepEditsAsConflict instead.
    if (QFile::exists(path) && readText(path) != m_noteContent) {
        emit currentNoteChangedOnDisk();
        return false;
    }
    if (!writeText(path, text))
        return false;
    loadCurrentNote();
    rebuildNotes(); // a save bumps mtime, which reorders the list
    emit vaultChanged();
    return true;
}

bool NotesBackend::keepEditsAsConflict(const QString &base, const QString &mine)
{
    const QString path = noteAbsolutePath();
    if (path.isEmpty() || !m_readOnlyReason.isEmpty() || !QFile::exists(path))
        return false;
    const QString disk = readText(path);
    const SyncModel::EnvelopeSplit split = SyncModel::splitEnvelope(disk);
    const QString theirs = SyncModel::editorForm(split.body);
    if (theirs == base || theirs == mine)
        return false;
    // Edits to different lines merge on their own; only lines both sides
    // changed differently become blocks to pick from.
    const QString merged = SyncModel::conflictBody(split.body, base, mine);
    if (!writeText(path, split.envelope + merged))
        return false;
    loadCurrentNote();
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
// its sign-in window. icloud-md clones the daemon's account by dsid from the
// session the daemon mirrors for it, and never opens a window itself.
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
        // icloud-md defaults to; a vault cloned with --filename-as-title from
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
    startSync(Mode::Plain, { QStringLiteral("pull") }, QStringLiteral("Pull"));
}

void NotesBackend::runPush()
{
    startSync(Mode::Plain, { QStringLiteral("push") }, QStringLiteral("Push"));
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
    m_syncRunning = true;
    emit syncRunningChanged();
    appendLog(QStringLiteral("$ icloud-md ") + args.join(u' '));
    setSyncMessage(label + QStringLiteral("…"));
    if (!icloudMdAvailable()) {
        appendLog(QStringLiteral("icloud-md not found on PATH. Install it to sync."));
        finishSync(-1);
        return;
    }
    m_syncProcess.setWorkingDirectory(rootPath());
    m_syncProcess.setArguments(args);
    if (m_lockRetry.isActive()) { // a background sync has the vault; retryLock starts this
        setSyncMessage(QStringLiteral("Waiting for a background sync to finish…"));
        return;
    }
    m_syncProcess.start();
}

void NotesBackend::finishSync(int exitCode)
{
    m_syncRunning = false;
    emit syncRunningChanged();
    const bool ok = exitCode == 0;
    appendLog(QStringLiteral("(exit %1)").arg(exitCode));

    QVariantMap parsed;
    if (ok && m_mode == Mode::Preview)
        parsed = SyncModel::parseStatusJson(m_captured);
    else if (ok && m_mode == Mode::History)
        parsed = SyncModel::parseHistoryJson(m_captured);
    const QString error = ok ? parsed.value(QStringLiteral("error")).toString()
                             : QStringLiteral("%1 failed (exit %2). See log.").arg(m_syncLabel).arg(exitCode);

    // Every icloud-md failure that only a sign-in fixes (expired session,
    // missing session file) hints at reauthenticate. Notes never runs that:
    // icloud-session owns the sign-in, so it is told (it confirms with Apple
    // before signing everyone out) and syncing pauses until it reports a
    // sign-in, since every further sync would fail the same way.
    const bool sessionExpired = !ok && m_captured.contains("icloud-md reauthenticate");
    if (sessionExpired)
        callSession(QStringLiteral("ReportSignInRequired"));
    // A pull or clone always talks to iCloud, so one that worked proves the
    // session; a push with nothing to send never checks it.
    const bool sessionWorks = ok && (m_syncLabel == u"Pull" || m_syncLabel == u"Clone");
    if (ok)
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
// refreshed icloud-md's copy, so icloud-md's refusal came from a stale copy
// and one retry should go through. A failure (no daemon) is logged, and
// SignIn's named.
void NotesBackend::callSession(const QString &method)
{
    QDBusConnection bus = QDBusConnection::sessionBus();
    if (!bus.isConnected()) {
        appendLog(QStringLiteral("icloud-session: no D-Bus session bus"));
        return;
    }
    ++m_sessionCalls;
    auto *watcher = new QDBusPendingCallWatcher(bus.asyncCall(sessionCall(method), kSessionTimeoutMs), this);
    connect(watcher, &QDBusPendingCallWatcher::finished, this, [this, method](QDBusPendingCallWatcher *call) {
        call->deleteLater();
        --m_sessionCalls;
        if (!call->isError()) {
            const QDBusPendingReply<bool> reply = *call;
            if (method == u"ReportSignInRequired" && reply.argumentAt<0>() && !m_retriedAfterReport && !m_syncRunning) {
                m_retriedAfterReport = true;
                setAuthExpired(false);
                runSync();
            }
            return;
        }
        appendLog(QStringLiteral("icloud-session %1 failed: %2").arg(method, call->error().message()));
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
// turning true, or a fresh expiry while signed in) resumes paused syncing
// and a waiting clone; signed out pauses syncing without trying first.
void NotesBackend::applySignIn(const QVariantMap &properties)
{
    const bool wasSignedIn = m_signInKnown && m_signedIn;
    const bool wasSigningIn = m_signingIn;
    const quint64 expiresBefore = m_expiresAt;
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
        if (m_authExpired && (!wasSignedIn || m_expiresAt != expiresBefore)) {
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
