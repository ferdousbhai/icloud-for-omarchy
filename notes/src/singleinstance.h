#ifndef SINGLEINSTANCE_H
#define SINGLEINSTANCE_H

#include <QLocalServer>
#include <QObject>

#include "vaultlock.h"

// One Notes window per vault: the first launch holds a lock and listens on
// a local socket beside it; a later launch finds the lock taken, asks the
// first to show itself over the socket, and exits. Two windows would each
// wait on the other's vault lock and edit the same notes unaware of each
// other.
class SingleInstance : public QObject
{
    Q_OBJECT

public:
    // base: a path prefix in the runtime directory, one per vault
    // (NotesBackend::lockPath() without its ".lock").
    explicit SingleInstance(const QString &base, QObject *parent = nullptr);

    // True when this process is the one instance (or the lock cannot be
    // opened at all, which is no reason to refuse to start). False when
    // another runs; it has been asked to show itself.
    bool claim();

signals:
    // A later launch asked this instance to show its window, passing on
    // the Wayland activation token its launcher gave it (may be empty).
    void activationRequested(const QString &token);

private:
    VaultLock m_lock;
    QString m_socketPath;
    QLocalServer m_server;
};

#endif
