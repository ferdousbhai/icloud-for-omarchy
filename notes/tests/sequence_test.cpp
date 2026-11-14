// Sequence test: seeded, bounded random sequences of what a person and the
// sync engine do, run against the real window (main.qml, offscreen) and the
// real backend, with the stub engine, in a scratch vault. A person types,
// autosave ticks, Ctrl+S, switches notes, focuses the editor after a while
// (a pull first), resolves a conflict (keeping both), answers the
// mass-delete banner and closes the window (onClosing, then the backend's
// destructor, as main() tears down). The engine runs syncs and pushes that
// the test holds open (ICLOUD_NOTES_SYNC_STUB_GATE) while it changes, moves
// or deletes the open note or another one; other programs change notes and
// Syncthing drops conflict copies.
//
// Every line either side writes carries a token (U<n>Z typed, R<n>Z from
// iCloud or another program), tied to the note it was written into. After
// every step: no file holds a token of another note; every conflict block
// has new lines on both sides (each side's own); no conflict markers are
// nested; and the engine ran with --yes no more often than the sequence
// confirmed the deletion. Whenever nothing is unsaved, and after the close,
// every token is on disk: in its note wherever it moved, or in that note's
// "(unsaved edits)" note (a note deleted in iCloud takes the lines it held
// on disk with it). A failure prints the seed and the steps.
//
// ICLOUD_NOTES_SEQUENCE_CASES=N runs N seeds (default 12);
// ICLOUD_NOTES_SEQUENCE_SEED=S runs seed S alone; ICLOUD_NOTES_SEQUENCE_STEPS
// sets the length (default 28); ICLOUD_NOTES_SEQUENCE_VERBOSE prints every
// sequence, not only a failing one.
#include "../src/notesbackend.h"
#include "../src/syncmodel.h"
#include "../src/vaultlock.h"
#include "check.h"

#include <QDirIterator>
#include <QElapsedTimer>
#include <QGuiApplication>
#include <QQmlApplicationEngine>
#include <QQmlContext>
#include <QQmlExpression>
#include <QQuickStyle>
#include <QQuickWindow>
#include <QRegularExpression>
#include <QStandardPaths>
#include <QTemporaryDir>
#include <QThread>
#include <functional>
#include <memory>

namespace {

// xorshift64*: small, seeded, the same sequence on every machine.
struct Rng {
    quint64 state;
    quint64 next()
    {
        state ^= state >> 12;
        state ^= state << 25;
        state ^= state >> 27;
        return state * 0x2545F4914F6CDD1DULL;
    }
    int below(int n) { return int(next() % quint64(n)); }
};

void settle(int ms)
{
    QElapsedTimer timer;
    timer.start();
    while (timer.elapsed() < ms) {
        QCoreApplication::processEvents(QEventLoop::AllEvents, 5);
        QThread::msleep(2);
    }
}

bool waitUntil(const std::function<bool()> &cond, int timeoutMs = 10000)
{
    QElapsedTimer timer;
    timer.start();
    while (!cond() && timer.elapsed() < timeoutMs) {
        QCoreApplication::processEvents(QEventLoop::AllEvents, 10);
        QThread::msleep(2);
    }
    return cond();
}

const QRegularExpression &tokenRe()
{
    static const QRegularExpression re(QStringLiteral("[UR][0-9]+Z"));
    return re;
}

QSet<QString> tokensIn(const QString &text)
{
    QSet<QString> out;
    for (const QRegularExpressionMatch &m : tokenRe().globalMatch(text))
        out.insert(m.captured());
    return out;
}

struct Note {
    QString id;
    QString title;
    QString file; // vault-relative; empty once deleted
};

class Sequence
{
public:
    Sequence(quint64 seed, const QString &dir, int steps) : m_rng{ seed * 0x9E3779B97F4A7C15ULL + 1 }, m_seed(seed), m_dir(dir), m_steps(steps) {}

    bool run()
    {
        setUp();
        bool ok = open();
        for (int i = 0; ok && i < m_steps && !m_closed; ++i) {
            step(i, i + 1 == m_steps);
            if (!gateHeld())
                waitIdle();
            ok = checkAlways() && (!quiescent() || checkAllOnDisk(false));
        }
        if (ok && !m_closed)
            ok = closeWindow();
        if (ok)
            ok = checkAlways() && checkAllOnDisk(true) && m_failure.isEmpty();
        if (!ok || qEnvironmentVariableIsSet("ICLOUD_NOTES_SEQUENCE_VERBOSE"))
            dump();
        tearDown();
        return ok;
    }

