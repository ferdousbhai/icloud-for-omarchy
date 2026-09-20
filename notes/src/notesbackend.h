#ifndef NOTESBACKEND_H
#define NOTESBACKEND_H

#include <QFileSystemWatcher>
#include <QObject>
#include <QProcess>
#include <QStringList>
#include <QVariant>

// The vault on disk plus the icloud-md CLI, exposed to QML. One sync
// process runs at a time; its Mode says how the output is consumed.
class NotesBackend : public QObject
{
    Q_OBJECT
    Q_PROPERTY(QStringList folders READ folders NOTIFY foldersChanged)
    Q_PROPERTY(QVariantMap folderNoteCounts READ folderNoteCounts NOTIFY foldersChanged)
    Q_PROPERTY(bool cloned READ cloned NOTIFY foldersChanged)
    Q_PROPERTY(bool icloudMdAvailable READ icloudMdAvailable NOTIFY foldersChanged)
    Q_PROPERTY(QString vaultTitleMode READ vaultTitleMode NOTIFY foldersChanged)
    Q_PROPERTY(QString currentFolder READ currentFolder WRITE setCurrentFolder NOTIFY currentFolderChanged)
    Q_PROPERTY(QStringList notes READ notes NOTIFY notesChanged)
    Q_PROPERTY(QVariantMap noteStates READ noteStates NOTIFY notesChanged)
    Q_PROPERTY(QVariantMap noteDetails READ noteDetails NOTIFY notesChanged)
    Q_PROPERTY(QString currentNote READ currentNote NOTIFY currentNoteChanged)
    Q_PROPERTY(QString noteBody READ noteBody NOTIFY noteContentChanged)
    Q_PROPERTY(QVariantList noteAttachments READ noteAttachments NOTIFY noteContentChanged)
    Q_PROPERTY(QString syncMessage READ syncMessage NOTIFY syncMessageChanged)
    Q_PROPERTY(QString syncLog READ syncLog NOTIFY syncLogChanged)
    Q_PROPERTY(bool syncRunning READ syncRunning NOTIFY syncRunningChanged)
    Q_PROPERTY(QVariantList statusEntries READ statusEntries NOTIFY pushPreviewChanged)
    Q_PROPERTY(int statusUnchanged READ statusUnchanged NOTIFY pushPreviewChanged)
    Q_PROPERTY(QStringList statusNotices READ statusNotices NOTIFY pushPreviewChanged)
    Q_PROPERTY(QString statusError READ statusError NOTIFY pushPreviewChanged)
    Q_PROPERTY(QVariantList historyEntries READ historyEntries NOTIFY historyChanged)
    Q_PROPERTY(QString diffText READ diffText NOTIFY historyChanged)
    Q_PROPERTY(QString historyError READ historyError NOTIFY historyChanged)
    Q_PROPERTY(double uiScale READ uiScale CONSTANT)

public:
    explicit NotesBackend(QObject *parent = nullptr);

    QStringList folders() const { return m_folders; }
    QVariantMap folderNoteCounts() const { return m_folderNoteCounts; }
    bool cloned() const { return !stateDir().isEmpty(); }
    bool icloudMdAvailable() const;
    QString vaultTitleMode() const;
    QString currentFolder() const { return m_currentFolder; }
    void setCurrentFolder(const QString &folder);
    QStringList notes() const { return m_notes; }
    QVariantMap noteStates() const { return m_noteStates; }
    QVariantMap noteDetails() const { return m_noteDetails; }
    QString currentNote() const { return m_currentNote; }
    QString noteContent() const { return m_noteContent; }
    // The editable body: the file with its frontmatter envelope held back
    // (Apple Notes never shows sync metadata; it is reattached on save).
    QString noteBody() const;
    QVariantList noteAttachments() const { return m_noteAttachments; }
    QString syncMessage() const { return m_syncMessage; }
    QString syncLog() const { return m_syncLog; }
    bool syncRunning() const { return m_syncRunning; }
    QVariantList statusEntries() const { return m_statusEntries; }
    int statusUnchanged() const { return m_statusUnchanged; }
    QStringList statusNotices() const { return m_statusNotices; }
    QString statusError() const { return m_statusError; }
    QVariantList historyEntries() const { return m_historyEntries; }
    QString diffText() const { return m_diffText; }
    QString historyError() const { return m_historyError; }
    double uiScale() const { return m_uiScale; }

    Q_INVOKABLE void refresh();
    Q_INVOKABLE void openNote(const QString &name);
    Q_INVOKABLE void saveCurrentNote(const QString &body);
    Q_INVOKABLE QString saveWarning(const QString &body);
    Q_INVOKABLE void newNote(const QString &name);
    Q_INVOKABLE void deleteCurrentNote();
    Q_INVOKABLE QString renameCurrentNote(const QString &title);
    Q_INVOKABLE void newFolder(const QString &name);
    Q_INVOKABLE QVariantList searchVault(const QString &query);
    Q_INVOKABLE QString toggleCheckbox(const QString &text, int line);
    Q_INVOKABLE QString exportPdf();
    Q_INVOKABLE void runClone(const QString &titleMode);
    Q_INVOKABLE void runPull();
    Q_INVOKABLE void runPush();
    Q_INVOKABLE void refreshPushPreview();
    Q_INVOKABLE void runHistory();
    Q_INVOKABLE void runDiff(const QString &ref);
    Q_INVOKABLE void clearLog();

signals:
    void foldersChanged();
    void currentFolderChanged();
    void notesChanged();
    void currentNoteChanged();
    void noteContentChanged();
    void currentNoteChangedOnDisk();
    void syncMessageChanged();
    void syncLogChanged();
    void syncRunningChanged();
    void pushPreviewChanged();
    void pushPreviewReady(bool ok);
    void historyChanged();
    void historyReady(bool ok);

private:
    enum class Mode { Plain, Preview, History, Diff };

    static QString rootPath();
    QString folderAbsolutePath(const QString &folder) const;
    QString noteAbsolutePath() const;
    QString vaultRelative(const QString &name) const;
    QString stateDir() const;
    QByteArray stateJson() const;
    void rebuildFolders();
    void rebuildNotes();
    void classifyNotes();
    void loadCurrentNote();
    void closeNote();
    void rewatch();
    void startSync(Mode mode, const QStringList &args, const QString &label);
    void finishSync(int exitCode);
    void setPushPreview(const QVariantMap &parsed, const QString &error);
    void appendLog(const QString &text);
    void setSyncMessage(const QString &text);

    QStringList m_folders;
    QVariantMap m_folderNoteCounts;
    QString m_currentFolder;
    QStringList m_notes;
    QVariantMap m_noteStates;
    QVariantMap m_noteDetails;
    QString m_currentNote;
    QString m_noteContent;
    QVariantList m_noteAttachments;
    QVariantList m_statusEntries;
    int m_statusUnchanged = 0;
    QStringList m_statusNotices;
    QString m_statusError;
    QVariantList m_historyEntries;
    QString m_diffText;
    QString m_historyError;
    QString m_syncMessage;
    QString m_syncLog;
    QString m_syncLabel;
    bool m_syncRunning = false;
    Mode m_mode = Mode::Plain;
    QByteArray m_captured;
    const double m_uiScale;
    QFileSystemWatcher m_watcher;
    QProcess m_syncProcess;
};

#endif
