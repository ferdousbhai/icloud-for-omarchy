// Backend tests: note classification, save warnings, mode-aware rename and
// the icloud-md CLI seam — against a scratch vault under a temporary
// directory, never the real one. Run with bin/test.
#include "../src/notesbackend.h"
#include "check.h"

#include <QDir>
#include <QEventLoop>
#include <QFile>
#include <QGuiApplication>
#include <QStandardPaths>
#include <QTemporaryDir>
#include <QTimer>

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
} // namespace

int main(int argc, char *argv[])
{
    // GUI application: PDF export lays out text and needs the font database.
    // bin/test forces the offscreen platform so this stays headless.
    QGuiApplication app(argc, argv);
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

    writeFile(QStringLiteral(".icloud-md/state.json"),
              QStringLiteral(R"({"titleMode":"in-body","notes":{)"
                             R"("id-a":{"file":"A.md"},)"
                             R"("id-b":{"file":"B.md"},)"
                             R"("id-e":{"file":"E.md"},)"
                             R"("id-f":{"file":"F.md"},)"
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

    // Attachments: only the files this note links to, decoded from the
    // URL-encoded links icloud-md writes; the folder-wide attachments/
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
    check(b.saveWarning(QStringLiteral("edited body\n")).isEmpty(), "backend clean save silent");
    // The envelope is hidden from the editor and reattached on save, so the
    // id cannot be edited away; files on disk keep it byte-for-byte.
    check(b.noteBody() == QStringLiteral("body\n"), "backend body hides envelope and heading");
    b.saveCurrentNote(QStringLiteral("changed\n"));
    check(readFile(QStringLiteral("A.md")) == QStringLiteral("---\napple-note-id: id-a\n---\n# Alpha\nchanged\n"),
          "backend save preserves envelope and heading");
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

    // CLI seam with the stub icloud-md: same argv, stdout, and parsing
    // the app uses against the real tool. No Apple account involved.
    const QString stubs =
        QDir(QCoreApplication::applicationDirPath() + QStringLiteral("/../stubs")).canonicalPath();
    check(QFile::exists(stubs + QStringLiteral("/icloud-md")), "stub present");
    qputenv("PATH", (stubs + QLatin1Char(':') + QString::fromLocal8Bit(qgetenv("PATH"))).toUtf8());
    check(b.icloudMdAvailable(), "stub on PATH");

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

    b.runHistory();
    waitForSync(b);
    check(b.historyError().isEmpty(), "seam history ok");
    check(b.historyEntries().size() == 1
              && b.historyEntries().at(0).toMap().value(QStringLiteral("id")).toString() == QStringLiteral("e9"),
          "seam history epochs");

    b.runDiff(QStringLiteral("e9"));
    waitForSync(b);
    check(b.diffText().contains(QStringLiteral("+ new")), "seam diff text");

    b.runPull();
    waitForSync(b);
    check(b.syncMessage() == QStringLiteral("Pull done."), "seam pull done");
    b.runSync(); // push, then pull
    waitForSync(b);
    if (b.syncRunning())
        waitForSync(b);
    check(b.syncLog().contains(QStringLiteral("$ icloud-md push\n")) && b.syncMessage() == QStringLiteral("Pull done."),
          "seam sync pushes then pulls");
    check(b.statusEntries().isEmpty(), "seam pull clears stale preview");

    // An expired session pauses syncing until a sign-in succeeds, which then syncs.
    qputenv("ICLOUD_MD_STUB_EXPIRED", "1");
    b.runPull();
    waitForSync(b);
    check(b.authExpired(), "seam expired session detected");
    qunsetenv("ICLOUD_MD_STUB_EXPIRED");
    b.runReauthenticate();
    waitForSync(b); // sign-in
    while (b.syncRunning())
        waitForSync(b); // the push and pull it triggers
    check(!b.authExpired() && b.syncMessage() == QStringLiteral("Pull done."), "seam sign-in resumes syncing");

    b.runClone(QStringLiteral("someone@example.com")); // the stub rejects clone
    waitForSync(b);
    check(b.syncMessage() == QStringLiteral("Clone failed — see log."), "seam failure reported");
    check(b.syncLog().contains(QStringLiteral("clone ") + rootPath()
                               + QStringLiteral(" --account someone@example.com --non-interactive")),
          "seam clone reuses the saved account without a browser");

    return report();
}