    // ---- The vault and the window (also what the named scenarios below use)
    void setUp()
    {
        m_vault = m_dir + QStringLiteral("/vault");
        qputenv("ICLOUD_NOTES_VAULT", m_vault.toUtf8());
        m_gate = m_dir + QStringLiteral("/gate");
        qputenv("ICLOUD_NOTES_SYNC_STUB_GATE", m_gate.toUtf8());
        m_engineLog = m_dir + QStringLiteral("/engine.log");
        qputenv("ICLOUD_NOTES_SYNC_STUB_LOG", m_engineLog.toUtf8());
        qunsetenv("ICLOUD_NOTES_SYNC_STUB_TOO_MANY");
        m_notes = { { QStringLiteral("n1"), QStringLiteral("Note-1"), QStringLiteral("Notes/One.md") },
                    { QStringLiteral("n2"), QStringLiteral("Note-2"), QStringLiteral("Notes/Two.md") },
                    { QStringLiteral("n3"), QStringLiteral("Note-3"), QStringLiteral("Work/Three.md") } };
        for (const Note &n : m_notes)
            writeFile(n.file, QStringLiteral("---\napple-note-id: %1\n---\n# %2\nfirst\n\nlast\n").arg(n.id, n.title));
        writeState();
    }

    void writeState()
    {
        QList<QStringList> tracked;
        for (const Note &n : m_notes)
            if (!n.file.isEmpty())
                tracked << QStringList{ n.id, n.file };
        writeFile(QStringLiteral(".icloud-notes/state.json"), stateJson(QStringLiteral("in-body"), tracked, QStringLiteral("Notes")));
    }

    bool open()
    {
        m_backend = std::make_unique<NotesBackend>();
        m_engine = std::make_unique<QQmlApplicationEngine>();
        m_engine->rootContext()->setContextProperty(QStringLiteral("backend"), m_backend.get());
        m_engine->load(QUrl::fromLocalFile(QStringLiteral(NOTES_QML)));
        m_window = m_engine->rootObjects().isEmpty() ? nullptr : qobject_cast<QQuickWindow *>(m_engine->rootObjects().constFirst());
        if (!m_window)
            return fail(QStringLiteral("main.qml did not load"));
        // The launch sync (sign-in unknown on the private bus: it syncs).
        if (!waitUntil([&] { return js(QStringLiteral("root.started")).toBool() && !m_backend->syncRunning(); }))
            return fail(QStringLiteral("the window never finished its first sync"));
        // Timers fire only as steps: nothing runs on its own between them.
        js(QStringLiteral("autosave.interval = 1000000000, autoPush.interval = 1000000000, poll.interval = 1000000000"));
        js(QStringLiteral("root.openNote('Notes', 'One.md')"));
        waitIdle();
        record(QStringLiteral("open Notes/One.md"));
        return m_failure.isEmpty();
    }

    void tearDown()
    {
        QFile::remove(m_gate);
        m_engine.reset();
        m_backend.reset();
        // A killed stub's last sleep may still hold the lock it inherited.
        waitUntil([] { return VaultLock(NotesBackend::lockPath()).tryLock() == VaultLock::Locked; });
        qunsetenv("ICLOUD_NOTES_SYNC_STUB_TOO_MANY");
    }

    // Runs `code` in main.qml's own context (its ids and functions).
    QVariant js(const QString &code)
    {
        if (!m_window)
            return {};
        QQmlExpression expression(qmlContext(m_window), m_window, QStringLiteral("(function () { return %1; })()").arg(code));
        const QVariant value = expression.evaluate();
        if (expression.hasError())
            fail(QStringLiteral("QML error in `%1`: %2").arg(code, expression.error().toString()));
        return value;
    }

    bool gateHeld() const { return QFile::exists(m_gate); }
    void waitIdle()
    {
        // Settled: no sync running for a moment (one may chain another).
        for (int i = 0; m_backend && i < 3; ++i) {
            waitUntil([&] { return !m_backend->syncRunning(); });
            settle(25);
        }
    }
    bool quiescent()
    {
        return !m_closed && !gateHeld() && !m_backend->syncRunning() && !js(QStringLiteral("root.dirty")).toBool();
    }

