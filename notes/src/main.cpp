#include <QGuiApplication>
#include <QQmlApplicationEngine>
#include <QQmlContext>
#include <QQuickStyle>

#include "src/notesbackend.h"

int main(int argc, char *argv[])
{
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
