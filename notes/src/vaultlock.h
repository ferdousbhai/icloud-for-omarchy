#ifndef VAULTLOCK_H
#define VAULTLOCK_H

#include <QFile>
#include <QString>

#include <cerrno>
#include <fcntl.h>
#include <sys/file.h>
#include <unistd.h>

// An exclusive flock on a lock file, held until release() or destruction.
// icloud-md has no lock of its own, so the app and `icloud-notes --sync`
// take this one before running it: the app for its whole lifetime, the
// background sync for its run. The file is never removed (removing a
// flock file races with the next locker); it is empty and lives in the
// runtime directory. Close-on-exec, so icloud-md never inherits it.
class VaultLock
{
public:
    enum Result { Locked, Busy, Failed };

    explicit VaultLock(const QString &path) : m_path(path) { }
    ~VaultLock() { release(); }
    VaultLock(const VaultLock &) = delete;
    VaultLock &operator=(const VaultLock &) = delete;

    // Never blocks: Busy while someone else holds it, Failed when the file
    // cannot be opened at all.
    Result tryLock()
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
        return Locked;
    }

    void release()
    {
        if (m_fd >= 0)
            ::close(m_fd);
        m_fd = -1;
    }

    bool held() const { return m_fd >= 0; }
    QString path() const { return m_path; }

private:
    QString m_path;
    int m_fd = -1;
};

#endif
