#include <QGuiApplication>
#include <QQmlApplicationEngine>
#include <QQmlContext>
#include <QQuickStyle>
#include <QWindow>

#include <QTextStream>

#include "src/cli.h"
#include "src/notesbackend.h"
#include "src/singleinstance.h"

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
    // `icloud-notes <command>`: the command line, headless (see cli.h).
    if (argc > 1 && isCliInvocation(argv[1]))
        return cliMain(argc, argv);

    QGuiApplication app(argc, argv);
    app.setOrganizationName(QStringLiteral("icloud-notes"));
    app.setApplicationName(QStringLiteral("icloud-notes"));
    app.setApplicationDisplayName(QStringLiteral("Notes"));

    // One window per vault: a second launch shows the first and exits.
    SingleInstance instance(NotesBackend::lockPath().chopped(5)); // less ".lock"
    if (!instance.claim())
        return 0;

    // Plain style that tracks the system light/dark palette, like Omawrite.
    QQuickStyle::setStyle(QStringLiteral("Basic"));

    NotesBackend backend;

    QQmlApplicationEngine engine;
    engine.rootContext()->setContextProperty(QStringLiteral("backend"), &backend);
    engine.load(QUrl(QStringLiteral("qrc:/qml/main.qml")));
    if (engine.rootObjects().isEmpty())
        return 1;
    QObject::connect(&instance, &SingleInstance::activationRequested, &app, [&engine](const QString &token) {
        auto *window = qobject_cast<QWindow *>(engine.rootObjects().constFirst());
        if (!window)
            return;
        // Wayland only raises a window for the launch that asked, by its token.
        if (!token.isEmpty())
            qputenv("XDG_ACTIVATION_TOKEN", token.toUtf8());
        if (window->visibility() == QWindow::Minimized)
            window->showNormal();
        window->show();
        window->raise();
        window->requestActivate();
    });
    return app.exec();
}
