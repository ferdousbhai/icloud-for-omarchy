// Offscreen screenshot tool for styling iterations (not a test): seeds a
// demo vault in a temporary directory, shows the real main.qml, grabs the
// window to a PNG and quits. Usage: shots out.png [path/to/main.qml]
// NOTES_SHOT=bare seeds an empty, unlinked vault instead.
// NOTES_SHOT=readonly shows the open note as one the sync tool will not push.
// NOTES_SHOT=conflict opens it on a merge conflict, for the version picker.
// NOTES_SHOT=unreadable opens it on nested markers the picker can't read.
#include "../check.h"
#include "../src/notesbackend.h"

#include <QDateTime>
#include <QGuiApplication>
#include <QQmlApplicationEngine>
#include <QQmlContext>
#include <QQuickStyle>
#include <QQuickWindow>
#include <QStandardPaths>
#include <QTemporaryDir>
#include <QTimer>

namespace {
// check.h's writeFile, then the file's mtime set daysAgo back, for the
// list's order and dates.
void writeAged(const QString &root, const QString &rel, const QString &content, int daysAgo)
{
    writeFile(rel, content, root);
    QFile f(root + QLatin1Char('/') + rel);
    if (f.open(QIODevice::ReadWrite))
        f.setFileTime(QDateTime::currentDateTime().addDays(-daysAgo), QFileDevice::FileModificationTime);
}
} // namespace

int main(int argc, char *argv[])
{
    if (argc < 2) {
        QTextStream(stderr) << "usage: shots out.png [path/to/main.qml]\n";
        return 2;
    }
    qputenv("QT_QPA_PLATFORM", "offscreen");
    QGuiApplication app(argc, argv);
    app.setOrganizationName(QStringLiteral("icloud-notes"));
    app.setApplicationName(QStringLiteral("icloud-notes-shots")); // its own settings, not the user's
    QStandardPaths::setTestModeEnabled(true); // settings and caches, not documents

    // A throwaway vault; the backend reads the path from ICLOUD_NOTES_VAULT.
    QTemporaryDir scratch;
    if (!scratch.isValid())
        return 1;
    const QString root = scratch.path() + QStringLiteral("/vault");
    qputenv("ICLOUD_NOTES_VAULT", root.toUtf8());
    if (qgetenv("NOTES_SHOT") == "bare") {
        QDir().mkpath(root); // empty, unlinked vault: banner + Clone CTA
    } else {
        const QString readOnly = qgetenv("NOTES_SHOT") == "readonly"
            ? QStringLiteral("is so large that Apple keeps its text in a separate file, which can't be written back yet")
            : QString();
        QStringList groceries{ QStringLiteral("a"), QStringLiteral("Notes/Groceries.md") };
        if (!readOnly.isEmpty())
            groceries << readOnly;
        writeAged(root, QStringLiteral(".icloud-md/state.json"),
                  stateJson(QStringLiteral("in-body"),
                            { groceries,
                              { QStringLiteral("b"), QStringLiteral("Notes/Trip ideas.md") },
                              { QStringLiteral("c"), QStringLiteral("Recipes/Pancakes.md") } }),
                  9);
        writeAged(root, QStringLiteral("Notes/Groceries.md"),
                  qgetenv("NOTES_SHOT") == "unreadable"
                      // Markers merged twice (nested), as a real vault once had them.
                      ? QStringLiteral("---\napple-note-id: a\n---\n\n<<<<<<< local\n# Groceries\nmilk, eggs\n||||||| base\n# Groceries\n=======\n>>>>>>> remote\n\n"
                                       "<<<<<<< local\n<<<<<<< local\n- [ ] oat milk\n||||||| base\n- [ ] coffee\n=======\n- [ ] coffee\n- [ ] maple syrup\n>>>>>>> remote\n"
                                       "||||||| base\n- [ ] coffee\n=======\n>>>>>>> remote\n")
                  : qgetenv("NOTES_SHOT") == "conflict"
                      ? QStringLiteral("---\napple-note-id: a\n---\n# Groceries\nmilk, eggs, **sourdough** from [the bakery](https://example.com)\n\n"
                                       "## Weekend\n<<<<<<< local\n- [ ] oat milk\n- [x] coffee\n- [ ] blueberries\n||||||| base\n"
                                       "- [ ] oat milk\n- [ ] coffee\n=======\n- [ ] oat milk\n- [ ] coffee, the *dark* roast\n"
                                       "- [ ] maple syrup\n>>>>>>> remote\n\n> don't forget the bags\n")
                      : QStringLiteral("---\napple-note-id: a\n---\n# Groceries\nmilk, eggs, **sourdough** from [the bakery](https://example.com)\n\n"
                                       "## Weekend\n- [ ] oat milk\n- [x] coffee\n- *maybe* `pancake mix`\n\n> don't forget the bags\n"),
                  0);
        writeAged(root, QStringLiteral("Notes/Trip ideas.md"),
                  QStringLiteral("---\napple-note-id: b\n---\n# Trip ideas\nKyoto in spring for the cherry blossoms.\n| day | plan |\n| 1 | arrive |\n"),
                  2);
        writeAged(root, QStringLiteral("Recipes/Pancakes.md"),
                  QStringLiteral("---\napple-note-id: c\n---\n# Pancakes\nflour, eggs, very hot pan\n"),
                  5);
    }

    QQuickStyle::setStyle(QStringLiteral("Basic"));
    NotesBackend backend;
    QQmlApplicationEngine engine;
    engine.rootContext()->setContextProperty(QStringLiteral("backend"), &backend);
    const QString qml = argc > 2 ? QString::fromLocal8Bit(argv[2])
                                 : QCoreApplication::applicationDirPath() + QStringLiteral("/../../../qml/main.qml");
    engine.load(QUrl::fromLocalFile(qml));
    auto *window = engine.rootObjects().isEmpty()
        ? nullptr
        : qobject_cast<QQuickWindow *>(engine.rootObjects().constFirst());
    if (!window)
        return 1;
    // Open the first note so the editor pane is populated.
    if (backend.folders().contains(QStringLiteral("Notes"))) {
        backend.setCurrentFolder(QStringLiteral("Notes"));
        backend.openNote(QStringLiteral("Groceries.md"));
    }

    QTimer::singleShot(1500, [&] {
        const QString out = QString::fromLocal8Bit(argv[1]);
        QTextStream(stdout) << (window->grabWindow().save(out) ? "saved " + out : QStringLiteral("grab failed")) << "\n";
        QCoreApplication::quit();
    });
    return app.exec();
}
