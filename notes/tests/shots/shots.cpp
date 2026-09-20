// Offscreen screenshot tool for styling iterations (not a test): seeds a
// demo vault in a temporary directory, shows the real main.qml, grabs the
// window to a PNG and quits. Usage: shots out.png [path/to/main.qml]
// NOTES_SHOT=bare seeds an empty, unlinked vault instead.
#include "../src/notesbackend.h"

#include <QDir>
#include <QFile>
#include <QGuiApplication>
#include <QQmlApplicationEngine>
#include <QQmlContext>
#include <QQuickStyle>
#include <QQuickWindow>
#include <QStandardPaths>
#include <QTemporaryDir>
#include <QTextStream>
#include <QTimer>

namespace {
void writeFile(const QString &root, const QString &rel, const QString &content, int daysAgo)
{
    const QString path = root + QLatin1Char('/') + rel;
    QDir().mkpath(QFileInfo(path).absolutePath());
    QFile f(path);
    if (!f.open(QIODevice::ReadWrite | QIODevice::Text | QIODevice::Truncate))
        return;
    f.write(content.toUtf8());
    f.flush();
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
        writeFile(root, QStringLiteral(".icloud-md/state.json"),
                  QStringLiteral(R"({"titleMode":"in-body","notes":{"a":{"file":"Notes/Groceries.md"},"b":{"file":"Notes/Trip ideas.md"},"c":{"file":"Recipes/Pancakes.md"}}})"),
                  9);
        writeFile(root, QStringLiteral("Notes/Groceries.md"),
                  QStringLiteral("---\napple-note-id: a\n---\n# Groceries\nmilk, eggs, **sourdough** from [the bakery](https://example.com)\n\n"
                                 "## Weekend\n- [ ] oat milk\n- [x] coffee\n- *maybe* `pancake mix`\n\n> don't forget the bags\n"),
                  0);
        writeFile(root, QStringLiteral("Notes/Trip ideas.md"),
                  QStringLiteral("---\napple-note-id: b\n---\n# Trip ideas\nKyoto in spring for the cherry blossoms.\n| day | plan |\n| 1 | arrive |\n"),
                  2);
        writeFile(root, QStringLiteral("Recipes/Pancakes.md"),
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
