#ifndef VAULTLOCK_H
#define VAULTLOCK_H

#include <QFile>
#include <QString>

#include <cerrno>
#include <fcntl.h>
#include <sys/file.h>
#include <unistd.h>

// An exclusive flock on a lock file, held until release() or destruction.
// icloud-notes-sync has no lock of its own, so the app and `icloud-notes --sync`
// take this one before running it: the app for its whole lifetime, the
// background sync for its run. The file is never removed (removing a
// flock file races with the next locker); it holds only the holder's own
// description, for whoever waits, and lives in the runtime directory.
// Close-on-exec, so icloud-notes-sync never inherits it.
class VaultLock
{
public:
    enum Result { Locked, Busy, Failed };

    explicit VaultLock(const QString &path) : m_path(path) { }
    ~VaultLock() { release(); }
    VaultLock(const VaultLock &) = delete;
    VaultLock &operator=(const VaultLock &) = delete;

    // Never blocks: Busy while someone else holds it, Failed when the file
    // cannot be opened at all. The holder describes itself as owner
    // ("Notes (pid 12)"), which holder() reads back to whoever waits.
    Result tryLock(const QString &owner = {})
    {
        if (m_fd >= 0)
            return Locked;
        const int fd = ::open(QFile::encodeName(m_path).constData(), O_RDWR | O_CREAT | O_CLOEXEC, 0600);
        if (fd < 0)
            return Failed;
        if (::flock(fd, LOCK_EX | LOCK_NB) != 0) {
            const bool busy = errno == EWOULDBLOCK;
            ::close(fd);
            return busy ? Busy : Failed;
        }
        m_fd = fd;
        const QByteArray text = owner.toUtf8();
        if (::ftruncate(fd, 0) == 0 && !text.isEmpty()) {
            [[maybe_unused]] const ssize_t written = ::pwrite(fd, text.constData(), size_t(text.size()), 0);
        }
        return Locked;
    }

    void release()
    {
        if (m_fd >= 0) {
            [[maybe_unused]] const int cleared = ::ftruncate(m_fd, 0);
            ::close(m_fd);
        }
        m_fd = -1;
    }

    bool held() const { return m_fd >= 0; }
    QString path() const { return m_path; }
    // Who holds the lock, as it described itself; empty when nobody said.
    QString holder() const
    {
        QFile file(m_path);
        return file.open(QIODevice::ReadOnly) ? QString::fromUtf8(file.read(256)).trimmed() : QString();
    }

private:
    QString m_path;
    int m_fd = -1;
};

#endif
