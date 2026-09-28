#include "singleinstance.h"

#include <QCoreApplication>
#include <QCryptographicHash>
#include <QFile>
#include <QLocalSocket>

SingleInstance::SingleInstance(const QString &base, QObject *parent)
    : QObject(parent), m_lock(base + QStringLiteral("-app.lock")), m_socketPath(base + QStringLiteral("-app.sock"))
{
    // A Unix socket path has room for about 100 bytes: past that, a short
    // name Qt puts in the temporary directory, still one per vault and user.
    if (QFile::encodeName(m_socketPath).size() > 100)
        m_socketPath = QStringLiteral("icloud-notes-")
            + QString::fromLatin1(QCryptographicHash::hash(base.toUtf8(), QCryptographicHash::Sha1).toHex().left(16));
    connect(&m_server, &QLocalServer::newConnection, this, [this] {
        while (QLocalSocket *socket = m_server.nextPendingConnection()) {
            // One line: "show", then the activation token, if any.
            auto read = [this, socket] {
                if (!socket->canReadLine())
                    return;
                const QString line = QString::fromUtf8(socket->readLine()).trimmed();
                socket->disconnectFromServer();
                if (line == u"show" || line.startsWith(QStringLiteral("show ")))
                    emit activationRequested(line.mid(5));
            };
            connect(socket, &QLocalSocket::readyRead, this, read);
            connect(socket, &QLocalSocket::disconnected, socket, &QObject::deleteLater);
            read();
        }
    });
}

bool SingleInstance::claim()
{
    switch (m_lock.tryLock(QStringLiteral("Notes (pid %1)").arg(QCoreApplication::applicationPid()))) {
    case VaultLock::Locked:
        // Holding the lock, any socket file left there is a dead instance's.
        QLocalServer::removeServer(m_socketPath);
        m_server.setSocketOptions(QLocalServer::UserAccessOption);
        if (!m_server.listen(m_socketPath)) // still the one instance, just not one a relaunch can show
            qWarning("icloud-notes: cannot listen on %s: %s", qPrintable(m_socketPath),
                     qPrintable(m_server.errorString()));
        return true;
    case VaultLock::Failed:
        return true;
    case VaultLock::Busy:
        break;
    }
    QLocalSocket socket;
    socket.connectToServer(m_socketPath);
    if (socket.waitForConnected(2000)) {
        socket.write("show " + qgetenv("XDG_ACTIVATION_TOKEN") + '\n');
        socket.waitForBytesWritten(1000);
        socket.disconnectFromServer();
    }
    return false;
}
