#include "backgroundsync.h"
#include "notesbackend.h"
#include "vaultlock.h"

#include <QCoreApplication>
#include <QDir>
#include <QEventLoop>
#include <QHash>
#include <QStandardPaths>
#include <QTextStream>
#include <QTimer>
#include <functional>

namespace {

// Spin the event loop until cond holds: D-Bus answers and icloud-md's exit
// arrive through it. No timeout; the systemd unit bounds the run.
void waitFor(const std::function<bool()> &cond)
{
    QEventLoop loop;
    QTimer poll;
    QObject::connect(&poll, &QTimer::timeout, &loop, [&] {
        if (cond())
            loop.quit();
    });
    poll.start(50);
    if (!cond())
        loop.exec();
}

// A systemd user unit may not see the PATH a login shell sets up, where
// `npm install -g` (into ~/.local, say) or mise put icloud-md and node.
void findIcloudMd()
{
    if (!QStandardPaths::findExecutable(QStringLiteral("icloud-md")).isEmpty())
        return;
    const QString home = QDir::homePath();
    QStringList path = qEnvironmentVariable("PATH").split(u':', Qt::SkipEmptyParts);
    for (const char *dir : { "/.local/bin", "/.local/share/mise/shims", "/.npm-global/bin", "/.bun/bin" })
        if (!path.contains(home + QLatin1StringView(dir)))
            path << home + QLatin1StringView(dir);
    qputenv("PATH", path.join(u':').toLocal8Bit());
}

} // namespace

int runBackgroundSync(QTextStream &out)
{
    const QString vault = NotesBackend::rootPath();
    if (!NotesBackend::vaultCloned()) {
        out << "No notes cloned at " << vault << "; nothing to sync.\n";
        return 0;
    }
    // The backend's own lock: it checks it again before each icloud-md run.
    NotesBackend backend(nullptr, NotesBackend::Role::Background);
    switch (backend.lockVault()) {
    case VaultLock::Locked:
        break;
    case VaultLock::Busy:
        out << "Notes is open (it syncs on its own) or another sync is running (" << backend.lockHolder()
            << "); skipped.\n";
        return 0;
    case VaultLock::Failed:
        out << "Could not open the sync lock " << NotesBackend::lockPath() << "; not syncing.\n";
        return 1;
    }
    findIcloudMd();
    waitFor([&] { return !backend.signInPending(); });
    if (!backend.signInKnown()) {
        out << "icloud-session is not available, so the sign-in is unknown; skipped.\n";
        return 0;
    }
    if (!backend.signedIn() || backend.authExpired()) {
        out << "Not signed in to iCloud; skipped.\n";
        return 0;
    }
    if (!backend.icloudMdAvailable()) {
        out << "icloud-md not found on PATH; install it with: npm install -g icloud-md\n";
        return 1;
    }

    // The last run of each half counts: after a refused session that
    // icloud-session vouched for, the retry's.
    QHash<QString, bool> results;
    QObject::connect(&backend, &NotesBackend::syncFinished, &backend,
                     [&](const QString &label, bool ok) { results.insert(label, ok); });
    backend.runSync();
    waitFor([&] { return backend.idle(); });
    out << backend.syncLog() << '\n';
    if (backend.authExpired()) {
        out << "iCloud refused the sign-in; icloud-session was told. Sync paused until you sign in.\n";
        return 1;
    }
    const bool ok = results.value(QStringLiteral("Push")) && results.value(QStringLiteral("Pull"));
    out << (ok ? "Synced " : "Sync failed for ") << vault << ".\n";
    return ok ? 0 : 1;
}