    // ---- Who wrote what where
    QString identityOfOpen() const
    {
        const QString rel = m_backend->currentFolder().isEmpty() ? m_backend->currentNote()
                                                                 : m_backend->currentFolder() + u'/' + m_backend->currentNote();
        const QString id = SyncModel::extractNoteId(m_backend->noteContent());
        return !id.isEmpty() && !SyncModel::isSyncConflictName(rel) ? id : QStringLiteral("file:") + rel;
    }
    QString titleOf(const QString &id) const
    {
        for (const Note &n : m_notes)
            if (n.id == id)
                return n.title;
        return {};
    }
    // Where a token of `identity` may be: its note (by id, never a
    // Syncthing copy of it), or that note's "(unsaved edits)" note; a file
    // of its own (an unsaved-edits note, a copy) holds only its own.
    bool allowed(const QString &identity, const QString &rel, const QString &content) const
    {
        if (identity.startsWith(u"file:"))
            return rel == identity.mid(5);
        const QString id = SyncModel::extractNoteId(content);
        if (!SyncModel::isSyncConflictName(rel) && id == identity)
            return true;
        return id.isEmpty() && rel.contains(QStringLiteral("(unsaved edits"))
            && SyncModel::splitEnvelope(content).body.section(u'\n', 0, 0) == QStringLiteral("# ") + titleOf(identity);
    }
    QHash<QString, QString> vaultFiles() const
    {
        QHash<QString, QString> files;
        const QDir root(m_vault);
        QDirIterator it(m_vault, { QStringLiteral("*.md") }, QDir::Files, QDirIterator::Subdirectories);
        while (it.hasNext()) {
            const QString rel = root.relativeFilePath(it.next());
            if (!rel.startsWith(u'.'))
                files.insert(rel, readFile(rel));
        }
        return files;
    }

    bool checkAlways()
    {
        if (!m_failure.isEmpty())
            return false;
        const QHash<QString, QString> files = vaultFiles();
        for (auto it = files.cbegin(); it != files.cend(); ++it) {
            for (const QString &token : tokensIn(it.value())) {
                if (!m_tokens.contains(token))
                    return fail(QStringLiteral("%1 holds %2, which nobody wrote").arg(it.key(), token));
                if (!allowed(m_tokens.value(token), it.key(), it.value()))
                    return fail(QStringLiteral("%1 holds %2, written into %3").arg(it.key(), token, m_tokens.value(token)));
            }
            if (SyncModel::hasUnreadableConflicts(it.value()))
                return fail(QStringLiteral("%1 has nested or broken conflict markers").arg(it.key()));
            for (const SyncModel::ConflictHunk &hunk : SyncModel::parseConflicts(it.value())) {
                const QSet<QString> local = tokensIn(hunk.local.join(u'\n')), remote = tokensIn(hunk.remote.join(u'\n'));
                const QSet<QString> mine = local - remote, theirs = remote - local;
                const auto all = [](const QSet<QString> &set, QChar kind) {
                    return std::all_of(set.cbegin(), set.cend(), [&](const QString &t) { return t.front() == kind; });
                };
                if (mine.isEmpty() || theirs.isEmpty() || !all(mine, u'U') || !all(theirs, u'R'))
                    return fail(QStringLiteral("%1 has a conflict block that is not both sides' own change: local %2, remote %3")
                                    .arg(it.key(), QStringList(local.values()).join(u','), QStringList(remote.values()).join(u',')));
            }
        }
        const int yes = readFile(QStringLiteral("engine.log"), m_dir).count(QStringLiteral("--yes"));
        if (yes > m_confirmations)
            return fail(QStringLiteral("the engine ran with --yes %1 time(s), confirmed %2").arg(yes).arg(m_confirmations));
        return true;
    }

    bool checkAllOnDisk(bool final)
    {
        const QHash<QString, QString> files = vaultFiles();
        for (auto it = m_tokens.cbegin(); it != m_tokens.cend(); ++it) {
            if (m_forgiven.contains(it.key()))
                continue;
            bool found = false;
            for (auto f = files.cbegin(); !found && f != files.cend(); ++f)
                found = tokensIn(f.value()).contains(it.key()) && allowed(it.value(), f.key(), f.value());
            if (!found)
                return fail(QStringLiteral("%1 (written into %2) is not on disk %3")
                                .arg(it.key(), it.value(), final ? QStringLiteral("after the close") : QStringLiteral("with nothing unsaved")));
        }
        return true;
    }

