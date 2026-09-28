#include <QGuiApplication>
#include <QQmlApplicationEngine>
#include <QQmlContext>
#include <QQuickStyle>

#include <QTextStream>

#include "src/backgroundsync.h"
#include "src/notesbackend.h"

int main(int argc, char *argv[])
{
    // `icloud-notes --sync`: one sync with no window (the systemd user
    // timer runs it while the app is closed), so no display either.
    if (argc > 1 && qstrcmp(argv[1], "--sync") == 0) {
        QCoreApplication app(argc, argv);
        app.setOrganizationName(QStringLiteral("icloud-notes"));
        app.setApplicationName(QStringLiteral("icloud-notes"));
        QTextStream out(stdout);
        return runBackgroundSync(out);
    }

    QGuiApplication app(argc, argv);
    app.setOrganizationName(QStringLiteral("icloud-notes"));
    app.setApplicationName(QStringLiteral("icloud-notes"));
    app.setApplicationDisplayName(QStringLiteral("Notes"));

    // Plain style that tracks the system light/dark palette, like Omawrite.
    QQuickStyle::setStyle(QStringLiteral("Basic"));

    NotesBackend backend;

    QQmlApplicationEngine engine;
    engine.rootContext()->setContextProperty(QStringLiteral("backend"), &backend);
    engine.load(QUrl(QStringLiteral("qrc:/qml/main.qml")));
    if (engine.rootObjects().isEmpty())
        return 1;
    return app.exec();
}
