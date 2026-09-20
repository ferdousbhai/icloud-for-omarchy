#include "notesbackend.h"
#include "syncmodel.h"

#include <QDir>
#include <QDirIterator>
#include <QFile>
#include <QFileInfo>
#include <QPrinter>
#include <QRegularExpression>
#include <QStandardPaths>
#include <QTextDocument>
#include <QTextStream>
#include <QUrl>
#include <algorithm>

namespace {

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

} // namespace

NotesBackend::NotesBackend(QObject *parent)
    : QObject(parent), m_uiScale(readUiScale())
{
    QDir().mkpath(rootPath());

    // External changes (an icloud-md pull in a terminal, say) re-list;
    // a change to the open note is reported so unsaved edits are kept.
    connect(&m_watcher, &QFileSystemWatcher::directoryChanged, this, [this] {
        rebuildFolders();
        rebuildNotes();
    });
    connect(&m_watcher, &QFileSystemWatcher::fileChanged, this, [this](const QString &path) {
        if (!m_currentNote.isEmpty() && path == noteAbsolutePath())
            emit currentNoteChangedOnDisk();
    });

    m_syncProcess.setProgram(QStringLiteral("icloud-md"));
    m_syncProcess.setProcessChannelMode(QProcess::MergedChannels);
    connect(&m_syncProcess, &QProcess::readyReadStandardOutput, this, [this] {
        const QByteArray out = m_syncProcess.readAllStandardOutput();
        m_captured += out;
        appendLog(QString::fromUtf8(out));
    });
    connect(&m_syncProcess, &QProcess::finished, this, [this](int exitCode) { finishSync(exitCode); });
    connect(&m_syncProcess, &QProcess::errorOccurred, this, [this](QProcess::ProcessError error) {
        if (error != QProcess::FailedToStart)
            return; // finished() follows for every other error
        appendLog(QStringLiteral("Failed to start icloud-md: ") + m_syncProcess.errorString());
        finishSync(-1);
    });

    refresh();
    setSyncMessage(!icloudMdAvailable() ? QStringLiteral("icloud-md not found on PATH — install it to sync.")
                   : cloned()           ? QStringLiteral("Ready.")
                                        : QStringLiteral("Not linked to iCloud yet — press Clone."));
}

QString NotesBackend::rootPath()
{
    return QStandardPaths::writableLocation(QStandardPaths::DocumentsLocation)
        + QStringLiteral("/icloud-notes");
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
QString NotesBackend::stateDir() const
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
    folders.sort(Qt::CaseInsensitive);

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
    const QString mode = SyncModel::readTitleMode(state);
    const QDir dir(folderAbsolutePath(m_currentFolder));
    QVariantMap states;
    QVariantMap details;
    for (const QString &name : m_notes) {
        const QString path = dir.filePath(name);
        const QString text = readText(path, kScanLimit + 1);
        const bool huge = text.size() > kScanLimit;
        const SyncModel::NotePreview preview = SyncModel::previewNote(text, name.chopped(3), mode);
        details.insert(name, QVariantMap{ { QStringLiteral("title"), preview.title },
                                          { QStringLiteral("snippet"), preview.snippet },
                                          { QStringLiteral("modifiedMs"),
                                            QFileInfo(path).lastModified().toMSecsSinceEpoch() } });
        QStringList flags;
        if (!huge && SyncModel::hasConflictMarkers(text))
            flags << QStringLiteral("conflict");
        if (!huge && SyncModel::hasTable(text))
            flags << QStringLiteral("tables");
        const bool hasId = !SyncModel::extractNoteId(text).isEmpty();
        if (!tracked.contains(vaultRelative(name)))
            flags << (hasId ? QStringLiteral("foreign-id") : QStringLiteral("new"));
        else if (!hasId)
            flags << QStringLiteral("missing-id");
        if (!flags.isEmpty())
            states.insert(name, flags);
    }
    m_noteStates = states;
    m_noteDetails = details;
}