    // ---- Steps
    void record(const QString &what) { m_log << what; }
    bool fail(const QString &why)
    {
        if (m_failure.isEmpty())
            m_failure = why;
        return false;
    }

    Note *pickNote(bool preferOpen)
    {
        QList<Note *> present;
        for (Note &n : m_notes)
            if (!n.file.isEmpty())
                present << &n;
        if (present.isEmpty())
            return nullptr;
        if (preferOpen && m_rng.below(4) != 0)
            for (Note *n : present)
                if (identityOfOpen() == n->id)
                    return n;
        return present.at(m_rng.below(int(present.size())));
    }
    // A line from iCloud or another program, after the title or at the end.
    void remoteLine(Note &n, const QString &who)
    {
        const QString token = QStringLiteral("R%1Z").arg(++m_counter);
        m_tokens.insert(token, n.id);
        QString text = readFile(n.file);
        const SyncModel::EnvelopeSplit split = SyncModel::splitEnvelope(text);
        const bool top = m_rng.below(2) == 0;
        if (top) {
            const qsizetype nl = split.body.indexOf(u'\n');
            text = split.envelope + split.body.left(nl + 1) + token + u'\n' + split.body.mid(nl + 1);
        } else {
            text += (text.endsWith(u'\n') ? QString() : QStringLiteral("\n")) + token + u'\n';
        }
        writeFile(n.file, text);
        record(QStringLiteral("%1 adds %2 %3 %4").arg(who, token, top ? QStringLiteral("after the title of") : QStringLiteral("at the end of"), n.file));
    }

    enum Action { Type, Autosave, CtrlS, SyncStart, EngineChange, EngineMove, EngineDelete, SyncEnd, Switch, Stale, Focus,
                  External, Copy, Resolve, FilesGone, FilesBack, Confirm, Reject, Close };

