// Backend tests: note classification, save warnings, mode-aware rename and
// the icloud-notes-sync CLI seam — against a scratch vault under a temporary
// directory, never the real one. Run with bin/test.
#include "../src/backgroundsync.h"
#include "../src/notesbackend.h"
#include "../src/singleinstance.h"
#include "../src/vaultlock.h"
#include "check.h"
#include "fake_session.h"

#include <QDateTime>
#include <QDir>
#include <QElapsedTimer>
#include <QEventLoop>
#include <QFile>
#include <QGuiApplication>
#include <QStandardPaths>
#include <QTemporaryDir>
#include <QThread>
#include <QTimer>
#include <functional>

namespace {
QString rootPath()
{
    return qEnvironmentVariable("ICLOUD_NOTES_VAULT");
}

void writeFile(const QString &rel, const QString &content)
{
    const QString path = rootPath() + QLatin1Char('/') + rel;
    QDir().mkpath(QFileInfo(path).absolutePath());
    QFile f(path);
    if (f.open(QIODevice::WriteOnly | QIODevice::Text | QIODevice::Truncate))
        f.write(content.toUtf8());
}

QString readFile(const QString &rel)
{
    QFile f(rootPath() + QLatin1Char('/') + rel);
    return f.open(QIODevice::ReadOnly | QIODevice::Text) ? QString::fromUtf8(f.readAll()) : QString();
}

bool hasFlag(const NotesBackend &b, const QString &note, const char *flag)
{
    return b.noteStates().value(note).toStringList().contains(QString::fromLatin1(flag));
}

// Trash entries the test created (freedesktop layout) so runs leave nothing behind.
void emptyTestTrash()
{
    const QDir info(QStandardPaths::writableLocation(QStandardPaths::GenericDataLocation)
                    + QStringLiteral("/Trash/info"));
    for (const QFileInfo &entry : info.entryInfoList({ QStringLiteral("*.trashinfo") }, QDir::Files)) {
        QFile f(entry.absoluteFilePath());
        if (!f.open(QIODevice::ReadOnly))
            continue;
        const QString body = QString::fromUtf8(f.readAll());
        if (!body.contains(rootPath()))
            continue;
        f.close();
        QDir(info.absolutePath() + QStringLiteral("/../files/") + entry.completeBaseName()).removeRecursively();
        QFile::remove(info.absolutePath() + QStringLiteral("/../files/") + entry.completeBaseName());
        QFile::remove(entry.absoluteFilePath());
    }
}

// Spin until the running sync finishes (or a timeout gives up).
void waitForSync(const NotesBackend &b)
{
    QEventLoop loop;
    QObject::connect(&b, &NotesBackend::syncRunningChanged, &loop, [&] {
        if (!b.syncRunning())
            loop.quit();
    });
    QTimer::singleShot(15000, &loop, &QEventLoop::quit);
    if (b.syncRunning())
        loop.exec();
}

// Spin the event loop until cond holds (or a timeout gives up): D-Bus
// replies and signals arrive through it.
bool waitUntil(const std::function<bool()> &cond, int timeoutMs = 5000)
{
    QElapsedTimer timer;
    timer.start();
    while (!cond() && timer.elapsed() < timeoutMs) {
        QCoreApplication::processEvents(QEventLoop::AllEvents, 20);
        QThread::msleep(5);
    }
    return cond();
}

// Spin until every read of icloud-session's properties has answered.
void waitForSignIn(const NotesBackend &b)
{
    waitUntil([&] { return !b.signInPending(); });
}

// Spin until the backend has run whatever syncs it chained.
void waitForIdle(const NotesBackend &b)
{
    waitUntil([&] { return !b.syncRunning(); }, 15000);
    QCoreApplication::processEvents();
}

QVariant seconds(qint64 fromNow)
{
    return QVariant::fromValue<qulonglong>(qulonglong(QDateTime::currentSecsSinceEpoch() + fromNow));
}
} // namespace