void NotesBackend::loadCurrentNote()
{
    const QString path = noteAbsolutePath();
    m_noteContent = path.isEmpty() ? QString() : readText(path);
    m_noteAttachments = path.isEmpty() ? QVariantList() : attachmentsFor(path, m_noteContent);
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

void NotesBackend::saveCurrentNote(const QString &body)
{
    // The editor holds the body only; the stored envelope is reattached
    // untouched, so sync metadata can never be edited away.
    const QString path = noteAbsolutePath();
    const QString text = SyncModel::splitEnvelope(m_noteContent).envelope + body;
    if (path.isEmpty() || text == m_noteContent || !writeText(path, text))
        return;
    loadCurrentNote();
    rebuildNotes(); // a save bumps mtime, which reorders the list
}

QString NotesBackend::saveWarning(const QString &body)
{
    // Checks run against the full file as it would be written.
    const QString text = SyncModel::splitEnvelope(m_noteContent).envelope + body;
    QStringList warnings;
    if (SyncModel::hasConflictMarkers(text) && !SyncModel::hasConflictMarkers(m_noteContent))
        warnings << QStringLiteral("Unresolved conflict markers present — push will refuse this note "
                                   "until they are resolved.");
    const QString id = SyncModel::extractNoteId(text);
    const QDir dir(folderAbsolutePath(m_currentFolder));
    for (const QString &name : m_notes) {
        if (id.isEmpty() || name == m_currentNote)
            continue;
        if (SyncModel::extractNoteId(readText(dir.filePath(name), 8192)) == id) {
            warnings << QStringLiteral("Another note in this folder (%1) carries the same apple-note-id. "
                                       "Pushing two files with one id is ambiguous — keep only one.")
                            .arg(name);
            break;
        }
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

void NotesBackend::deleteCurrentNote()
{
    const QString path = noteAbsolutePath();
    if (path.isEmpty())
        return;
    QFile::moveToTrash(path); // the next push moves the note to Recently Deleted
    closeNote();
    rebuildNotes();
    rewatch();
}

QString NotesBackend::renameCurrentNote(const QString &title)
{
    const QString clean = sanitized(title);
    if (m_currentNote.isEmpty())
        return QStringLiteral("No note selected.");
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

void NotesBackend::newFolder(const QString &name)
{
    const QString clean = sanitized(name);
    if (clean.isEmpty())
        return;
    QDir().mkpath(QDir(folderAbsolutePath(m_currentFolder)).filePath(clean));
    rebuildFolders();
    rewatch();
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
    QTextDocument doc;
    doc.setPlainText(noteBody());
    QPrinter printer;
    printer.setOutputFormat(QPrinter::PdfFormat);
    printer.setOutputFileName(pdf);
    doc.print(&printer);
    if (!QFile::exists(pdf))
        return QStringLiteral("Could not write the PDF.");
    setSyncMessage(QStringLiteral("Exported %1 next to the note.").arg(name));
    return {};
}

void NotesBackend::runClone(const QString &titleMode)
{
    // Clone targets a fresh directory; the root doubles as that directory.
    // The title shape is chosen once, here, and can never change after.
    QStringList args{ QStringLiteral("clone"), rootPath() };
    if (titleMode == u"filename")
        args << QStringLiteral("--filename-as-title");
    startSync(Mode::Plain, args, QStringLiteral("Clone"));
}

void NotesBackend::runPull()
{
    startSync(Mode::Plain, { QStringLiteral("pull") }, QStringLiteral("Pull"));
}

void NotesBackend::runPush()
{
    startSync(Mode::Plain, { QStringLiteral("push") }, QStringLiteral("Push"));
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
        appendLog(QStringLiteral("icloud-md not found on PATH — install it to sync."));
        finishSync(-1);
        return;
    }
    m_syncProcess.setWorkingDirectory(rootPath());
    m_syncProcess.setArguments(args);
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
                             : QStringLiteral("%1 failed (exit %2) — see log.").arg(m_syncLabel).arg(exitCode);

    switch (m_mode) {
    case Mode::Plain:
        setPushPreview({}, {}); // a pull or push makes the last preview stale
        refresh(); // a pull or clone changes files behind our back
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
    setSyncMessage(m_syncLabel + (error.isEmpty() ? QStringLiteral(" done.") : QStringLiteral(" failed — see log.")));
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

void NotesBackend::setSyncMessage(const QString &text)
{
    if (m_syncMessage == text)
        return;
    m_syncMessage = text;
    emit syncMessageChanged();
}
