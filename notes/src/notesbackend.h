#ifndef NOTESBACKEND_H
#define NOTESBACKEND_H

#include <QFileSystemWatcher>
#include <QHash>
#include <QObject>
#include <QProcess>
#include <QQuickTextDocument>
#include <QSet>
#include <QTimer>

#include "markdownhighlighter.h"
#include "vaultlock.h"
#include <QStringList>
#include <QVariant>

class QDBusMessage;
class QDBusServiceWatcher;

// The vault on disk plus the icloud-notes-sync CLI, exposed to QML. One sync
// process runs at a time; its Mode says how the output is consumed.
class NotesBackend : public QObject
{
    Q_OBJECT
    Q_PROPERTY(QStringList folders READ folders NOTIFY foldersChanged)
    Q_PROPERTY(QVariantMap folderNoteCounts READ folderNoteCounts NOTIFY foldersChanged)
    Q_PROPERTY(bool cloned READ cloned NOTIFY foldersChanged)
    Q_PROPERTY(bool syncToolAvailable READ syncToolAvailable NOTIFY foldersChanged)
    Q_PROPERTY(QString vaultTitleMode READ vaultTitleMode NOTIFY foldersChanged)
    Q_PROPERTY(QString currentFolder READ currentFolder WRITE setCurrentFolder NOTIFY currentFolderChanged)
    Q_PROPERTY(QStringList notes READ notes NOTIFY notesChanged)
    Q_PROPERTY(QVariantMap noteStates READ noteStates NOTIFY notesChanged)
    Q_PROPERTY(QVariantMap noteDetails READ noteDetails NOTIFY notesChanged)
    Q_PROPERTY(QString currentNote READ currentNote NOTIFY currentNoteChanged)
    Q_PROPERTY(QString noteBody READ noteBody NOTIFY noteContentChanged)
    Q_PROPERTY(QVariantList noteAttachments READ noteAttachments NOTIFY noteContentChanged)
    Q_PROPERTY(QVariantList noteConflicts READ noteConflicts NOTIFY noteContentChanged)
    Q_PROPERTY(bool noteConflictsUnreadable READ noteConflictsUnreadable NOTIFY noteContentChanged)
    Q_PROPERTY(bool noteHasSyncedCopy READ noteHasSyncedCopy NOTIFY noteContentChanged)
    // Why the sync tool will never push the current note, or empty when it is
    // editable. A read-only note opens locked: edits could never sync.
    Q_PROPERTY(QString readOnlyReason READ readOnlyReason NOTIFY noteContentChanged)
    Q_PROPERTY(QString syncMessage READ syncMessage NOTIFY syncMessageChanged)
    Q_PROPERTY(QString syncLog READ syncLog NOTIFY syncLogChanged)
    Q_PROPERTY(bool syncRunning READ syncRunning NOTIFY syncRunningChanged)
    // The iCloud sign-in is gone (icloud-session says signed out, or
    // icloud-notes-sync was refused); syncing pauses until icloud-session reports
    // a sign-in again.
    Q_PROPERTY(bool authExpired READ authExpired NOTIFY authExpiredChanged)
    // The account as icloud-session (the D-Bus daemon that owns the Apple
    // sign-in for every iCloud app) reports it. signInKnown is false while
    // the daemon is missing or has not answered; signInPending while a read
    // of it is on the way.
    Q_PROPERTY(bool signInKnown READ signInKnown NOTIFY signInChanged)
    Q_PROPERTY(bool signInPending READ signInPending NOTIFY signInChanged)
    Q_PROPERTY(bool signedIn READ signedIn NOTIFY signInChanged)
    Q_PROPERTY(QString appleId READ appleId NOTIFY signInChanged)
    // icloud-session's sign-in window is open.
    Q_PROPERTY(bool signingIn READ signingIn NOTIFY signInChanged)
    // Whole days until a "Keep me signed in" sign-in lapses, -1 when there
    // is none (a phone QR sign-in, say, lasts only hours), or -2 while
    // unknown or signed out.
    Q_PROPERTY(int signInDaysLeft READ signInDaysLeft NOTIFY signInChanged)
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
    // App: the window's backend, which holds the vault's lock for its whole
    // lifetime (waiting, syncs deferred, while a background sync has it).
    // Background: `icloud-notes --sync`, which takes the lock with
    // lockVault() before syncing; no theme, fonts or desktop settings are
    // read. Cli: an `icloud-notes <command>` run, which takes the lock the
    // same way before it changes the vault or syncs, and reads
    // icloud-session only when asked (refreshSignIn). icloud-notes-sync
    // takes the same lock itself; the backend hands it the one it holds.
    enum class Role { App, Background, Cli };
    explicit NotesBackend(QObject *parent = nullptr, Role role = Role::App);
    ~NotesBackend() override;