int main(int argc, char *argv[])
{
    // GUI application: PDF export lays out text and needs the font database.
    // bin/test forces the offscreen platform so this stays headless.
    QGuiApplication app(argc, argv);
    // The fake icloud-session claims the real daemon's bus name: only ever
    // on the private session bus bin/test starts, never the desktop's.
    if (qEnvironmentVariable("ICLOUD_NOTES_PRIVATE_BUS") != QStringLiteral("1")) {
        QTextStream(stderr) << "backend_test: run it through bin/test (a private D-Bus session bus)\n";
        return EXIT_FAILURE;
    }
    // The vault under test lives in a temporary directory that is deleted
    // with it; the backend reads the path from ICLOUD_NOTES_VAULT. It sits
    // under the cache dir rather than /tmp so it is on the home filesystem,
    // where moving to the trash works. (Qt's test mode is not used: it
    // redirects neither DocumentsLocation nor, usably, the trash.)
    QDir().mkpath(QStandardPaths::writableLocation(QStandardPaths::CacheLocation));
    QTemporaryDir scratch(QStandardPaths::writableLocation(QStandardPaths::CacheLocation)
                          + QStringLiteral("/icloud-notes-test-XXXXXX"));
    if (!scratch.isValid())
        return EXIT_FAILURE;
    qputenv("ICLOUD_NOTES_VAULT", (scratch.path() + QStringLiteral("/vault")).toUtf8());
    // The sync locks go to the runtime directory: the scratch one, so the
    // tests never meet the real app's (or leave lock files behind).
    qputenv("XDG_RUNTIME_DIR", scratch.path().toUtf8());

    writeFile(QStringLiteral(".icloud-md/state.json"),
              QStringLiteral(R"({"titleMode":"in-body","notes":{)"
                             R"("id-a":{"file":"A.md"},)"
                             R"("id-b":{"file":"B.md"},)"
                             R"("id-e":{"file":"E.md"},)"
                             R"("id-f":{"file":"F.md","unpublishableReason":"is too large"},)"
                             R"("id-g":{"file":"G.md"}}})"));
    writeFile(QStringLiteral("A.md"), QStringLiteral("---\napple-note-id: id-a\n---\n# Alpha\nbody\n"));
    writeFile(QStringLiteral("B.md"),
              QStringLiteral("---\napple-note-id: id-b\n---\n# Beta\n<<<<<<< local\nx\n=======\ny\n>>>>>>> remote\n"));
    writeFile(QStringLiteral("C.md"), QStringLiteral("# Fresh\nbrand new\n"));
    writeFile(QStringLiteral("D.md"), QStringLiteral("---\napple-note-id: foreign-9\n---\n# Copied\n"));
    writeFile(QStringLiteral("E.md"), QStringLiteral("# Lost its id\n"));
    writeFile(QStringLiteral("F.md"),
              QStringLiteral("---\napple-note-id: id-f\n---\n# Grid\n| a | b |\n| c | d |\n"));
    writeFile(QStringLiteral("G.md"),
              QStringLiteral("---\napple-note-id: id-g\n---\n# Pics\n![pic.png](attachments/pic.png)\n"
                             "[my%20notes.pdf](attachments/my%20notes.pdf)\n"));
    writeFile(QStringLiteral("attachments/pic.png"), QStringLiteral("fake-png-bytes"));
    writeFile(QStringLiteral("attachments/my notes.pdf"), QStringLiteral("fake-pdf-bytes"));
    writeFile(QStringLiteral("attachments/other-note.png"), QStringLiteral("belongs to a sibling note"));
    writeFile(QStringLiteral("Sub/H.md"), QStringLiteral("# Nested\n"));

    NotesBackend b;
    check(b.vaultTitleMode() == QStringLiteral("in-body"), "backend mode in-body");
    check(b.folders() == QStringList{ QString(), QStringLiteral("Sub") }, "backend folders listed");
    check(b.folderNoteCounts().value(QString()).toInt() == 8
              && b.folderNoteCounts().value(QStringLiteral("Sub")).toInt() == 1,
          "backend folder counts");
    check(!b.noteStates().contains(QStringLiteral("A.md")), "backend clean note unflagged");
    check(hasFlag(b, QStringLiteral("B.md"), "conflict"), "backend conflict flagged");
    check(hasFlag(b, QStringLiteral("C.md"), "new"), "backend new flagged");
    check(hasFlag(b, QStringLiteral("D.md"), "foreign-id"), "backend foreign id flagged");
    check(hasFlag(b, QStringLiteral("E.md"), "missing-id"), "backend missing id flagged");
    check(hasFlag(b, QStringLiteral("F.md"), "tables"), "backend tables flagged");
    check(hasFlag(b, QStringLiteral("F.md"), "read-only"), "backend read-only flagged");
    check(!hasFlag(b, QStringLiteral("A.md"), "read-only"), "backend editable note not read-only");

    // A note the sync tool will never push opens locked: saves and renames are
    // refused, so edits cannot pile up locally where they would never sync.
    b.openNote(QStringLiteral("F.md"));
    check(b.readOnlyReason() == QStringLiteral("is too large"), "backend read-only reason");
    {
        const QString before = readFile(QStringLiteral("F.md"));
        b.saveCurrentNote(QStringLiteral("edited\n"));
        check(readFile(QStringLiteral("F.md")) == before, "backend read-only save refused");
        check(!b.renameCurrentNote(QStringLiteral("Other")).isEmpty(), "backend read-only rename refused");
    }

    // Attachments: only the files this note links to, decoded from the
    // URL-encoded links the sync tool writes; the folder-wide attachments/
    // directory also holds sibling notes' files.
    b.openNote(QStringLiteral("G.md"));
    {
        const QVariantList at = b.noteAttachments();
        check(at.size() == 2, "backend attachments are the note's links only");
        check(at.at(0).toMap().value(QStringLiteral("image")).toBool()
                  && at.at(0).toMap().value(QStringLiteral("url")).toString().endsWith(
                         QStringLiteral("attachments/pic.png")),
              "backend image attachment listed");
        check(!at.at(1).toMap().value(QStringLiteral("image")).toBool()
                  && at.at(1).toMap().value(QStringLiteral("name")).toString() == QStringLiteral("my notes.pdf"),
              "backend file attachment decoded");
    }

    b.openNote(QStringLiteral("A.md"));
    check(b.noteAttachments().isEmpty(), "backend attachments follow the note");
    check(b.readOnlyReason().isEmpty(), "backend read-only reason follows the note");
    check(b.saveWarning(QStringLiteral("edited body\n")).isEmpty(), "backend clean save silent");
    // The envelope is hidden from the editor and reattached on save, so the
    // id cannot be edited away; files on disk keep it byte-for-byte.
    check(b.noteBody() == QStringLiteral("# Alpha\nbody\n"), "backend body hides envelope, keeps title line");
    b.saveCurrentNote(QStringLiteral("# Alpha\nchanged\n"));
    check(readFile(QStringLiteral("A.md")) == QStringLiteral("---\napple-note-id: id-a\n---\n# Alpha\nchanged\n"),
          "backend save preserves envelope");
    // Apple's no-break spaces and soft line breaks survive an edit elsewhere.
    b.saveCurrentNote(QStringLiteral("# Alpha\nIBAN\u00a0123\u2028BIC\n"));
    b.saveCurrentNote(QStringLiteral("# Alpha\nIBAN 123\nBIC code\n")); // as the editor hands it back
    check(readFile(QStringLiteral("A.md"))
              == QStringLiteral("---\napple-note-id: id-a\n---\n# Alpha\nIBAN\u00a0123\u2028BIC code\n"),
          "backend save keeps Apple's special spaces and breaks");
    b.saveCurrentNote(QStringLiteral("# Alpha\nchanged\n"));
    check(b.saveWarning(QStringLiteral("<<<<<<< x\n")).contains(QStringLiteral("conflict")),
          "backend markers warn");
    // A stray copy of A on disk shares its id: saving A warns about the twin.
    writeFile(QStringLiteral("A copy.md"), QStringLiteral("---\napple-note-id: id-a\n---\n# Alpha\nbody\n"));
    b.refresh();
    check(b.saveWarning(QStringLiteral("edited\n")).contains(QStringLiteral("A copy.md")), "backend duplicate id warns");
    QFile::remove(rootPath() + QStringLiteral("/A copy.md"));
    b.refresh();

    // Scans are cached per file by mtime and size: a later rewrite of the
    // same size is still seen. (Bump the mtime explicitly: on a fast disk
    // this write can land in the same millisecond as the previous one.)
    writeFile(QStringLiteral("A.md"), QStringLiteral("---\napple-note-id: id-a\n---\n# Alpha\nchangeZ\n"));
    {
        QFile f(rootPath() + QStringLiteral("/A.md"));
        if (f.open(QIODevice::ReadWrite))
            f.setFileTime(QDateTime::currentDateTime().addSecs(2), QFileDevice::FileModificationTime);
    }
    b.refresh();
    check(b.noteDetails().value(QStringLiteral("A.md")).toMap().value(QStringLiteral("snippet")).toString()
              == QStringLiteral("changeZ"),
          "backend scan cache follows rewrites");

    // A merge conflict opens as versions to pick from; picking one writes
    // the note without markers and clears its badge.
    writeFile(QStringLiteral("C.md"), QStringLiteral("# C\n<<<<<<< local\nsame\nmine\n||||||| base\nsame\n=======\nsame\ntheirs\n>>>>>>> remote\nend\n"));
    b.refresh();
    b.openNote(QStringLiteral("C.md"));
    {
        const QVariantList conflicts = b.noteConflicts();
        const QVariantList local = conflicts.value(0).toMap().value(QStringLiteral("local")).toList();
        check(conflicts.size() == 1 && local.size() == 2
                  && !local.at(0).toMap().value(QStringLiteral("changed")).toBool()
                  && local.at(1).toMap().value(QStringLiteral("changed")).toBool(),
              "backend conflict versions with changed lines");
        check(conflicts.value(0).toMap().value(QStringLiteral("before")).toStringList() == QStringList{ QStringLiteral("# C") }
                  && conflicts.value(0).toMap().value(QStringLiteral("after")).toStringList() == QStringList{ QStringLiteral("end") },
              "backend conflict context");
    }
    check(!b.resolveConflicts({}).isEmpty(), "backend resolve needs a choice");
    check(b.resolveConflicts({ QStringLiteral("remote") }).isEmpty(), "backend resolve ok");
    check(readFile(QStringLiteral("C.md")) == QStringLiteral("# C\nsame\ntheirs\nend\n"), "backend resolve writes the pick");
    check(b.noteConflicts().isEmpty() && !hasFlag(b, QStringLiteral("C.md"), "conflict"), "backend resolve clears the conflict");
    QFile::remove(rootPath() + QStringLiteral("/C.md"));
    b.refresh();

    // A note rewritten on disk after it was loaded is never saved over:
    // the save is refused, and the unsaved edits become a conflict with it.
    writeFile(QStringLiteral("D.md"), QStringLiteral("---\napple-note-id: id-d\n---\n# D\none\ntwo\n"));
    b.refresh();
    b.openNote(QStringLiteral("D.md"));
    writeFile(QStringLiteral("D.md"), QStringLiteral("---\napple-note-id: id-d\n---\n# D\none\nfrom iCloud\n"));
    check(!b.saveCurrentNote(QStringLiteral("# D\none\nmine\n")), "backend stale save refused");
    check(readFile(QStringLiteral("D.md")).contains(QStringLiteral("from iCloud")), "backend stale save wrote nothing");
    check(!b.keepEditsAsConflict(QStringLiteral("# D\none\ntwo\n"), QStringLiteral("# D\none\nfrom iCloud\n")),
          "backend no conflict when the disk holds the edits");
    check(b.keepEditsAsConflict(QStringLiteral("# D\none\ntwo\n"), QStringLiteral("# D\none\nmine\n")), "backend edits kept as conflict");
    check(readFile(QStringLiteral("D.md")).startsWith(QStringLiteral("---\napple-note-id: id-d\n---\n# D\none\n<<<<<<< local\nmine\n"))
              && b.noteConflicts().size() == 1,
          "backend kept conflict keeps the envelope and opens as versions");
    // Edits to different lines merge into the note, with nothing to pick.
    writeFile(QStringLiteral("D.md"), QStringLiteral("---\napple-note-id: id-d\n---\n# D\none\n\ntwo\n"));
    b.refresh();
    b.openNote(QStringLiteral("D.md"));
    writeFile(QStringLiteral("D.md"), QStringLiteral("---\napple-note-id: id-d\n---\n# D\none\n\ntwo from iCloud\n"));
    {
        bool changed = false;
        QObject::connect(&b, &NotesBackend::vaultChanged, &b, [&] { changed = true; }, Qt::SingleShotConnection);
        check(!b.saveCurrentNote(QStringLiteral("# D\none, mine\n\ntwo\n")), "backend stale save refused before a merge");
        check(b.keepEditsAsConflict(QStringLiteral("# D\none\n\ntwo\n"), QStringLiteral("# D\none, mine\n\ntwo\n")),
              "backend separate edits merged");
        check(readFile(QStringLiteral("D.md"))
                      == QStringLiteral("---\napple-note-id: id-d\n---\n# D\none, mine\n\ntwo from iCloud\n")
                  && b.noteConflicts().isEmpty() && changed,
              "backend clean merge written without markers, and synced");
    }
    QFile::remove(rootPath() + QStringLiteral("/D.md"));
    b.refresh();

    // A pull moves (and retitles) the note open with unsaved edits: it is
    // followed to its new file by its id, and the edits merge there.
    writeFile(QStringLiteral("Moving.md"), QStringLiteral("---\napple-note-id: id-move\n---\n# Moving\none\n\ntwo\n"));
    b.refresh();
    b.openNote(QStringLiteral("Moving.md"));
    QFile::remove(rootPath() + QStringLiteral("/Moving.md"));
    writeFile(QStringLiteral("Elsewhere/Moved.md"), QStringLiteral("---\napple-note-id: id-move\n---\n# Moved\none\n\ntwo\n"));
    b.refresh();
    check(b.currentFolder() == QStringLiteral("Elsewhere") && b.currentNote() == QStringLiteral("Moved.md"),
          "backend note moved by a pull is followed by its id");
    check(b.keepEditsAsConflict(QStringLiteral("# Moving\none\n\ntwo\n"), QStringLiteral("# Moving\none\n\ntwo, mine\n"))
              && readFile(QStringLiteral("Elsewhere/Moved.md"))
                     == QStringLiteral("---\napple-note-id: id-move\n---\n# Moved\none\n\ntwo, mine\n"),
          "backend edits merge into the moved note");
    QDir(rootPath() + QStringLiteral("/Elsewhere")).removeRecursively();
    b.setCurrentFolder(QString());
    b.refresh();

    // A pull deletes it: nothing to save to (the save says so instead of
    // dropping the edits), and they are kept as a new note.
    writeFile(QStringLiteral("Doomed.md"), QStringLiteral("---\napple-note-id: id-doomed\n---\n# Doomed\nkeep\u00a0me\n"));
    b.refresh();
    b.openNote(QStringLiteral("Doomed.md"));
    QFile::remove(rootPath() + QStringLiteral("/Doomed.md"));
    b.refresh();
    check(b.currentNote().isEmpty(), "backend deleted note closes");
    check(!b.saveCurrentNote(QStringLiteral("# Doomed\nkeep me, edited\n")), "backend save with no note is refused");
    {
        QString told;
        QObject::connect(&b, &NotesBackend::editsKeptAsNote, &b, [&](const QString &m) { told = m; },
                         Qt::SingleShotConnection);
        check(b.keepEditsAsConflict(QStringLiteral("# Doomed\nkeep me\n"), QStringLiteral("# Doomed\nkeep me, edited\n"))
                  && b.currentNote() == QStringLiteral("Doomed (unsaved edits).md")
                  && readFile(QStringLiteral("Doomed (unsaved edits).md")) == QStringLiteral("# Doomed\nkeep\u00a0me, edited\n")
                  && told.contains(QStringLiteral("Doomed (unsaved edits)")),
              "backend edits to a deleted note kept as a new note, and said so");
    }
    QFile::remove(rootPath() + QStringLiteral("/Doomed (unsaved edits).md"));
    b.refresh();

    // Unsaved edits on a note with an unresolved conflict are never merged
    // into it (that nests the markers): they go to a new note, and the
    // conflicted note stays exactly as it is.
    {
        const QString block = QStringLiteral("---\napple-note-id: id-clash\n---\n# Clash\nsame\n"
                                             "<<<<<<< local\nx\n=======\ny\n>>>>>>> remote\n");
        writeFile(QStringLiteral("Clash.md"), block);
        b.refresh();
        b.openNote(QStringLiteral("Clash.md"));
        const QString loaded = b.noteBody();
        const QString changed = QStringLiteral("---\napple-note-id: id-clash\n---\n# Clash\nsame, from iCloud\n"
                                               "<<<<<<< local\nx\n=======\ny\n>>>>>>> remote\n");
        writeFile(QStringLiteral("Clash.md"), changed);
        const QString mine = QStringLiteral("# Clash\nsame, mine\n<<<<<<< local\nx\n=======\ny\n>>>>>>> remote\n");
        QString told;
        QObject::connect(&b, &NotesBackend::editsKeptAsNote, &b, [&](const QString &m) { told = m; },
                         Qt::SingleShotConnection);
        check(b.keepEditsAsConflict(loaded, mine) && b.currentNote() == QStringLiteral("Clash (unsaved edits).md")
                  && readFile(QStringLiteral("Clash (unsaved edits).md")) == mine,
              "backend edits on a conflicted note go to a new note");
        check(readFile(QStringLiteral("Clash.md")) == changed, "backend conflicted note left as it is");
        check(told.contains(QStringLiteral("unresolved conflict")) && told.contains(QStringLiteral("Clash (unsaved edits)")),
              "backend says why the edits are in a new note");
        QFile::remove(rootPath() + QStringLiteral("/Clash (unsaved edits).md"));

        // The note resolved elsewhere, but the editor still holds markers.
        writeFile(QStringLiteral("Clash.md"), block);
        b.refresh();
        b.openNote(QStringLiteral("Clash.md"));
        const QString resolved = QStringLiteral("---\napple-note-id: id-clash\n---\n# Clash\nsame\ny\n");
        writeFile(QStringLiteral("Clash.md"), resolved);
        check(b.keepEditsAsConflict(b.noteBody(), mine) && b.currentNote() == QStringLiteral("Clash (unsaved edits).md")
                  && readFile(QStringLiteral("Clash.md")) == resolved,
              "backend marker-bearing edits are not merged into a resolved note");
        QFile::remove(rootPath() + QStringLiteral("/Clash (unsaved edits).md"));
        QFile::remove(rootPath() + QStringLiteral("/Clash.md"));
        b.refresh();
    }

    // Markers nothing can read (the shape a nested merge leaves): no
    // versions to pick, but a way out, each backed up first.
    {
        const QString envelope = QStringLiteral("---\napple-note-id: id-order\n---\n");
        const QString nested = QStringLiteral("# Shopping\n<<<<<<< local\n<<<<<<< local\n- apples\n||||||| base\n"
                                              "- pears\n=======\n- plums\n>>>>>>> remote\n||||||| base\n- pears\n"
                                              "=======\n- grapes\n>>>>>>> remote\n- bread\n");
        const QString synced = QStringLiteral("# Shopping\n- pears\n- bread\n");
        const QString backups = rootPath() + QStringLiteral("/.icloud-md/conflict-backups");
        auto backupFiles = [&] { return QDir(backups).entryList({ QStringLiteral("*.md") }, QDir::Files, QDir::Name); };
        writeFile(QStringLiteral("Order.md"), envelope + nested);
        b.refresh();
        b.openNote(QStringLiteral("Order.md"));
        check(b.noteConflictsUnreadable() && b.noteConflicts().isEmpty() && !b.noteHasSyncedCopy(),
              "backend unreadable markers detected");
        check(hasFlag(b, QStringLiteral("Order.md"), "conflict"), "backend unreadable note still flagged");
        check(!b.recoverConflictedNote(QStringLiteral("synced")).value(QStringLiteral("ok")).toBool()
                  && readFile(QStringLiteral("Order.md")) == envelope + nested && backupFiles().isEmpty(),
              "backend no synced copy: nothing replaced");
        check(!b.recoverConflictedNote(QStringLiteral("bogus")).value(QStringLiteral("ok")).toBool()
                  && readFile(QStringLiteral("Order.md")) == envelope + nested,
              "backend unknown recovery refused");
        writeFile(QStringLiteral("Order.md"), envelope + nested + QStringLiteral("- milk\n"));
        check(!b.recoverConflictedNote(QStringLiteral("strip")).value(QStringLiteral("ok")).toBool()
                  && readFile(QStringLiteral("Order.md")) == envelope + nested + QStringLiteral("- milk\n"),
              "backend recovery refused over a newer change on disk");
        writeFile(QStringLiteral("Order.md"), envelope + nested);
        b.refresh();

        // Remove the markers: every line but the markers.
        const QVariantMap stripped = b.recoverConflictedNote(QStringLiteral("strip"));
        const QString backup1 = stripped.value(QStringLiteral("backup")).toString();
        check(stripped.value(QStringLiteral("ok")).toBool()
                  && readFile(QStringLiteral("Order.md"))
                         == envelope + QStringLiteral("# Shopping\n- apples\n- pears\n- plums\n- pears\n- grapes\n- bread\n")
                  && !b.noteConflictsUnreadable() && !hasFlag(b, QStringLiteral("Order.md"), "conflict"),
              "backend strip keeps every line but the markers");
        check(backup1.startsWith(backups + QLatin1Char('/')) && readFile(QDir(rootPath()).relativeFilePath(backup1)) == envelope + nested
                  && stripped.value(QStringLiteral("message")).toString().contains(QStringLiteral(".icloud-md/conflict-backups/")),
              "backend strip backed the note up first, and says where");
        check(!b.notes().contains(QFileInfo(backup1).fileName()), "backend backup is not a note");

        // Use the last synced version: the base copy, envelope kept.
        writeFile(QStringLiteral(".icloud-md/base/id-order.md"), synced);
        writeFile(QStringLiteral("Order.md"), envelope + nested);
        b.refresh();
        check(b.noteHasSyncedCopy(), "backend synced copy found");
        const QVariantMap restored = b.recoverConflictedNote(QStringLiteral("synced"));
        const QString backup2 = restored.value(QStringLiteral("backup")).toString();
        check(restored.value(QStringLiteral("ok")).toBool() && readFile(QStringLiteral("Order.md")) == envelope + synced,
              "backend synced version restored");
        check(backup2 != backup1 && readFile(QDir(rootPath()).relativeFilePath(backup2)) == envelope + nested
                  && backupFiles().size() == 2,
              "backend synced restore backed the note up first");
        check(!b.recoverConflictedNote(QStringLiteral("strip")).value(QStringLiteral("ok")).toBool(),
              "backend recovery only for unreadable markers");
        QFile::remove(rootPath() + QStringLiteral("/Order.md"));
        QFile::remove(rootPath() + QStringLiteral("/.icloud-md/base/id-order.md"));
        QDir(backups).removeRecursively();
        b.refresh();
    }

    // In-body rename retitles the first line, keeping the envelope.
    b.openNote(QStringLiteral("A.md"));
    check(b.renameCurrentNote(QStringLiteral("Renamed")).isEmpty(), "backend rename ok");
    check(b.noteContent().startsWith(QStringLiteral("---\napple-note-id: id-a\n---\n# Renamed\n")),
          "backend rename retitles line");
    check(b.noteDetails().value(QStringLiteral("A.md")).toMap().value(QStringLiteral("title")).toString()
              == QStringLiteral("Renamed"),
          "backend rename updates list title");
    check(!hasFlag(b, QStringLiteral("A.md"), "missing-id"), "backend rename keeps id");

    // Filename mode renames the file instead.
    writeFile(QStringLiteral(".icloud-md/state.json"),
              QStringLiteral(R"({"titleMode":"filename","notes":{"id-a":{"file":"A.md"}}})"));
    check(b.vaultTitleMode() == QStringLiteral("filename"), "backend mode filename");
    check(b.renameCurrentNote(QStringLiteral("Second")).isEmpty(), "backend file rename ok");
    check(b.currentNote() == QStringLiteral("Second.md"), "backend file rename updates note");
    check(QFile::exists(rootPath() + QStringLiteral("/Second.md")), "backend file rename on disk");
    check(b.renameCurrentNote(QStringLiteral("Second")).isEmpty(), "backend same-name noop");
    check(!b.renameCurrentNote(QStringLiteral("")).isEmpty(), "backend empty title refused");
    b.newNote(QStringLiteral("Plain"));
    check(readFile(QStringLiteral("Plain.md")).isEmpty(), "backend filename-mode note starts empty");

    // List details + vault search. Filename mode titles by file name, so
    // Second.md (whose first line still reads "# Renamed") titles "Second".
    {
        const QVariantMap d = b.noteDetails().value(QStringLiteral("Second.md")).toMap();
        check(d.value(QStringLiteral("title")).toString() == QStringLiteral("Second"),
              "backend detail title");
        check(d.value(QStringLiteral("modifiedMs")).toLongLong() > 0, "backend detail mtime");
        const QVariantList hits = b.searchVault(QStringLiteral("renamed"));
        check(hits.size() == 1
                  && hits.at(0).toMap().value(QStringLiteral("file")).toString() == QStringLiteral("Second.md"),
              "backend search finds");
        const QVariantList nested = b.searchVault(QStringLiteral("nested"));
        check(nested.size() == 1
                  && nested.at(0).toMap().value(QStringLiteral("folder")).toString() == QStringLiteral("Sub"),
              "backend search reports folder");
        check(b.searchVault(QStringLiteral("zzz-no-match")).isEmpty(), "backend search empty");
        check(b.searchVault(QStringLiteral("x")).isEmpty(), "backend search needs 2 chars");
    }

    // Folders: rename moves the directory (and the selection with it),
    // delete trashes it and falls back to All Notes.
    b.setCurrentFolder(QStringLiteral("Sub"));
    check(b.renameCurrentFolder(QStringLiteral("Moved")).isEmpty(), "backend folder rename ok");
    check(b.currentFolder() == QStringLiteral("Moved") && QFile::exists(rootPath() + QStringLiteral("/Moved/H.md")),
          "backend folder rename moves notes");
    check(!b.renameCurrentFolder(QStringLiteral("")).isEmpty(), "backend folder rename refuses empty");
    check(b.deleteCurrentFolder().isEmpty(), "backend folder delete ok");
    check(b.currentFolder().isEmpty() && !QDir(rootPath() + QStringLiteral("/Moved")).exists(),
          "backend folder delete");
    emptyTestTrash();
    b.setCurrentFolder(QString());

    // PDF export writes next to the note and never overwrites.
    b.openNote(QStringLiteral("Second.md"));
    check(b.exportPdf().isEmpty(), "backend pdf exports");
    check(QFile::exists(rootPath() + QStringLiteral("/Second.pdf")), "backend pdf on disk");
    check(!b.exportPdf().isEmpty(), "backend pdf no overwrite");

    // No icloud-session on the bus: the sign-in is simply unknown, with no
    // crash, no countdown, no banner and no pause. (bin/test's private bus
    // has no service files, so nothing is D-Bus activated either.)
    waitForSignIn(b);
    check(!b.signInKnown() && b.signInDaysLeft() == -2 && !b.authExpired(),
          "session absent leaves the sign-in unknown");

    // The fake daemon, signed in with "Keep me signed in": 20 days left. It
    // appears after Notes started, which Notes notices on its own.
    QDBusConnection fakeBus = QDBusConnection::connectToBus(QDBusConnection::SessionBus, QStringLiteral("fake-session"));
    check(fakeBus.isConnected(), "session private bus connected");
    FakeSession fake(fakeBus);
    fake.signedIn = true;
    fake.appleId = QStringLiteral("someone@example.com");
    fake.dsid = QStringLiteral("1006081438");
    fake.expiresAt = seconds(20 * 86400 + 3600).toULongLong();
    check(fake.claimName(), "session fake owns the bus name");
    check(waitUntil([&] { return b.signedIn(); }), "session appearing later is read");
    check(b.appleId() == QStringLiteral("someone@example.com") && b.signInDaysLeft() == 20 && !b.authExpired(),
          "session signed in: days left and no banner");
    fake.set({ { QStringLiteral("ExpiresAt"), QVariant::fromValue<qulonglong>(0) } });
    check(waitUntil([&] { return b.signInDaysLeft() == -1; }), "session without a lasting sign-in");
    fake.set({ { QStringLiteral("ExpiresAt"), seconds(20 * 86400 + 3600) } });
    check(waitUntil([&] { return b.signInDaysLeft() == 20; }), "session expiry follows PropertiesChanged");

    // CLI seam with the stub icloud-notes-sync: same argv, stdout, stderr,
    // exit codes and parsing the app uses against the real tool. No Apple account involved.
    const QString stubs =
        QDir(QCoreApplication::applicationDirPath() + QStringLiteral("/../stubs")).canonicalPath();
    check(QFile::exists(stubs + QStringLiteral("/icloud-notes-sync")), "stub present");
    const QByteArray systemPath = qgetenv("PATH");
    qputenv("PATH", (stubs + QLatin1Char(':') + QString::fromLocal8Bit(systemPath)).toUtf8());
    check(b.syncToolAvailable(), "stub on PATH");

    b.refreshPushPreview();
    waitForSync(b);
    check(b.statusError().isEmpty(), "seam preview ok");
    check(b.statusEntries().size() == 3, "seam preview entries");
    check(b.statusEntries().at(1).toMap().value(QStringLiteral("resolution")).toString() == QStringLiteral("refused"),
          "seam preview refusal");
    check(b.statusEntries().at(1).toMap().value(QStringLiteral("reason")).toString().contains(QStringLiteral("attachments")),
          "seam preview reason");
    check(b.statusUnchanged() == 2, "seam preview unchanged");
    check(b.statusNotices().size() == 1, "seam preview notices");
    // Exit 3 (entries to push) is the preview's answer, not a failure, and
    // the progress on stderr is logged but never parsed as the JSON.
    check(b.syncLog().contains(QStringLiteral("(exit 3)")) && b.syncMessage() == QStringLiteral("Push preview done."),
          "seam preview exit 3 is success");
    check(b.syncLog().contains(QStringLiteral("icloud-md:progress:fetch:12")), "seam preview stderr in the log");

    b.runHistory();
    waitForSync(b);
    check(b.historyError().isEmpty(), "seam history ok");
    check(b.historyEntries().size() == 1
              && b.historyEntries().at(0).toMap().value(QStringLiteral("id")).toString() == QStringLiteral("e9"),
          "seam history epochs");

    b.runDiff(QStringLiteral("e9"));
    waitForSync(b);
    check(b.diffText().contains(QStringLiteral("+ new")), "seam diff text");
    check(b.historyError().isEmpty() && b.syncMessage() == QStringLiteral("Diff done."), "seam diff exit 3 is success");
    check(!b.diffText().contains(QStringLiteral("Fetching")), "seam diff text is stdout alone");

    b.runPull();
    waitForSync(b);
    check(b.syncMessage() == QStringLiteral("Pull done."), "seam pull done");
    {
        // The chain ends once, after the pull: what unlocks an editor that
        // waited for it (syncRunning also flips off between the halves).
        QStringList ended;
        QString lastRun;
        const auto c1 = QObject::connect(&b, &NotesBackend::syncFinished, &b, [&](const QString &label) { lastRun = label; });
        const auto c2 = QObject::connect(&b, &NotesBackend::syncChainFinished, &b, [&] { ended << lastRun; });
        b.runSync(); // push, then pull
        waitForSync(b);
        if (b.syncRunning())
            waitForSync(b);
        check(b.syncLog().contains(QStringLiteral("$ icloud-notes-sync push\n")) && b.syncMessage() == QStringLiteral("Pull done."),
              "seam sync pushes then pulls");
        check(ended == QStringList{ QStringLiteral("Pull") }, "seam sync chain ends once, after the pull");
        QObject::disconnect(c1);
        QObject::disconnect(c2);
    }
    check(b.statusEntries().isEmpty(), "seam pull clears stale preview");

    // A save (Ctrl+S, leaving the note) while a pull runs writes nothing
    // under it; the edits land once it is done, merged with its changes.
    writeFile(QStringLiteral("Q.md"), QStringLiteral("---\napple-note-id: id-q\n---\n# Q\none\n\ntwo\n"));
    b.refresh();
    b.openNote(QStringLiteral("Q.md"));
    qputenv("ICLOUD_NOTES_SYNC_STUB_SLEEP", "1");
    {
        QString written;
        QObject::connect(&b, &NotesBackend::queuedSaveWritten, &b, [&](const QString &body) { written = body; },
                         Qt::SingleShotConnection);
        b.runPull();
        check(!b.saveCurrentNote(QStringLiteral("# Q\none, mine\n\ntwo\n"))
                  && readFile(QStringLiteral("Q.md")) == QStringLiteral("---\napple-note-id: id-q\n---\n# Q\none\n\ntwo\n"),
              "seam save under a running pull waits");
        writeFile(QStringLiteral("Q.md"), QStringLiteral("---\napple-note-id: id-q\n---\n# Q\none\n\ntwo from iCloud\n"));
        waitForIdle(b);
        check(readFile(QStringLiteral("Q.md")) == QStringLiteral("---\napple-note-id: id-q\n---\n# Q\none, mine\n\ntwo from iCloud\n")
                  && written == QStringLiteral("# Q\none, mine\n\ntwo\n"),
              "seam waiting save merged in once the pull is done");
    }
    b.runPull(); // a pull that leaves the note alone: saved as asked
    b.saveCurrentNote(QStringLiteral("# Q\none, mine again\n\ntwo from iCloud\n"));
    waitForIdle(b);
    check(readFile(QStringLiteral("Q.md"))
              == QStringLiteral("---\napple-note-id: id-q\n---\n# Q\none, mine again\n\ntwo from iCloud\n"),
          "seam waiting save written once the pull is done");
    qunsetenv("ICLOUD_NOTES_SYNC_STUB_SLEEP");
    QFile::remove(rootPath() + QStringLiteral("/Q.md"));
    b.refresh();

    // icloud-notes-sync refused a copy of the session that icloud-session says still
    // works (and has refreshed): one retry goes through.
    fake.stillSignedIn = true;
    qputenv("ICLOUD_NOTES_SYNC_STUB_EXPIRED", "1");
    b.runPull();
    waitForSync(b);
    qunsetenv("ICLOUD_NOTES_SYNC_STUB_EXPIRED"); // the refreshed copy works
    check(waitUntil([&] { return fake.reportCalls == 1 && !b.syncRunning() && !b.authExpired()
                                 && b.syncMessage() == QStringLiteral("Pull done."); }),
          "seam refused copy of a working session retries once");
    // If the retry is refused too, syncing pauses instead of retrying again.
    qputenv("ICLOUD_NOTES_SYNC_STUB_EXPIRED", "1");
    b.runPull();
    waitForSync(b);
    check(waitUntil([&] { return fake.reportCalls == 3 && !b.syncRunning(); }) && b.authExpired(),
          "seam a second refusal pauses");
    fake.stillSignedIn = false;
    qunsetenv("ICLOUD_NOTES_SYNC_STUB_EXPIRED");
    // A token rotation moves ExpiresAt; that is not a sign-in.
    fake.set({ { QStringLiteral("ExpiresAt"), QVariant::fromValue<qulonglong>(fake.expiresAt + 60) } });
    waitUntil([] { return false; }, 300); // let the change arrive
    check(b.authExpired() && fake.reportCalls == 3, "seam a token rotation does not resume or retry");
    fake.set({ { QStringLiteral("SigningIn"), true } }); // signed in again through the window
    fake.set({ { QStringLiteral("SigningIn"), false } });
    check(waitUntil([&] { return !b.authExpired() && !b.syncRunning(); }), "seam a new sign-in resumes after the pause");
    waitForIdle(b);
    fake.reportCalls = 0;

    // A push with nothing to send never asks iCloud, so it proves nothing:
    // push fine and pull refused retries once, then pauses. (It used to
    // re-arm the retry and loop for as long as the pull was refused.)
    fake.stillSignedIn = true;
    qputenv("ICLOUD_NOTES_SYNC_STUB_EXPIRED", "pull");
    b.runSync();
    check(waitUntil([&] { return fake.reportCalls == 2 && b.idle() && b.authExpired(); }, 15000),
          "seam refused pull after an empty push retries once, then pauses");
    waitUntil([] { return false; }, 700);
    check(fake.reportCalls == 2 && !b.syncRunning() && b.authExpired(), "seam refused pull does not retry again");
    qunsetenv("ICLOUD_NOTES_SYNC_STUB_EXPIRED");
    fake.stillSignedIn = false;
    // Signing in again can leave the expiry as it was (0 for a sign-in that
    // does not last): the sign-in window closing while signed in resumes.
    fake.set({ { QStringLiteral("SigningIn"), true } });
    waitUntil([&] { return b.signingIn(); });
    fake.set({ { QStringLiteral("SigningIn"), false } });
    check(waitUntil([&] { return !b.authExpired() && !b.syncRunning()
                                 && b.syncMessage() == QStringLiteral("Pull done."); }, 15000),
          "seam re-sign-in with an unchanged expiry resumes");

    // icloud-session's answer (it checks with Apple first) arrives while
    // another run is going: the retry waits for it instead of being dropped.
    fake.reportCalls = 0;
    fake.stillSignedIn = true;
    fake.reportDelayMs = 700;
    qputenv("ICLOUD_NOTES_SYNC_STUB_EXPIRED", "1");
    b.runPull();
    waitForSync(b);
    qunsetenv("ICLOUD_NOTES_SYNC_STUB_EXPIRED");
    check(waitUntil([&] { return fake.reportCalls == 1; }) && b.authExpired(), "seam slow report: paused while it is checked");
    qputenv("ICLOUD_NOTES_SYNC_STUB_SLEEP", "1");
    b.clearLog();
    b.runPush(); // still running when the answer comes
    check(waitUntil([&] { return !b.syncRunning() && b.idle() && b.syncMessage() == QStringLiteral("Pull done."); }, 15000)
              && b.syncLog().contains(QStringLiteral("$ icloud-notes-sync pull\n")) && !b.authExpired(),
          "seam report answered during a run retries after it");
    qunsetenv("ICLOUD_NOTES_SYNC_STUB_SLEEP");
    fake.reportDelayMs = 0;

    // A report nobody could answer leaves the sign-in unknown: not paused,
    // and no retry either.
    fake.reportCalls = 0;
    fake.reportFails = true;
    qputenv("ICLOUD_NOTES_SYNC_STUB_EXPIRED", "1");
    b.clearLog();
    b.runPull();
    waitForSync(b);
    check(waitUntil([&] { return fake.reportCalls == 1 && b.idle(); }) && !b.authExpired()
              && b.syncLog().count(QStringLiteral("$ icloud-notes-sync pull\n")) == 1,
          "seam failed report is unknown, not paused, and not retried");
    qunsetenv("ICLOUD_NOTES_SYNC_STUB_EXPIRED");
    fake.reportFails = false;
    fake.stillSignedIn = false;
    fake.reportCalls = 0;

    // A sign-in required is exit 2 (4 from icloud-notes-sync 0.1.1 and
    // older); icloud-md's text marker (with exit 1) still counts too. Each
    // alone pauses syncing and reports it.
    for (const char *style : { "code", "legacy", "marker" }) {
        fake.reportCalls = 0;
        qputenv("ICLOUD_NOTES_SYNC_STUB_SIGNIN", style);
        qputenv("ICLOUD_NOTES_SYNC_STUB_EXPIRED", "1");
        b.runPull();
        waitForSync(b);
        qunsetenv("ICLOUD_NOTES_SYNC_STUB_EXPIRED");
        qunsetenv("ICLOUD_NOTES_SYNC_STUB_SIGNIN");
        const QByteArray name = QByteArray("seam ") + style;
        check(b.authExpired() && waitUntil([&] { return fake.reportCalls == 1; })
                  && b.syncMessage() == QStringLiteral("Sync paused. Sign in to iCloud to resume."),
              (name + " alone pauses and reports").constData());
        fake.set({ { QStringLiteral("SigningIn"), true } });
        waitUntil([&] { return b.signingIn(); });
        fake.set({ { QStringLiteral("SigningIn"), false } });
        check(waitUntil([&] { return !b.authExpired() && b.idle() && b.syncMessage() == QStringLiteral("Pull done."); }, 15000),
              (name + ": a new sign-in resumes").constData());
    }
    fake.reportCalls = 0;

    // icloud-notes-sync refused the session: icloud-session is told, and syncing
    // pauses (no further push or pull) until it reports a sign-in.
    qputenv("ICLOUD_NOTES_SYNC_STUB_EXPIRED", "1");
    b.runPull();
    waitForSync(b);
    check(b.authExpired(), "seam expired session detected");
    check(waitUntil([&] { return fake.reportCalls == 1; }), "seam expired session reported to icloud-session");
    check(b.syncMessage() == QStringLiteral("Sync paused. Sign in to iCloud to resume."),
          "seam expired session named once, not as a generic failure");
    int chainEnds = 0;
    const auto chainEnd = QObject::connect(&b, &NotesBackend::syncChainFinished, &b, [&] { ++chainEnds; });
    b.runSync(); // a push that hits the expired session skips its pull
    waitForSync(b);
    QObject::disconnect(chainEnd);
    check(chainEnds == 1, "seam expired push ends the chain");
    check(!b.syncRunning() && b.syncMessage() == QStringLiteral("Sync paused. Sign in to iCloud to resume."),
          "seam expired push does not report a failure or pull");
    check(!b.syncLog().contains(QStringLiteral("reauthenticate\n")), "seam never runs a reauthenticate of its own");
    // The daemon confirms with Apple and signs out.
    fake.set({ { QStringLiteral("SignedIn"), false }, { QStringLiteral("AppleId"), QString() },
               { QStringLiteral("Dsid"), QString() }, { QStringLiteral("ExpiresAt"), QVariant::fromValue<qulonglong>(0) } });
    check(waitUntil([&] { return !b.signedIn(); }) && b.authExpired() && b.signInDaysLeft() == -2,
          "seam signed out stays paused");
    {
        NotesBackend relaunched; // the daemon remembers: the next launch starts paused
        waitForSignIn(relaunched);
        check(relaunched.signInKnown() && relaunched.authExpired()
                  && relaunched.syncMessage() == QStringLiteral("Sync paused. Sign in to iCloud to resume."),
              "seam signed out holds across a relaunch");
    }
    qunsetenv("ICLOUD_NOTES_SYNC_STUB_EXPIRED");

    // The banner's Sign in asks icloud-session and returns at once; the
    // sign-in arrives as property changes, and syncing resumes with a sync.
    b.clearLog();
    b.signIn();
    check(waitUntil([&] { return fake.signInCalls == 1; }), "sign in button calls SignIn");
    check(!b.syncRunning() && b.authExpired(), "sign in waits for SignedIn");
    fake.set({ { QStringLiteral("SigningIn"), true } });
    check(waitUntil([&] { return b.signingIn(); }), "sign in window open is shown");
    fake.set({ { QStringLiteral("SignedIn"), true }, { QStringLiteral("AppleId"), QStringLiteral("someone@example.com") },
               { QStringLiteral("Dsid"), QStringLiteral("1006081438") },
               { QStringLiteral("ExpiresAt"), seconds(30 * 86400 + 3600) }, { QStringLiteral("SigningIn"), false } });
    check(waitUntil([&] { return !b.authExpired(); }), "seam sign-in clears the pause");
    waitUntil([&] { return b.syncMessage() == QStringLiteral("Pull done."); }, 15000);
    check(b.syncLog().contains(QStringLiteral("$ icloud-notes-sync push\n")) && b.syncMessage() == QStringLiteral("Pull done."),
          "seam sign-in resumes syncing");
    check(b.signInDaysLeft() == 30, "seam new sign-in's days left");

    // Signed out from elsewhere (another app, or Apple ended the session):
    // syncing pauses at once, without running icloud-notes-sync into the refusal.
    b.clearLog();
    fake.set({ { QStringLiteral("SignedIn"), false } });
    check(waitUntil([&] { return b.authExpired(); }) && !b.syncRunning()
              && b.syncMessage() == QStringLiteral("Sync paused. Sign in to iCloud to resume."),
          "session signed out elsewhere pauses syncing");
    check(b.syncLog().isEmpty(), "session signed out runs no sync");
    fake.set({ { QStringLiteral("SignedIn"), true } });
    check(waitUntil([&] { return !b.authExpired(); }), "session signed in again resumes");
    waitForIdle(b);
    check(b.syncLog().contains(QStringLiteral("$ icloud-notes-sync pull\n")), "session signed in again syncs");

    // A re-read with nothing new neither pauses nor syncs.
    b.clearLog();
    b.refreshSignIn();
    waitForSignIn(b);
    check(!b.authExpired() && b.signedIn() && b.syncLog().isEmpty(), "session re-read is quiet");

    // Background sync (`icloud-notes --sync`), on a vault of its own so its
    // lock is not the one b holds.
    const QString vaultPath = rootPath();
    const QString bgVault = scratch.path() + QStringLiteral("/background");
    qputenv("ICLOUD_NOTES_VAULT", bgVault.toUtf8());
    auto backgroundSync = [](QString &output) {
        output.clear();
        QTextStream out(&output);
        return runBackgroundSync(out);
    };
    QString bgOut;
    check(backgroundSync(bgOut) == 0 && !bgOut.contains(QStringLiteral("$ icloud-notes-sync")),
          "background: no vault, nothing to do");
    writeFile(QStringLiteral(".icloud-md/state.json"), QStringLiteral(R"({"titleMode":"in-body","notes":{}})"));
    check(NotesBackend::lockPath() != QString::fromUtf8(qgetenv("XDG_RUNTIME_DIR")) + QStringLiteral("/icloud-notes.lock"),
          "background: a vault named by ICLOUD_NOTES_VAULT has a lock of its own");
    {
        const int code = backgroundSync(bgOut);
        const qsizetype push = bgOut.indexOf(QStringLiteral("$ icloud-notes-sync push\n")),
                        pull = bgOut.indexOf(QStringLiteral("$ icloud-notes-sync pull\n"));
        check(code == 0 && push >= 0 && pull > push && bgOut.contains(QStringLiteral("stub pull ok")),
              "background: pushes then pulls, exit 0");
    }
    {
        VaultLock held(NotesBackend::lockPath());
        check(held.tryLock() == VaultLock::Locked, "background: lock taken by another holder");
        check(backgroundSync(bgOut) == 0 && !bgOut.contains(QStringLiteral("$ icloud-notes-sync")),
              "background: skipped while the lock is held");
    }
    {
        NotesBackend app; // the open app holds the lock for its lifetime
        check(backgroundSync(bgOut) == 0 && !bgOut.contains(QStringLiteral("$ icloud-notes-sync"))
                  && bgOut.contains(QStringLiteral("Notes is open"))
                  && bgOut.contains(QStringLiteral("(Notes (pid %1))").arg(QCoreApplication::applicationPid())),
              "background: the open app blocks it, and is named");
    }
    check(backgroundSync(bgOut) == 0 && bgOut.contains(QStringLiteral("$ icloud-notes-sync pull")),
          "background: runs again once the app is closed");
    {
        // The app opening during a background sync waits for it, then syncs.
        VaultLock background(NotesBackend::lockPath());
        background.tryLock(QStringLiteral("a background sync (icloud-notes --sync, pid 1)"));
        NotesBackend app;
        app.runSync();
        check(app.syncRunning()
                  && app.syncMessage() == QStringLiteral("Waiting for a background sync (icloud-notes --sync, pid 1) to finish…"),
              "background: the app's sync waits for the lock, naming its holder");
        waitUntil([] { return false; }, 700);
        check(app.syncRunning() && !app.syncLog().contains(QStringLiteral("stub push ok")),
              "background: nothing runs while it waits");
        background.release();
        check(waitUntil([&] { return !app.syncRunning() && app.syncMessage() == QStringLiteral("Pull done."); }, 15000)
                  && app.syncLog().contains(QStringLiteral("stub push ok")),
              "background: the app syncs once the lock is free");
        check(backgroundSync(bgOut) == 0 && !bgOut.contains(QStringLiteral("$ icloud-notes-sync")),
              "background: the app keeps the lock it waited for");
    }
    {
        // A lock that cannot even be opened never means running icloud-notes-sync
        // without it (it used to sync anyway).
        const QByteArray runtime = qgetenv("XDG_RUNTIME_DIR");
        const QString notADir = scratch.path() + QStringLiteral("/not-a-dir");
        QFile(notADir).open(QIODevice::WriteOnly); // a file where the lock's directory should be
        qputenv("XDG_RUNTIME_DIR", notADir.toUtf8());
        NotesBackend app;
        app.runSync();
        waitForIdle(app);
        check(!app.syncRunning() && !app.syncLog().contains(QStringLiteral("stub push ok"))
                  && !app.syncLog().contains(QStringLiteral("stub pull ok"))
                  && app.syncLog().contains(QStringLiteral("not running icloud-notes-sync without it")),
              "background: no lock, no icloud-notes-sync");
        qputenv("XDG_RUNTIME_DIR", runtime);
    }
    {
        // One window per vault: a second launch asks the first to show
        // itself (passing its activation token on) and does not start.
        const QString base = NotesBackend::lockPath().chopped(5);
        SingleInstance first(base);
        check(first.claim(), "instance: the first launch runs");
        QString token = QStringLiteral("unset");
        QObject::connect(&first, &SingleInstance::activationRequested, &first, [&](const QString &t) { token = t; });
        qputenv("XDG_ACTIVATION_TOKEN", "tok-1");
        SingleInstance second(base);
        check(!second.claim(), "instance: a second launch does not run");
        qunsetenv("XDG_ACTIVATION_TOKEN");
        check(waitUntil([&] { return token != QStringLiteral("unset"); }) && token == QStringLiteral("tok-1"),
              "instance: the second launch shows the first");
    }
    {
        // The first gone, the next launch runs (its stale socket replaced).
        SingleInstance again(NotesBackend::lockPath().chopped(5));
        check(again.claim(), "instance: runs again once the first exits");
    }
    // A refused session fails the run, after telling icloud-session.
    fake.reportCalls = 0;
    qputenv("ICLOUD_NOTES_SYNC_STUB_EXPIRED", "1");
    check(backgroundSync(bgOut) != 0 && fake.reportCalls == 1 && !bgOut.contains(QStringLiteral("$ icloud-notes-sync pull")),
          "background: refused session reported, exit non-zero");
    qunsetenv("ICLOUD_NOTES_SYNC_STUB_EXPIRED");
    // Signed out: nothing runs.
    fake.set({ { QStringLiteral("SignedIn"), false } });
    check(backgroundSync(bgOut) == 0 && !bgOut.contains(QStringLiteral("$ icloud-notes-sync"))
              && bgOut.contains(QStringLiteral("Not signed in")),
          "background: skipped when signed out");
    fake.set({ { QStringLiteral("SignedIn"), true } });
    qputenv("ICLOUD_NOTES_VAULT", vaultPath.toUtf8());
    waitUntil([&] { return !b.authExpired(); });
    waitForIdle(b);

    // First run: no vault yet. Signed in, the clone uses the daemon's
    // account by dsid and opens no window of any kind.
    const QString fresh = scratch.path() + QStringLiteral("/fresh");
    QDir().mkpath(fresh);
    qputenv("ICLOUD_NOTES_VAULT", fresh.toUtf8());
    const QString cloneArgs = QStringLiteral("$ icloud-notes-sync clone --account 1006081438 --non-interactive ") + fresh;
    {
        NotesBackend first;
        waitForSignIn(first);
        check(!first.cloned() && first.signedIn(), "clone signed in, no vault");
        const int signIns = fake.signInCalls;
        first.runClone();
        waitForIdle(first); // the stub rejects clone
        check(first.syncLog().contains(cloneArgs), "clone uses --account <dsid>");
        check(fake.signInCalls == signIns, "clone signed in opens no sign-in");
    }
    // Signed out: sign in through icloud-session first, then clone.
    fake.set({ { QStringLiteral("SignedIn"), false }, { QStringLiteral("Dsid"), QString() } });
    {
        NotesBackend first;
        waitForSignIn(first);
        check(!first.authExpired(), "clone signed out, no vault: nothing to pause");
        bool finished = false;
        QObject::connect(&first, &NotesBackend::cloneFinished, [&] { finished = true; });
        first.runClone();
        check(waitUntil([&] { return fake.signInCalls == 1 + 1; }) && !first.syncRunning(),
              "clone signs in first");
        fake.set({ { QStringLiteral("SigningIn"), true } });
        waitUntil([&] { return first.signingIn(); });
        fake.set({ { QStringLiteral("SignedIn"), true }, { QStringLiteral("Dsid"), QStringLiteral("1006081438") },
                   { QStringLiteral("SigningIn"), false } });
        check(waitUntil([&] { return finished; }, 15000) && first.syncLog().contains(cloneArgs),
              "clone follows the sign-in with --account <dsid>");

        // A sign-in window closed without signing in ends the clone.
        fake.set({ { QStringLiteral("SignedIn"), false }, { QStringLiteral("Dsid"), QString() } });
        waitUntil([&] { return !first.signedIn(); });
        finished = false;
        first.clearLog();
        first.runClone();
        waitUntil([&] { return fake.signInCalls == 3; });
        fake.set({ { QStringLiteral("SigningIn"), true } });
        waitUntil([&] { return first.signingIn(); });
        fake.set({ { QStringLiteral("SigningIn"), false } });
        check(waitUntil([&] { return finished; }) && !first.syncLog().contains(QStringLiteral("clone")),
              "clone cancelled with the sign-in window");
    }

    // The daemon gone: unknown again, and Sign in says why instead of crashing.
    check(fake.releaseName(), "session fake releases the bus name");
    {
        NotesBackend absent;
        waitForSignIn(absent);
        check(!absent.signInKnown() && absent.signInDaysLeft() == -2 && !absent.authExpired(),
              "session absent at launch is unknown");
        absent.signIn();
        check(waitUntil([&] { return absent.syncMessage().contains(QStringLiteral("not available")); }),
              "session absent: Sign in says so");
        bool finished = false;
        QObject::connect(&absent, &NotesBackend::cloneFinished, [&](bool ok) { finished = !ok; });
        absent.runClone();
        check(waitUntil([&] { return finished; }) && !absent.syncLog().contains(QStringLiteral("clone")),
              "session absent: clone refused, not run blind");
    }
    qputenv("ICLOUD_NOTES_VAULT", bgVault.toUtf8());
    check(backgroundSync(bgOut) == 0 && !bgOut.contains(QStringLiteral("$ icloud-notes-sync"))
              && bgOut.contains(QStringLiteral("unknown")),
          "background: skipped when icloud-session is unknown");

    return report();
}
