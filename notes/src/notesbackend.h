#ifndef NOTESBACKEND_H
#define NOTESBACKEND_H

#include <QFileSystemWatcher>
#include <QHash>
#include <QObject>
#include <QProcess>
#include <QQuickTextDocument>

#include "markdownhighlighter.h"
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
    // An Apple ID already signed in to icloud-md on this machine, or empty.
    // With one, a missing vault is cloned without a sign-in window.
    Q_PROPERTY(QString savedAccount READ savedAccount NOTIFY foldersChanged)
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
    // Apple ended the saved session and icloud-md could not revive it on its
    // own; syncing pauses until runReauthenticate succeeds.
    Q_PROPERTY(bool authExpired READ authExpired NOTIFY authExpiredChanged)
    Q_PROPERTY(QVariantList statusEntries READ statusEntries NOTIFY pushPreviewChanged)
    Q_PROPERTY(int statusUnchanged READ statusUnchanged NOTIFY pushPreviewChanged)
    Q_PROPERTY(QStringList statusNotices READ statusNotices NOTIFY pushPreviewChanged)
    Q_PROPERTY(QString statusError READ statusError NOTIFY pushPreviewChanged)
    Q_PROPERTY(QVariantList historyEntries READ historyEntries NOTIFY historyChanged)
    Q_PROPERTY(QString diffText READ diffText NOTIFY historyChanged)
    Q_PROPERTY(QString historyError READ historyError NOTIFY historyChanged)
    Q_PROPERTY(double uiScale READ uiScale CONSTANT)
    Q_PROPERTY(QVariantMap theme READ theme NOTIFY themeChanged)
    Q_PROPERTY(QString iconFont READ iconFont CONSTANT)

public:
    explicit NotesBackend(QObject *parent = nullptr);

    QStringList folders() const { return m_folders; }
    QVariantMap folderNoteCounts() const { return m_folderNoteCounts; }
    bool cloned() const { return !stateDir().isEmpty(); }
    bool icloudMdAvailable() const;
    QString vaultTitleMode() const;
    QString savedAccount() const;
    QString currentFolder() const { return m_currentFolder; }
    void setCurrentFolder(const QString &folder);
    QStringList notes() const { return m_notes; }
    QVariantMap noteStates() const { return m_noteStates; }
    QVariantMap noteDetails() const { return m_noteDetails; }
    QString currentNote() const { return m_currentNote; }
    QString noteContent() const { return m_noteContent; }
    // The editable body: the file with its frontmatter envelope and, in
    // in-body vaults, its heading line held back (Notes never shows sync
    // metadata, and the title is edited in its own field). Both are
    // reattached on save.
    QString noteBody() const;
    QVariantList noteAttachments() const { return m_noteAttachments; }
    QString syncMessage() const { return m_syncMessage; }
    QString syncLog() const { return m_syncLog; }
    bool syncRunning() const { return m_syncRunning; }
    bool authExpired() const { return m_authExpired; }
    QVariantList statusEntries() const { return m_statusEntries; }
    int statusUnchanged() const { return m_statusUnchanged; }
    QStringList statusNotices() const { return m_statusNotices; }
    QString statusError() const { return m_statusError; }
    QVariantList historyEntries() const { return m_historyEntries; }
    QString diffText() const { return m_diffText; }
    QString historyError() const { return m_historyError; }
    double uiScale() const { return m_uiScale; }
    // The active Omarchy theme's resolved palette (accent, background,
    // foreground, muted, ...), empty off Omarchy so QML falls back to the
    // system palette. Follows theme changes live.
    QVariantMap theme() const { return m_theme; }
    // A Nerd Font family for toolbar glyphs, empty when none is installed.
    QString iconFont() const { return m_iconFont; }

    Q_INVOKABLE void refresh();
    Q_INVOKABLE void openNote(const QString &name);
    Q_INVOKABLE void saveCurrentNote(const QString &body);
    Q_INVOKABLE QString saveWarning(const QString &body);
    Q_INVOKABLE void newNote(const QString &name);
    Q_INVOKABLE QString deleteCurrentNote();
    Q_INVOKABLE QString renameCurrentNote(const QString &title);
    Q_INVOKABLE void newFolder(const QString &name);
    // Folders have no id upstream, so these do what a mv/rm on disk does:
    // a rename becomes a new Notes folder plus note moves, a delete sends
    // the notes to Recently Deleted; the old folder stays in Notes, empty.
    Q_INVOKABLE QString renameCurrentFolder(const QString &name);
    Q_INVOKABLE QString deleteCurrentFolder();
    Q_INVOKABLE QVariantList searchVault(const QString &query);
    Q_INVOKABLE QString toggleCheckbox(const QString &text, int line);
    Q_INVOKABLE QString exportPdf();
    // Clone the account's notes into the vault. With an account that is
    // already signed in here, no browser opens; without one, Apple's sign-in
    // window does, once per device.
    Q_INVOKABLE void runClone(const QString &account = QString());
    Q_INVOKABLE void runPull();
    Q_INVOKABLE void runPush();
    // Push whatever changed locally, then pull: the periodic sync, and what
    // launch does, so edits made while the app was closed or by another
    // program in any folder reach iCloud without a click.
    Q_INVOKABLE void runSync();
    // Opens Apple's sign-in window for the bound account (2FA is usually
    // skipped for a returning browser profile) and syncs once it succeeds.
    Q_INVOKABLE void runReauthenticate();
    Q_INVOKABLE void refreshPushPreview();
    Q_INVOKABLE void runHistory();
    Q_INVOKABLE void runDiff(const QString &ref);
    Q_INVOKABLE void clearLog();
    // Styles the editor's Markdown in the theme's colours; formatting only.
    Q_INVOKABLE void attachEditor(QQuickTextDocument *document);

signals:
    void foldersChanged();
    void currentFolderChanged();
    void notesChanged();
    void currentNoteChanged();
    void noteContentChanged();
    void currentNoteChangedOnDisk();
    // Files in the vault changed outside a sync (a save, another editor, a
    // note removed in a file manager): what automatic push acts on.
    void vaultChanged();
    void syncMessageChanged();
    void syncLogChanged();
    void syncRunningChanged();
    void authExpiredChanged();
    void pushPreviewChanged();
    void pushPreviewReady(bool ok);
    void historyChanged();
    void historyReady(bool ok);
    void cloneFinished(bool ok);
    void themeChanged();

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
    void loadTheme();
    QString assembleNote(const QString &body) const;
    MarkdownHighlighter::Colors highlighterColors() const;
    void appendLog(const QString &text);
    void setSyncMessage(const QString &text);

    // What one read of a note yields, kept until the file's mtime or size
    // moves, so a save in a folder of hundreds of notes re-reads one file.
    struct NoteScan {
        qint64 modifiedMs = 0;
        qint64 size = 0;
        QString mode;
        QString title;
        QString snippet;
        QString id;
        bool conflict = false;
        bool table = false;
    };
    QHash<QString, NoteScan> m_scans; // keyed by absolute path, current folder only

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
    bool m_pullAfterPush = false;
    bool m_authExpired = false;
    QByteArray m_captured;
    const double m_uiScale;
    QVariantMap m_theme;
    MarkdownHighlighter *m_highlighter = nullptr;
    QString m_iconFont;
    QFileSystemWatcher m_watcher;
    QFileSystemWatcher m_themeWatcher;
    QProcess m_syncProcess;
};

#endif