    QStringList folders() const { return m_folders; }
    QVariantMap folderNoteCounts() const { return m_folderNoteCounts; }
    bool cloned() const { return vaultCloned(); }
    static bool vaultCloned();
    // The vault on disk (ICLOUD_NOTES_VAULT, or ~/Documents/icloud-notes).
    static QString rootPath();
    // The lock the app, background syncs, command line changes and
    // icloud-notes-sync share for the vault: $XDG_RUNTIME_DIR/
    // icloud-notes-<FNV-1a 64 of the vault's canonical path>.lock (see
    // notes-sync/src/cmd/lock.rs, which must agree).
    static QString lockPath();
    // Take the vault's lock now (the app does at start, and retries).
    VaultLock::Result lockVault();
    // Let it go again (a command line run done with the vault).
    void unlockVault() { m_lock.release(); }
    // Who holds the vault's lock, as it described itself ("another sync"
    // when it did not).
    QString lockHolder() const;
    // No sync running or waiting to run, and no icloud-session call or
    // read unanswered: what a background sync waits for before exiting.
    bool idle() const { return !m_syncRunning && m_sessionCalls == 0 && m_signInReads == 0; }
    // The sync engine to run: $ICLOUD_NOTES_SYNC_BIN when set (tests,
    // development; nothing else is tried then), else the packaged
    // /usr/lib/icloud-notes/icloud-notes-sync, else icloud-notes-sync on
    // PATH (a development build). Empty when none is executable.
    static QString syncToolPath();
    bool syncToolAvailable() const;
    QString vaultTitleMode() const;
    // The account's default folder ("Notes"), vault-relative; empty when
    // the sync tool's state does not say.
    QString defaultFolder() const;
    QString currentFolder() const { return m_currentFolder; }
    void setCurrentFolder(const QString &folder);
    QStringList notes() const { return m_notes; }
    QVariantMap noteStates() const { return m_noteStates; }
    QVariantMap noteDetails() const { return m_noteDetails; }
    QString currentNote() const { return m_currentNote; }
    QString noteContent() const { return m_noteContent; }
    // The editable body: the file with its frontmatter envelope held back
    // (Notes never shows sync metadata) and reattached on save. In in-body
    // vaults the title is simply the body's first line, as in Typora.
    QString noteBody() const;
    QVariantList noteAttachments() const { return m_noteAttachments; }
    // The open note's conflict blocks, for choosing between versions:
    // {local, remote: [{text, changed}], before, after: [context lines]}.
    QVariantList noteConflicts() const;
    // Conflict markers noteConflicts cannot read (nested, out of order or
    // left open), so there are no versions to pick from.
    bool noteConflictsUnreadable() const;
    // icloud-notes-sync keeps the note's last synced text (a base copy).
    bool noteHasSyncedCopy() const;
    QString readOnlyReason() const { return m_readOnlyReason; }
    QString syncMessage() const { return m_syncMessage; }
    QString syncLog() const { return m_syncLog; }
    bool syncRunning() const { return m_syncRunning; }
    bool authExpired() const { return m_authExpired; }
    bool signInKnown() const { return m_signInKnown; }
    bool signInPending() const { return m_signInReads > 0; }
    bool signedIn() const { return m_signInKnown && m_signedIn; }
    QString appleId() const { return m_appleId; }
    bool signingIn() const { return m_signingIn; }
    int signInDaysLeft() const;
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
    // False when nothing was written because the note changed on disk
    // since it was loaded, or is gone (no note open), or the write failed:
    // the edits stay unsaved. Also false while a sync runs, which may be
    // rewriting the note: the edits are written once it is done (merged
    // with whatever it changed), and queuedSaveWritten says so.
    Q_INVOKABLE bool saveCurrentNote(const QString &body);
    // The note changed on disk under unsaved editor text: rather than let
    // a save overwrite that change, merge the two (base is what the editor
    // loaded, mine what it holds now). Edits to different lines merge into
    // the note directly; lines both sides changed become conflict blocks
    // to pick from (noteConflicts). True when the merge was written; false
    // when there is nothing to merge (the disk holds base or mine).
    // A note that went away under the edits (a pull deleted, moved or
    // renamed it) is followed by its apple-note-id and merged there; one
    // deleted outright gets the edits as a new note, "<title> (unsaved
    // edits)", opened in its place (editsKeptAsNote says so).
    Q_INVOKABLE bool keepEditsAsConflict(const QString &base, const QString &mine);
    Q_INVOKABLE QString saveWarning(const QString &body);
    Q_INVOKABLE void newNote(const QString &name);
    Q_INVOKABLE QString deleteCurrentNote();
    Q_INVOKABLE QString renameCurrentNote(const QString &title);
    // Move the open note to another existing folder (vault-relative, ""
    // for the root), as a mv on disk does: the next push moves it in Notes.
    // Refused for a read-only note, one with attachments (their links are
    // relative to its folder) and a name the folder already has.
    Q_INVOKABLE QString moveCurrentNote(const QString &folder);
    // Keep one side of each conflict block ("local", "remote" or "both").
    Q_INVOKABLE QString resolveConflicts(const QStringList &choices);
    // A note with unreadable conflict markers: "strip" keeps every line
    // but the markers, "synced" goes back to the last synced text. The
    // file is first copied, byte for byte, under conflictBackupDir();
    // without that copy nothing is replaced. {ok, message, backup}.
    Q_INVOKABLE QVariantMap recoverConflictedNote(const QString &how);
    static QString conflictBackupDir();
    Q_INVOKABLE void newFolder(const QString &name);
    // Folders have no id upstream, so these do what a mv/rm on disk does:
    // a rename becomes a new Notes folder plus note moves, a delete sends
    // the notes to Recently Deleted; the old folder stays in Notes, empty.
    Q_INVOKABLE QString renameCurrentFolder(const QString &name);
    Q_INVOKABLE QString deleteCurrentFolder();
    Q_INVOKABLE QVariantList searchVault(const QString &query);
    Q_INVOKABLE QString toggleCheckbox(const QString &text, int line);
    Q_INVOKABLE QString exportPdf();
    // Clone the account's notes into the vault: signs in through
    // icloud-session first when needed, then clones its account (by dsid)
    // without icloud-notes-sync ever opening a window of its own.
    Q_INVOKABLE void runClone();
    Q_INVOKABLE void runPull();
    Q_INVOKABLE void runPush();
    // Push whatever changed locally, then pull: the periodic sync, and what
    // launch does, so edits made while the app was closed or by another
    // program in any folder reach iCloud without a click.
    Q_INVOKABLE void runSync();
    // Asks icloud-session to open its sign-in window; returns at once.
    // Syncing resumes when the daemon reports the sign-in.
    Q_INVOKABLE void signIn();
    Q_INVOKABLE void refreshPushPreview();
    Q_INVOKABLE void runHistory();
    Q_INVOKABLE void runDiff(const QString &ref);
    Q_INVOKABLE void clearLog();
    // Styles the editor's Markdown in the theme's colours; formatting only.
    Q_INVOKABLE void attachEditor(QQuickTextDocument *document);
    // Where the editor cursor is (-1 when it has no focus): Markdown marks
    // show on that line only.
    Q_INVOKABLE void setEditorCursor(int position);
    // Re-reads icloud-session's properties in the background (changes also
    // arrive on their own); keeps the day count current in an app left open.
    Q_INVOKABLE void refreshSignIn();

