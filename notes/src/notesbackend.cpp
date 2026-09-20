#include "notesbackend.h"
#include "markdownhighlighter.h"
#include "syncmodel.h"

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

NotesBackend::NotesBackend(QObject *parent)
    : QObject(parent), m_uiScale(readUiScale()), m_iconFont(findIconFont())
{
    QDir().mkpath(rootPath());

    // A theme change rewrites colors.toml (or the directory holding it);
    // re-read and re-arm the watch, since a replaced file drops out of it.
    loadTheme();
    connect(&m_themeWatcher, &QFileSystemWatcher::fileChanged, this, &NotesBackend::loadTheme);
    connect(&m_themeWatcher, &QFileSystemWatcher::directoryChanged, this, &NotesBackend::loadTheme);

    // External changes (an icloud-md pull in a terminal, say) re-list;
    // a change to the open note is reported so unsaved edits are kept.
    connect(&m_watcher, &QFileSystemWatcher::directoryChanged, this, [this] {
        rebuildFolders();
        rebuildNotes();
    });
    connect(&m_watcher, &QFileSystemWatcher::fileChanged, this, [this](const QString &path) {
        // Our own saves come back through here too; only a real difference counts.
        if (!m_currentNote.isEmpty() && path == noteAbsolutePath() && readText(path) != m_noteContent)
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
    const QString body = SyncModel::splitEnvelope(m_noteContent).body;
    return vaultTitleMode() == u"filename" ? body : SyncModel::splitTitle(body).rest;
}

// The file as it is written for an editor body: stored envelope and heading
// line first, untouched, so neither can be edited away.
QString NotesBackend::assembleNote(const QString &body) const
{
    const SyncModel::EnvelopeSplit split = SyncModel::splitEnvelope(m_noteContent);
    const QString titleLine =
        vaultTitleMode() == u"filename" ? QString() : SyncModel::splitTitle(split.body).titleLine;
    return split.envelope + titleLine + body;
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
    const QString path = noteAbsolutePath();
    const QString text = assembleNote(body);
    if (path.isEmpty() || text == m_noteContent || !writeText(path, text))
        return;
    loadCurrentNote();
    rebuildNotes(); // a save bumps mtime, which reorders the list
}

QString NotesBackend::saveWarning(const QString &body)
{
    // Checks run against the full file as it would be written.
    const QString text = assembleNote(body);
    QStringList warnings;
    if (SyncModel::hasConflictMarkers(text) && !SyncModel::hasConflictMarkers(m_noteContent))
        warnings << QStringLiteral("Unresolved conflict markers present — push will refuse this note "
                                   "until they are resolved.");
    const QString id = SyncModel::extractNoteId(text);
    const QDir dir(folderAbsolutePath(m_currentFolder));
    for (const QString &name : m_notes) {
        if (id.isEmpty() || name == m_currentNote || m_scans.value(dir.filePath(name)).id != id)
            continue;
        warnings << QStringLiteral("Another note in this folder (%1) carries the same apple-note-id. "
                                   "Pushing two files with one id is ambiguous — keep only one.")
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
    // Rendered like the editor shows it: the title as a heading, then the body.
    const QString title = m_noteDetails.value(m_currentNote).toMap().value(QStringLiteral("title")).toString();
    QTextDocument doc;
    doc.setMarkdown(u"# " + title + u"\n\n" + noteBody());
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
    // Clone targets a fresh directory; the root doubles as that directory.
    // Titles stay the first line of each note, as in Notes.app and as
    // icloud-md defaults to; a vault cloned with --filename-as-title from
    // the CLI is still read correctly (see vaultTitleMode).
    startSync(Mode::Plain, { QStringLiteral("clone"), rootPath() }, QStringLiteral("Clone"));
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