    void step(int index, bool last)
    {
        if (last) {
            closeWindow();
            return;
        }
        const bool syncing = m_backend->syncRunning();
        const bool held = gateHeld();
        const bool refused = !m_backend->deletionsRefused().isEmpty();
        QList<std::pair<Action, int>> weights{ { Type, 8 }, { Autosave, 4 }, { CtrlS, 1 }, { Switch, 2 }, { Stale, 1 }, { Focus, 1 },
                                               { Copy, 1 } };
        if (index >= m_steps / 2)
            weights << std::pair{ Close, 1 };
        if (!held)
            weights << std::pair{ SyncStart, 4 } << std::pair{ External, 1 };
        else
            weights << std::pair{ SyncEnd, 3 };
        if (syncing && held)
            weights << std::pair{ EngineChange, 4 } << std::pair{ EngineMove, 1 } << std::pair{ EngineDelete, 1 };
        if (js(QStringLiteral("root.resolving")).toBool() && !syncing)
            weights << std::pair{ Resolve, 4 };
        weights << std::pair{ m_tooMany ? FilesBack : FilesGone, 1 };
        if (refused && !syncing)
            weights << std::pair{ Confirm, 1 } << std::pair{ Reject, 1 };
        int total = 0;
        for (const auto &w : weights)
            total += w.second;
        int roll = m_rng.below(total);
        Action action = weights.first().first;
        for (const auto &w : weights) {
            if (roll < w.second) {
                action = w.first;
                break;
            }
            roll -= w.second;
        }

        switch (action) {
        case Type: {
            if (m_backend->currentNote().isEmpty() || js(QStringLiteral("editor.readOnly || root.resolving")).toBool()) {
                record(QStringLiteral("(nothing to type into)"));
                return;
            }
            // The first key into an untouched note after a while pulls
            // first, and is dropped (the editor's Keys.onPressed).
            if (js(QStringLiteral("!root.dirty && root.pullIfStale()")).toBool()) {
                record(QStringLiteral("a key starts a pull first (dropped)"));
                return;
            }
            const QString token = QStringLiteral("U%1Z").arg(++m_counter);
            m_tokens.insert(token, identityOfOpen());
            const bool top = m_rng.below(3) == 0;
            js(top ? QStringLiteral("editor.insert(editor.text.indexOf('\\n') + 1, '%1\\n')").arg(token)
                   : QStringLiteral("editor.insert(editor.length, (editor.text.endsWith('\\n') || editor.length === 0 ? '' : '\\n') + '%1\\n')").arg(token));
            record(QStringLiteral("type %1 %2 (%3)").arg(token, top ? QStringLiteral("after the title") : QStringLiteral("at the end"), m_tokens.value(token)));
            return;
        }
        case Autosave:
            js(QStringLiteral("autosave.triggered()"));
            record(QStringLiteral("autosave tick"));
            return;
        case CtrlS:
            js(QStringLiteral("root.save()"));
            if (js(QStringLiteral("saveWarnDialog.visible")).toBool()) {
                record(QStringLiteral("Ctrl+S asks \"Save anyway?\": no"));
                js(QStringLiteral("saveWarnDialog.reject()"));
            } else {
                record(QStringLiteral("Ctrl+S"));
            }
            return;
        case SyncStart: {
            writeFile(QStringLiteral("gate"), QString(), m_dir);
            const int how = m_rng.below(3);
            js(how == 0 ? QStringLiteral("root.syncNow()") : how == 1 ? QStringLiteral("poll.triggered()") : QStringLiteral("autoPush.triggered()"));
            settle(20);
            record(QStringLiteral("%1 (held open): %2").arg(how == 0 ? QStringLiteral("sync") : how == 1 ? QStringLiteral("poll tick") : QStringLiteral("push tick"),
                                                            m_backend->syncRunning() ? QStringLiteral("running") : QStringLiteral("did not start")));
            return;
        }
        case EngineChange: {
            if (Note *n = pickNote(true))
                remoteLine(*n, QStringLiteral("the sync"));
            return;
        }
        case EngineMove: {
            Note *n = pickNote(true);
            if (!n)
                return;
            const QString target = (n->file.startsWith(u"Notes/") ? QStringLiteral("Work/") : QStringLiteral("Notes/")) + n->file.section(u'/', -1);
            QDir().mkpath(QFileInfo(m_vault + u'/' + target).absolutePath());
            QFile::rename(m_vault + u'/' + n->file, m_vault + u'/' + target);
            record(QStringLiteral("the sync moves %1 to %2").arg(n->file, target));
            n->file = target;
            writeState();
            return;
        }
        case EngineDelete: {
            Note *n = pickNote(true);
            if (!n)
                return;
            // A note deleted in iCloud takes the lines it held on disk with it.
            for (const QString &token : tokensIn(readFile(n->file)))
                m_forgiven.insert(token);
            QFile::remove(m_vault + u'/' + n->file);
            record(QStringLiteral("the sync deletes %1").arg(n->file));
            n->file.clear();
            writeState();
            return;
        }
        case SyncEnd:
            QFile::remove(m_gate);
            record(QStringLiteral("the sync ends"));
            return;
        case Switch: {
            QStringList candidates = vaultFiles().keys();
            candidates.sort();
            const QString open = m_backend->currentFolder().isEmpty() ? m_backend->currentNote()
                                                                      : m_backend->currentFolder() + u'/' + m_backend->currentNote();
            candidates.removeAll(open);
            if (candidates.isEmpty())
                return;
            const QString rel = candidates.at(m_rng.below(int(candidates.size())));
            js(QStringLiteral("root.openNote('%1', '%2')").arg(rel.section(u'/', 0, -2), rel.section(u'/', -1)));
            const QString now = m_backend->currentFolder().isEmpty() ? m_backend->currentNote()
                                                                     : m_backend->currentFolder() + u'/' + m_backend->currentNote();
            record(QStringLiteral("switch to %1%2").arg(rel, now == rel ? QString() : QStringLiteral(" (stays: %1)").arg(js(QStringLiteral("root.notice")).toString())));
            return;
        }
        case Stale:
            js(QStringLiteral("root.lastFocusSync = 0"));
            record(QStringLiteral("a minute passes"));
            return;
        case Focus:
            js(QStringLiteral("searchField.forceActiveFocus(), editor.forceActiveFocus()"));
            record(QStringLiteral("click into the editor"));
            return;
        case External: {
            if (Note *n = pickNote(true))
                remoteLine(*n, QStringLiteral("another program"));
            return;
        }
        case Copy: {
            Note *n = pickNote(true);
            if (!n)
                return;
            const QString copy = n->file.chopped(3) + QStringLiteral(".sync-conflict-20261114-1015%1-ABCDEF7.md").arg(++m_copies, 2, 10, QLatin1Char('0'));
            writeFile(copy, QStringLiteral("---\napple-note-id: %1\n---\n# %2\nthe phone's version\n").arg(n->id, n->title));
            record(QStringLiteral("Syncthing leaves %1").arg(copy));
            return;
        }
        case Resolve:
            js(QStringLiteral("root.chooseAllConflicts('both'), root.applyConflictChoices()"));
            record(QStringLiteral("resolve: keep both"));
            return;
        case FilesGone:
            m_tooMany = true;
            qputenv("ICLOUD_NOTES_SYNC_STUB_TOO_MANY", "1");
            record(QStringLiteral("most note files go missing (pushes refused)"));
            return;
        case FilesBack:
            m_tooMany = false;
            qunsetenv("ICLOUD_NOTES_SYNC_STUB_TOO_MANY");
            record(QStringLiteral("the files are back"));
            return;
        case Confirm:
            ++m_confirmations;
            js(QStringLiteral("deleteMissingDialog.open(), deleteMissingDialog.accept()"));
            record(QStringLiteral("Delete them… → Delete from iCloud"));
            return;
        case Reject:
            js(QStringLiteral("deleteMissingDialog.open(), deleteMissingDialog.reject()"));
            record(QStringLiteral("Delete them… → Keep them"));
            return;
        case Close:
            closeWindow();
            return;
        }
    }