    // Runs of pull and push pass --json to icloud-notes-sync, so its result
    // is in lastOutput() (the command line's --json).
    void setToolJson(bool json) { m_toolJson = json; }
    // The last icloud-notes-sync run's exit code and stdout, as they were
    // when syncFinished was emitted for it.
    int lastExitCode() const { return m_lastExit; }
    QByteArray lastOutput() const { return m_captured; }

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
    // Any icloud-session property changed, or a read of them finished.
    void signInChanged();
    void pushPreviewChanged();
    void pushPreviewReady(bool ok);
    void historyChanged();
    void historyReady(bool ok);
    void cloneFinished(bool ok);
    // The open note was deleted elsewhere under unsaved edits, which were
    // written to a new note instead; message says where.
    void editsKeptAsNote(const QString &message);
    // A save refused while a sync ran was written after it (body as given).
    void queuedSaveWritten(const QString &body);
    // One icloud-notes-sync run ended ("Push", "Pull", "Clone", ...).
    void syncFinished(const QString &label, bool ok);
    // The last run of a chain ended and nothing follows it: after the pull
    // of runSync (or its push, when that found the sign-in gone), unlike
    // syncRunningChanged, which also flips between the two halves.
    void syncChainFinished();
    void themeChanged();

private:
    enum class Mode { Plain, Preview, History, Diff };

