// Console asserts shared by the tests: no framework in this repo by design.
// A non-zero exit means a regression. Also the scratch-vault file helpers.
#ifndef CHECK_H
#define CHECK_H

#include <QDir>
#include <QFile>
#include <QFileInfo>
#include <QList>
#include <QStringList>
#include <QTextStream>
#include <cstdlib>

namespace {
int failures = 0;

inline void check(bool cond, const char *name)
{
    QTextStream(stdout) << (cond ? "ok " : "FAIL ") << name << "\n";
    if (!cond)
        ++failures;
}

inline int report()
{
    QTextStream(stdout) << (failures ? "RESULT FAIL\n" : "RESULT OK\n");
    return failures ? EXIT_FAILURE : EXIT_SUCCESS;
}

// The vault the code under test uses: ICLOUD_NOTES_VAULT, which every test
// points at a scratch directory.
inline QString testVault()
{
    return qEnvironmentVariable("ICLOUD_NOTES_VAULT");
}

inline void writeFile(const QString &rel, const QString &content, const QString &root = testVault())
{
    const QString path = root + QLatin1Char('/') + rel;
    QDir().mkpath(QFileInfo(path).absolutePath());
    QFile f(path);
    if (f.open(QIODevice::WriteOnly | QIODevice::Truncate))
        f.write(content.toUtf8());
}

inline QString readFile(const QString &rel, const QString &root = testVault())
{
    QFile f(root + QLatin1Char('/') + rel);
    return f.open(QIODevice::ReadOnly) ? QString::fromUtf8(f.readAll()) : QString();
}

// A state file as icloud-notes-sync writes it (layout 4, in .icloud-notes/), which vault-info
// reads: the title mode, the default folder's directory (if any) and the
// tracked notes, {id, file[, read-only reason]}.
inline QString stateJson(const QString &mode, const QList<QStringList> &notes, const QString &defaultDir = {})
{
    QStringList entries;
    for (const QStringList &n : notes)
        entries << QStringLiteral(R"("%1":{"file":"%2","recordChangeTag":"t","modificationDate":0%3})")
                       .arg(n.at(0), n.at(1),
                            n.size() > 2 ? QStringLiteral(R"(,"unpublishableReason":"%1")").arg(n.at(2)) : QString());
    const QString folders = defaultDir.isEmpty()
        ? QString()
        : QStringLiteral(R"("folders":{"DefaultFolder-CloudKit":{"name":"%1","dirName":"%1"}},)").arg(defaultDir);
    return QStringLiteral(R"({"layoutVersion":4,"titleMode":"%1",%2"notes":{%3}})")
        .arg(mode, folders, entries.join(u','));
}
} // namespace

#endif