    // Closing as the app does: the window's onClosing, then (accepted) the
    // QML engine goes before the backend, whose destructor writes a save
    // left waiting on a sync.
    bool closeWindow()
    {
        const bool dirty = js(QStringLiteral("root.dirty")).toBool();
        m_window->close();
        settle(20);
        if (m_window->isVisible()) {
            record(QStringLiteral("close: refused (%1)").arg(js(QStringLiteral("root.notice")).toString()));
            // Nothing here holds edits back (no guardrail, no other holder
            // of the vault): a refused close is a finding.
            return fail(QStringLiteral("the window refused to close: %1").arg(js(QStringLiteral("root.notice")).toString()));
        }
        record(QStringLiteral("close%1%2").arg(dirty ? QStringLiteral(" with unsaved edits") : QString(),
                                               m_backend->syncRunning() ? QStringLiteral(" during a sync") : QString()));
        m_engine.reset();
        m_window = nullptr;
        m_backend.reset();
        QFile::remove(m_gate);
        waitUntil([] { return VaultLock(NotesBackend::lockPath()).tryLock() == VaultLock::Locked; });
        m_closed = true;
        return true;
    }

    void dump()
    {
        QTextStream out(stdout);
        out << (m_failure.isEmpty() ? "sequence seed " : "FAIL sequence seed ") << m_seed << ": " << m_failure << "\n";
        for (int i = 0; i < m_log.size(); ++i)
            out << "  " << i << ". " << m_log.at(i) << "\n";
        const QHash<QString, QString> files = vaultFiles();
        QStringList names = files.keys();
        names.sort();
        for (const QString &name : names)
            out << "  --- " << name << "\n" << files.value(name);
        out << "  (replay: ICLOUD_NOTES_SEQUENCE_SEED=" << m_seed << " notes/bin/test)\n";
    }