    QString folderAbsolutePath(const QString &folder) const;
    QString noteAbsolutePath() const;
    QString vaultRelative(const QString &name) const;
    // What `icloud-notes-sync vault-info` says about the vault, so the app
    // never parses the engine's state file: asked once, then again only
    // when the engine, the vault or the state file changes. Defaults (in-body,
    // nothing tracked) without an engine or a readable state.
    struct VaultInfo {
        QString titleMode = QStringLiteral("in-body");
        QString defaultFolderDir;
        QString stateFile;
        QSet<QString> tracked; // vault-relative files
        QHash<QString, QString> readOnly; // vault-relative file -> reason
        QHash<QString, QString> baseFiles; // note id -> absolute path of its last-synced body
    };
    const VaultInfo &vaultInfo() const;
    void startEngine();
    void rebuildFolders();
    void rebuildNotes();
    void classifyNotes();
    void loadCurrentNote();
    void closeNote();
    QString findNoteById(const QString &id) const;
    bool followNote(const QString &id);
    void loseCurrentNote();
    bool keepEditsAsNewNote(const QString &mine, const QString &why);
    QString syncedCopyPath() const;
    void writeQueuedSave();
    void rewatch();
    void startSync(Mode mode, const QStringList &args, const QString &label);
    void finishSync(int exitCode);
    void setPushPreview(const QVariantMap &parsed, const QString &error);
    void loadTheme();
    QString assembleNote(const QString &body) const;
    MarkdownHighlighter::Colors highlighterColors() const;
    void appendLog(const QString &text);
    void setSyncMessage(const QString &text);
    void setAuthExpired(bool expired);
    QDBusMessage sessionCall(const QString &method) const;
    void callSession(const QString &method);
    void applySignIn(const QVariantMap &properties);
    void continueClone();
    void resumeSync();
    void startProcess();
    void retryLock();

private slots:
    void sessionPropertiesChanged(const QString &interface, const QVariantMap &changed,
                                  const QStringList &invalidated);

private:

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
    // The open note as it was when its file went away (see loseCurrentNote).
    struct LostNote {
        QString folder;
        QString title;
        QString content;
        bool valid = false;
    } m_lostNote;
    bool m_following = false;
    bool m_merging = false; // keepEditsAsConflict is reloading its merge
    // A save asked for while a sync ran: the editor body, and the note it
    // was edited from.
    struct QueuedSave {
        QString base;
        QString body;
        bool valid = false;
    } m_queuedSave;
    QVariantList m_noteAttachments;
    QString m_readOnlyReason;
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
    // A sign-in came back while a sync ran: sync once it finishes.
    bool m_syncWhenIdle = false;
    // icloud-session's properties, as last read or announced.
    int m_signInReads = 0;
    bool m_signInKnown = false;
    bool m_signedIn = false;
    QString m_appleId;
    QString m_dsid;
    quint64 m_expiresAt = 0; // unix seconds, 0 = session-only or unknown
    bool m_signingIn = false;
    // runClone is waiting on a sign-in; m_cloneSignInAsked once SignIn() went out.
    bool m_cloneWanted = false;
    // Set while a sync retried after icloud-session vouched for the session,
    // so a second refusal pauses instead of retrying again.
    bool m_retriedAfterReport = false;
    bool m_cloneSignInAsked = false;
    QDBusServiceWatcher *m_sessionWatcher = nullptr;
    int m_sessionCalls = 0; // icloud-session method calls awaiting an answer
    const Role m_role;
    bool m_toolJson = false;
    int m_lastExit = 0;
    VaultLock m_lock;
    // Runs while someone else holds the lock: a sync asked for meanwhile
    // starts once it is free, never before.
    QTimer m_lockRetry;
    QByteArray m_captured; // the run's stdout: its --json result, or diff text
    QByteArray m_capturedErr; // its stderr: progress, warnings, errors
    const double m_uiScale;
    QVariantMap m_theme;
    MarkdownHighlighter *m_highlighter = nullptr;
    QString m_iconFont;
    QFileSystemWatcher m_watcher;
    QFileSystemWatcher m_themeWatcher;
    QProcess m_syncProcess;
    mutable VaultInfo m_vaultInfo;
    mutable QString m_vaultInfoKey;
};

#endif
