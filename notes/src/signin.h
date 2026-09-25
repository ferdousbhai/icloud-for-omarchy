#pragma once

#include <QDateTime>
#include <QDirIterator>
#include <QSqlDatabase>
#include <QSqlQuery>
#include <QUuid>

// How long the iCloud sign-in lasts, read from the browser profile icloud-md
// signs in with. A "Keep me signed in" sign-in stores X-APPLE-WEBAUTH-TOKEN
// as a persistent cookie (30 days); any other sign-in keeps it for the
// browser session only, and it is gone once that window closes.
namespace SignIn {

// Chromium stores expiry as microseconds since 1601-01-01 UTC.
inline QDateTime fromChromiumTime(qint64 micros)
{
    static const QDateTime epoch(QDate(1601, 1, 1), QTime(0, 0), QTimeZone::UTC);
    return epoch.addMSecs(micros / 1000);
}

// When the persistent sign-in cookie in one Chromium cookie database
// expires; invalid when it has none or the database cannot be read.
inline QDateTime tokenExpiry(const QString &cookiesDb)
{
    QDateTime expiry;
    const QString connection = QUuid::createUuid().toString();
    {
        QSqlDatabase db = QSqlDatabase::addDatabase(QStringLiteral("QSQLITE"), connection);
        db.setDatabaseName(cookiesDb);
        db.setConnectOptions(QStringLiteral("QSQLITE_OPEN_READONLY"));
        if (db.open()) {
            QSqlQuery query(db);
            if (query.exec(QStringLiteral("SELECT MAX(expires_utc) FROM cookies WHERE host_key = '.icloud.com' "
                                          "AND name = 'X-APPLE-WEBAUTH-TOKEN' AND is_persistent = 1"))
                && query.next() && !query.value(0).isNull())
                expiry = fromChromiumTime(query.value(0).toLongLong());
            db.close();
        }
    }
    QSqlDatabase::removeDatabase(connection);
    return expiry;
}

// The latest such expiry across every account signed in on this machine.
inline QDateTime latestTokenExpiry(const QString &accountsDir)
{
    QDateTime latest;
    QDirIterator it(accountsDir, { QStringLiteral("Cookies") }, QDir::Files, QDirIterator::Subdirectories);
    while (it.hasNext()) {
        const QDateTime expiry = tokenExpiry(it.next());
        if (expiry.isValid() && (!latest.isValid() || expiry > latest))
            latest = expiry;
    }
    return latest;
}

} // namespace SignIn
