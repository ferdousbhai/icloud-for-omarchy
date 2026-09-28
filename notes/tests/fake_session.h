// A stand-in for the icloud-session daemon: the same bus name, object path,
// interface, properties and methods, on its own connection to the private
// session bus bin/test starts. It counts the calls Notes makes and announces
// property changes with PropertiesChanged, as the real daemon does.
#ifndef FAKE_SESSION_H
#define FAKE_SESSION_H

#include <QDBusConnection>
#include <QDBusMessage>
#include <QObject>
#include <QStringList>
#include <QVariantMap>

class FakeSession : public QObject
{
    Q_OBJECT
    Q_CLASSINFO("D-Bus Interface", "io.github.ferdousbhai.ICloudSession")
    Q_PROPERTY(bool SignedIn MEMBER signedIn)
    Q_PROPERTY(QString AppleId MEMBER appleId)
    Q_PROPERTY(QString Dsid MEMBER dsid)
    Q_PROPERTY(qulonglong ExpiresAt MEMBER expiresAt)
    Q_PROPERTY(bool SigningIn MEMBER signingIn)

public:
    static inline const QString service = QStringLiteral("io.github.ferdousbhai.ICloudSession");
    static inline const QString path = QStringLiteral("/io/github/ferdousbhai/ICloudSession");

    explicit FakeSession(const QDBusConnection &bus) : m_bus(bus)
    {
        m_bus.registerObject(path, this, QDBusConnection::ExportAllProperties | QDBusConnection::ExportAllSlots);
    }

    bool claimName() { return m_bus.registerService(service); }
    bool releaseName() { return m_bus.unregisterService(service); }

    // Change properties and announce them, as the daemon does.
    void set(const QVariantMap &changed)
    {
        for (auto it = changed.cbegin(); it != changed.cend(); ++it)
            setProperty(it.key().toUtf8().constData(), it.value());
        QDBusMessage signal = QDBusMessage::createSignal(path, QStringLiteral("org.freedesktop.DBus.Properties"),
                                                         QStringLiteral("PropertiesChanged"));
        signal << service << changed << QStringList();
        m_bus.send(signal);
    }

    bool signedIn = false;
    QString appleId;
    QString dsid;
    qulonglong expiresAt = 0;
    bool signingIn = false;
    int signInCalls = 0;
    int reportCalls = 0;

public slots:
    void SignIn() { ++signInCalls; }
    void ReportSignInRequired() { ++reportCalls; }

private:
    QDBusConnection m_bus;
};

#endif