    Rng m_rng;
    const quint64 m_seed;
    const QString m_dir;
    const int m_steps;
    QString m_vault, m_gate, m_engineLog;
    QList<Note> m_notes;
    QHash<QString, QString> m_tokens; // token -> the note (id, or file:<path>) it was written into
    QSet<QString> m_forgiven;          // on disk in a note iCloud deleted
    QStringList m_log;
    QString m_failure;
    int m_counter = 0;
    int m_copies = 0;
    int m_confirmations = 0;
    bool m_tooMany = false;
    bool m_closed = false;
    std::unique_ptr<NotesBackend> m_backend;
    std::unique_ptr<QQmlApplicationEngine> m_engine;
    QQuickWindow *m_window = nullptr;
};

// Named cases, each a generated failure (or a scan's finding) reduced.

// A folder's right-click menu while the open note's edits cannot be saved
// yet (a sync runs): the switch to that folder is refused, so is the menu.
// It popped up all the same, and its Rename and Delete then acted on the
// folder still open, not the one clicked.
void folderMenuWhileEditsWait(const QString &dir)
{
    Sequence s(0, dir, 0);
    s.setUp();
    bool ok = s.open();
    writeFile(QStringLiteral("gate"), QString(), dir);
    s.js(QStringLiteral("root.syncNow()"));
    ok = ok && waitUntil([&] { return s.m_backend->syncRunning(); });
    s.js(QStringLiteral("editor.insert(editor.length, 'U1Z\\n')"));
    s.js(QStringLiteral("root.folderMenuFor('Work')"));
    const bool refused = !s.js(QStringLiteral("folderMenu.visible")).toBool() && s.m_backend->currentFolder() == u"Notes";
    QFile::remove(s.m_gate);
    s.waitIdle();
    s.js(QStringLiteral("root.folderMenuFor('Work')"));
    const bool shown = s.js(QStringLiteral("folderMenu.visible")).toBool() && s.m_backend->currentFolder() == u"Work"
        && readFile(QStringLiteral("Notes/One.md")).contains(QStringLiteral("U1Z"));
    s.js(QStringLiteral("folderMenu.close()"));
    ok = ok && s.closeWindow() && s.m_failure.isEmpty();
    s.tearDown();
    check(ok && refused, "window: no folder menu while the switch to that folder is refused (edits waiting on a sync)");
    check(ok && shown, "window: the folder menu once the edits are saved and the folder is open");
}

} // namespace

int main(int argc, char *argv[])
{
    QGuiApplication app(argc, argv);
    app.setOrganizationName(QStringLiteral("icloud-notes"));
    app.setApplicationName(QStringLiteral("icloud-notes-sequence-test"));
    if (qEnvironmentVariable("ICLOUD_NOTES_PRIVATE_BUS") != QStringLiteral("1")) {
        QTextStream(stderr) << "sequence_test: run it through bin/test (a private D-Bus session bus)\n";
        return EXIT_FAILURE;
    }
    QQuickStyle::setStyle(QStringLiteral("Basic"));
    QDir().mkpath(QStandardPaths::writableLocation(QStandardPaths::CacheLocation));
    QTemporaryDir scratch(QStandardPaths::writableLocation(QStandardPaths::CacheLocation) + QStringLiteral("/icloud-notes-sequence-XXXXXX"));
    if (!scratch.isValid())
        return EXIT_FAILURE;
    const QString stubs = QDir(QCoreApplication::applicationDirPath() + QStringLiteral("/../stubs")).canonicalPath();
    qputenv("ICLOUD_NOTES_SYNC_BIN", (stubs + QStringLiteral("/icloud-notes-sync")).toUtf8());
    qputenv("XDG_RUNTIME_DIR", scratch.path().toUtf8());
    for (const char *dir : { "XDG_CONFIG_HOME", "XDG_DATA_HOME" }) {
        const QString path = scratch.path() + u'/' + QLatin1StringView(dir).toString().toLower();
        QDir().mkpath(path);
        qputenv(dir, path.toUtf8());
    }

    folderMenuWhileEditsWait(scratch.path() + QStringLiteral("/folder-menu"));

    bool seedGiven = false;
    const quint64 only = qEnvironmentVariable("ICLOUD_NOTES_SEQUENCE_SEED").toULongLong(&seedGiven);
    bool given = false;
    int cases = qEnvironmentVariable("ICLOUD_NOTES_SEQUENCE_CASES").toInt(&given);
    if (!given || cases <= 0)
        cases = 12;
    int steps = qEnvironmentVariable("ICLOUD_NOTES_SEQUENCE_STEPS").toInt(&given);
    if (!given || steps <= 1)
        steps = 28;
    int passed = 0;
    QList<quint64> seeds;
    if (seedGiven)
        seeds << only;
    else
        for (int i = 1; i <= cases; ++i)
            seeds << quint64(i);
    for (const quint64 seed : seeds) {
        const QString dir = scratch.path() + QStringLiteral("/seq-%1").arg(seed);
        QDir().mkpath(dir);
        if (Sequence(seed, dir, steps).run())
            ++passed;
    }
    check(passed == seeds.size(),
          qPrintable(QStringLiteral("sequences: %1 of %2 seeded runs of typing, saves, syncs and closes keep every edit, each in its own note")
                         .arg(passed)
                         .arg(seeds.size())));
    return report();
}
