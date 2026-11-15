// Layout test: the real window (main.qml, offscreen) on the real backend and
// the stub engine, in a scratch vault, at a wide width (folder list and note
// list) and a narrow one (the one-pane tree). What is selected, the open
// note and its unsaved edits carry over each way across the breakpoint; the
// tree opens notes and folders, searches, moves a note dragged onto a folder,
// and on a very narrow window shows the editor or the tree, one at a time.
// Run with bin/test.
#include "../src/notesbackend.h"
#include "../src/vaultlock.h"
#include "check.h"

#include <QDateTime>
#include <QElapsedTimer>
#include <QGuiApplication>
#include <QKeyEvent>
#include <QMouseEvent>
#include <QQmlApplicationEngine>
#include <QQmlContext>
#include <QQmlExpression>
#include <QQuickStyle>
#include <QQuickWindow>
#include <QStandardPaths>
#include <QTemporaryDir>
#include <QThread>
#include <functional>
#include <memory>

namespace {

bool waitUntil(const std::function<bool()> &cond, int timeoutMs = 60000)
{
    QElapsedTimer timer;
    timer.start();
    while (!cond() && timer.elapsed() < timeoutMs) {
        QCoreApplication::processEvents(QEventLoop::AllEvents, 10);
        QThread::msleep(2);
    }
    return cond();
}

void settle(int ms = 50)
{
    QElapsedTimer timer;
    timer.start();
    while (timer.elapsed() < ms) {
        QCoreApplication::processEvents(QEventLoop::AllEvents, 5);
        QThread::msleep(2);
    }
}

void writeAged(const QString &rel, const QString &content, int daysAgo)
{
    writeFile(rel, content);
    QFile f(testVault() + u'/' + rel);
    if (f.open(QIODevice::ReadWrite))
        f.setFileTime(QDateTime::currentDateTime().addDays(-daysAgo), QFileDevice::FileModificationTime);
}

class Window
{
public:
    std::unique_ptr<NotesBackend> backend;
    std::unique_ptr<QQmlApplicationEngine> engine;
    QQuickWindow *window = nullptr;

    bool open()
    {
        backend = std::make_unique<NotesBackend>();
        engine = std::make_unique<QQmlApplicationEngine>();
        engine->rootContext()->setContextProperty(QStringLiteral("backend"), backend.get());
        engine->load(QUrl::fromLocalFile(QStringLiteral(NOTES_QML)));
        window = engine->rootObjects().isEmpty() ? nullptr : qobject_cast<QQuickWindow *>(engine->rootObjects().constFirst());
        if (!window)
            return false;
        window->show();
        waitUntil([&] { return window->isExposed(); }, 10000);
        if (!waitUntil([&] { return js(QStringLiteral("root.started")).toBool() && !backend->syncRunning(); }))
            return false;
        // Nothing syncs on its own between the steps.
        js(QStringLiteral("autosave.interval = 1000000000, autoPush.interval = 1000000000, poll.interval = 1000000000"));
        return true;
    }

    void close()
    {
        engine.reset();
        backend.reset();
        waitUntil([] { return VaultLock(NotesBackend::lockPath()).tryLock() == VaultLock::Locked; });
    }

    // Runs `code` in main.qml's own context (its ids and functions).
    QVariant js(const QString &code)
    {
        QQmlExpression expression(qmlContext(window), window, QStringLiteral("(function () { return %1; })()").arg(code));
        QVariant value = expression.evaluate();
        if (expression.hasError())
            QTextStream(stdout) << "QML error in `" << code << "`: " << expression.error().toString() << "\n";
        if (value.userType() == qMetaTypeId<QJSValue>())
            value = value.value<QJSValue>().toVariant();
        return value;
    }
    QString str(const QString &code) { return js(code).toString(); }

    // Waits out a sync an action started (opening a note pulls first).
    void idle()
    {
        for (int i = 0; i < 3; ++i) {
            waitUntil([&] { return !backend->syncRunning(); });
            settle(30);
        }
    }

    // A geometry event still on its way (the window's first show) can undo
    // a resize: it is asked for again until it holds.
    void resize(int width)
    {
        for (int i = 0; i < 20; ++i) {
            window->resize(width, window->height());
            settle();
            if (window->width() == width && js(QStringLiteral("root.width")).toInt() == width) {
                settle();
                if (window->width() == width)
                    return;
            }
        }
        QTextStream(stdout) << "the window would not take a width of " << width << " (it is " << window->width() << ")\n";
    }

    QString openPath() { return str(QStringLiteral("root.openPath")); }

    // The tree's rows: "N path" or "F folder", indented two spaces a level.
    QStringList tree()
    {
        return js(QStringLiteral("(function () { var out = []; for (var i = 0; i < treeModel.count; i++) {"
                                 " var r = treeModel.get(i); out.push('  '.repeat(r.depth) + (r.kind === 'note' ? 'N ' + r.path : 'F ' + r.folder)); }"
                                 " return out; })()"))
            .toStringList();
    }
    int treeRow(const QString &line) { return tree().indexOf(line); }

    // A row of the tree, clicked as a person clicks it.
    void clickTree(const QString &line)
    {
        const int row = treeRow(line);
        js(QStringLiteral("treeView.positionViewAtIndex(%1, ListView.Contain)").arg(row));
        settle();
        const QPointF at = rowPoint(row, 60);
        mouse(QEvent::MouseButtonPress, at);
        mouse(QEvent::MouseButtonRelease, at);
        settle();
    }

    // A point on row `row` of a list (treeView, folderView, noteView), in the window.
    QPointF rowPoint(int row, int x, const QString &view = QStringLiteral("treeView"))
    {
        const QVariantMap p = js(QStringLiteral("(function () { var p = %3.itemAtIndex(%1).mapToItem(null, %2, 14); return { x: p.x, y: p.y }; })()")
                                     .arg(row)
                                     .arg(x)
                                     .arg(view))
                                  .toMap();
        return { p.value(QStringLiteral("x")).toDouble(), p.value(QStringLiteral("y")).toDouble() };
    }

    void mouse(QEvent::Type type, const QPointF &at)
    {
        const Qt::MouseButtons buttons = type == QEvent::MouseButtonRelease ? Qt::NoButton : Qt::LeftButton;
        QMouseEvent event(type, at, at, window->mapToGlobal(at), type == QEvent::MouseMove ? Qt::NoButton : Qt::LeftButton,
                          buttons, Qt::NoModifier);
        static QElapsedTimer clock;
        if (!clock.isValid())
            clock.start();
        event.setTimestamp(quint64(clock.elapsed()) + 1000); // apart, as a person's clicks are
        QCoreApplication::sendEvent(window, &event);
        QCoreApplication::processEvents();
    }

    // A key pressed where the focus is, then whatever sync it started.
    void key(Qt::Key key)
    {
        QKeyEvent press(QEvent::KeyPress, key, Qt::NoModifier);
        QCoreApplication::sendEvent(window, &press);
        QKeyEvent release(QEvent::KeyRelease, key, Qt::NoModifier);
        QCoreApplication::sendEvent(window, &release);
        idle();
    }

    // A note row dragged with the mouse onto another row of the tree.
    void dragTree(const QString &from, const QString &onto) { drag(rowPoint(treeRow(from), 60), rowPoint(treeRow(onto), 60)); }

    void drag(const QPointF &start, const QPointF &end)
    {
        mouse(QEvent::MouseButtonPress, start);
        for (int i = 1; i <= 12; ++i) {
            mouse(QEvent::MouseMove, start + (end - start) * (i / 12.0));
            settle(10);
        }
        mouse(QEvent::MouseButtonRelease, end);
        settle();
    }
};

void run()
{
    // Newest first: Groceries, Plan, Trip ideas, Pancakes, Brownies, Top.
    writeAged(QStringLiteral("Notes/Groceries.md"), QStringLiteral("---\napple-note-id: a\n---\n# Groceries\nmilk\n"), 0);
    writeAged(QStringLiteral("Work/Plan.md"), QStringLiteral("# Quarterly plan\nship it\n"), 1);
    writeAged(QStringLiteral("Notes/Trip ideas.md"), QStringLiteral("---\napple-note-id: b\n---\n# Trip ideas\nKyoto\n"), 2);
    writeAged(QStringLiteral("Recipes/Pancakes.md"), QStringLiteral("---\napple-note-id: c\n---\n# Pancakes\nflour, eggs\n"), 5);
    writeAged(QStringLiteral("Recipes/Desserts/Brownies.md"), QStringLiteral("# Brownies\ncocoa\n"), 12);
    writeAged(QStringLiteral("Top.md"), QStringLiteral("# At the top\nloose note\n"), 20);
    writeFile(QStringLiteral(".icloud-notes/state.json"),
              stateJson(QStringLiteral("in-body"),
                        { { QStringLiteral("a"), QStringLiteral("Notes/Groceries.md") },
                          { QStringLiteral("b"), QStringLiteral("Notes/Trip ideas.md") },
                          { QStringLiteral("c"), QStringLiteral("Recipes/Pancakes.md") } },
                        QStringLiteral("Notes")));

    Window w;
    const bool opened = w.open();
    check(opened, "layout: the window opens on the scratch vault");
    if (!opened)
        return;

    // Every note, for the tree: newest first, with its title and folder.
    {
        QStringList paths, titles, folders;
        for (const QVariant &v : w.backend->allNotes()) {
            const QVariantMap note = v.toMap();
            paths << note.value(QStringLiteral("path")).toString();
            titles << note.value(QStringLiteral("title")).toString();
            folders << note.value(QStringLiteral("folder")).toString();
        }
        check(paths == QStringList{ QStringLiteral("Notes/Groceries.md"), QStringLiteral("Work/Plan.md"), QStringLiteral("Notes/Trip ideas.md"),
                                    QStringLiteral("Recipes/Pancakes.md"), QStringLiteral("Recipes/Desserts/Brownies.md"), QStringLiteral("Top.md") }
                  && titles.at(1) == u"Quarterly plan" && titles.at(5) == u"At the top"
                  && folders.at(4) == u"Recipes/Desserts" && folders.at(5).isEmpty(),
              "layout: allNotes lists every note newest first, with its title and folder, nested and top-level ones too");
    }

    // The breakpoint: two panes at 1300 px, one at half of a 1920 px screen.
    w.resize(1300);
    const bool wide = !w.js(QStringLiteral("root.narrow")).toBool() && w.js(QStringLiteral("folderView.visible && noteView.visible && !treeView.visible")).toBool();
    w.resize(1062);
    const bool stillWide = !w.js(QStringLiteral("root.narrow")).toBool();
    w.resize(1061);
    const bool narrowAt1061 = w.js(QStringLiteral("root.narrow")).toBool();
    w.resize(960);
    const bool narrow = w.js(QStringLiteral("root.narrow && treeView.visible && !folderView.visible && !noteView.visible")).toBool();
    check(wide && stillWide && narrowAt1061 && narrow,
          "layout: two panes down to 1062 px, the one-pane tree below (960 px, half a 1920 px screen, included)");

    // Wide to narrow: the open note, its unsaved edits and the selection stay.
    w.resize(1300);
    w.js(QStringLiteral("root.openNote('Recipes', 'Pancakes.md')"));
    w.idle();
    w.js(QStringLiteral("editor.insert(editor.length, 'typed while wide\\n')"));
    w.resize(960);
    const QStringList treeNarrow = w.tree();
    check(w.openPath() == u"Recipes/Pancakes.md" && w.js(QStringLiteral("root.dirty")).toBool()
              && w.str(QStringLiteral("editor.text")).contains(QStringLiteral("typed while wide"))
              && w.str(QStringLiteral("treeModel.get(root.treeIndexOfCurrent()).path")) == u"Recipes/Pancakes.md"
              && treeNarrow.contains(QStringLiteral("F Recipes")) && treeNarrow.contains(QStringLiteral("  N Recipes/Pancakes.md"))
              && w.str(QStringLiteral("root.kbFocus('notes'), treeModel.get(treeView.currentIndex).path")) == u"Recipes/Pancakes.md",
          "layout: going narrow keeps the open note and its unsaved edits, and the tree unfolds to it and has it selected");

    // In the tree, a click on a folder unfolds it; one on a note opens it
    // (the edits of the note left are saved first).
    w.clickTree(QStringLiteral("F Notes"));
    const bool unfolded = w.tree().contains(QStringLiteral("  N Notes/Groceries.md")) && w.backend->currentFolder() == u"Notes";
    w.clickTree(QStringLiteral("  N Notes/Groceries.md"));
    w.idle();
    check(unfolded && w.openPath() == u"Notes/Groceries.md" && !w.js(QStringLiteral("root.dirty")).toBool()
              && readFile(QStringLiteral("Recipes/Pancakes.md")).contains(QStringLiteral("typed while wide")),
          "layout: the tree unfolds a clicked folder and opens a clicked note, saving the edits left behind");

    // The arrows walk the tree: Down opens the next note, Up the one before,
    // Left steps out to the folder and folds it, Right unfolds it.
    w.key(Qt::Key_Down);
    const bool down = w.openPath() == u"Notes/Trip ideas.md";
    w.key(Qt::Key_Up);
    const bool up = w.openPath() == u"Notes/Groceries.md";
    w.key(Qt::Key_Left);
    const bool out = w.str(QStringLiteral("treeModel.get(treeView.currentIndex).folder")) == u"Notes";
    w.key(Qt::Key_Left);
    const bool folded = !w.tree().contains(QStringLiteral("  N Notes/Groceries.md"));
    w.key(Qt::Key_Right);
    if (!(down && up && out && folded))
        QTextStream(stdout) << "arrows: down " << down << " up " << up << " out " << out << " folded " << folded << " open "
                            << w.openPath() << " tree " << w.tree().join(u'|') << "\n";
    check(down && up && out && folded && w.tree().contains(QStringLiteral("  N Notes/Groceries.md"))
              && w.openPath() == u"Notes/Groceries.md",
          "layout: the arrow keys walk the tree, opening notes, and fold and unfold folders");

    // Narrow to wide: the note opened in the tree is the open one, in its folder.
    w.resize(1300);
    check(w.openPath() == u"Notes/Groceries.md"
              && w.str(QStringLiteral("root.kbFocus('folders'), folderView.model[folderView.currentIndex]")) == u"Notes"
              && w.str(QStringLiteral("root.kbFocus('notes'), root.rowNote(noteView.model[noteView.currentIndex]).file")) == u"Groceries.md",
          "layout: going wide shows the note opened in the tree selected in its folder's list");

    // A folder selected with no note open carries over both ways.
    w.js(QStringLiteral("root.openFolder('Work')"));
    w.resize(960);
    const bool workSelected = w.backend->currentNote().isEmpty()
        && w.str(QStringLiteral("treeModel.get(root.treeIndexOfCurrent()).folder")) == u"Work";
    w.clickTree(QStringLiteral("F Recipes"));
    w.resize(1300);
    check(workSelected && w.backend->currentFolder() == u"Recipes"
              && w.str(QStringLiteral("root.kbFocus('folders'), folderView.model[folderView.currentIndex]")) == u"Recipes",
          "layout: the selected folder carries over each way");

    // The search box filters the whole tree and opens the folders with matches.
    w.resize(960);
    w.js(QStringLiteral("root.searchText = 'brownie'"));
    check(w.tree() == QStringList{ QStringLiteral("F "), QStringLiteral("  N Recipes/Desserts/Brownies.md"), QStringLiteral("F Recipes"),
                                   QStringLiteral("  F Recipes/Desserts"), QStringLiteral("    N Recipes/Desserts/Brownies.md") }
              && w.str(QStringLiteral("searchField.text + treeSearch.text")) == u"browniebrownie",
          "layout: a search filters the tree to its matches, folders holding one open; both layouts' boxes show it");
    w.js(QStringLiteral("root.searchText = ''"));
    settle();

    // A note row dragged onto a folder moves the note there.
    w.js(QStringLiteral("root.setExpanded('Notes', true), root.setExpanded('Work', true)"));
    settle();
    w.dragTree(QStringLiteral("  N Notes/Trip ideas.md"), QStringLiteral("F Work"));
    w.idle();
    check(QFile::exists(testVault() + QStringLiteral("/Work/Trip ideas.md")) && !QFile::exists(testVault() + QStringLiteral("/Notes/Trip ideas.md"))
              && w.tree().contains(QStringLiteral("  N Work/Trip ideas.md")),
          "layout: dragging a note onto a folder of the tree moves it there");
    // The same move, a note of the wide note list dragged onto the folder list, and from a note's menu.
    w.resize(1300);
    w.js(QStringLiteral("root.openFolder('Work')"));
    settle();
    w.drag(w.rowPoint(w.js(QStringLiteral("noteView.model.indexOf('Trip ideas.md')")).toInt(), 60, QStringLiteral("noteView")),
           w.rowPoint(w.js(QStringLiteral("folderView.model.indexOf('Recipes/Desserts')")).toInt(), 60, QStringLiteral("folderView")));
    w.idle();
    const bool movedWide = QFile::exists(testVault() + QStringLiteral("/Recipes/Desserts/Trip ideas.md"));
    w.js(QStringLiteral("root.noteMenuFor('Recipes/Desserts', 'Trip ideas.md')"));
    const bool menu = w.js(QStringLiteral("noteMenu.visible")).toBool();
    w.js(QStringLiteral("noteMenu.close(), moveDialog.open()"));
    settle();
    w.js(QStringLiteral("moveTarget.currentIndex = moveTarget.find('Notes'), moveDialog.accept()"));
    w.idle();
    check(movedWide && menu && QFile::exists(testVault() + QStringLiteral("/Notes/Trip ideas.md")),
          "layout: the note list moves a note dropped on a folder too, and a note's menu moves it with Move to");

    // Very narrow: the editor or the tree, one at a time.
    w.js(QStringLiteral("root.openNote('Notes', 'Groceries.md')"));
    w.idle();
    w.resize(450);
    const bool editorOnly = w.js(QStringLiteral("root.editorVisible && !root.sidebarVisible && editor.visible")).toBool();
    w.js(QStringLiteral("sidebarButton.clicked()"));
    settle();
    const bool treeOnly = w.js(QStringLiteral("root.sidebarVisible && !root.editorVisible && treeView.visible")).toBool();
    w.clickTree(QStringLiteral("  N Notes/Trip ideas.md"));
    w.idle();
    check(editorOnly && treeOnly && w.openPath() == u"Notes/Trip ideas.md"
              && w.js(QStringLiteral("root.editorVisible && !root.sidebarVisible")).toBool(),
          "layout: below the tree's and editor's minimums, the open note fills the window; the sidebar button "
          "brings the tree, and a note opened there brings the editor back");
    w.resize(1300);
    check(w.js(QStringLiteral("root.sidebarVisible && root.editorVisible && noteView.visible")).toBool()
              && w.openPath() == u"Notes/Trip ideas.md",
          "layout: wide again, both panes and the editor are back on the same note");
    w.close();
}

} // namespace

int main(int argc, char *argv[])
{
    QGuiApplication app(argc, argv);
    app.setOrganizationName(QStringLiteral("icloud-notes"));
    app.setApplicationName(QStringLiteral("icloud-notes-layout-test"));
    if (qEnvironmentVariable("ICLOUD_NOTES_PRIVATE_BUS") != QStringLiteral("1")) {
        QTextStream(stderr) << "layout_test: run it through bin/test (a private D-Bus session bus)\n";
        return EXIT_FAILURE;
    }
    QQuickStyle::setStyle(QStringLiteral("Basic"));
    QDir().mkpath(QStandardPaths::writableLocation(QStandardPaths::CacheLocation));
    QTemporaryDir scratch(QStandardPaths::writableLocation(QStandardPaths::CacheLocation) + QStringLiteral("/icloud-notes-layout-XXXXXX"));
    if (!scratch.isValid())
        return EXIT_FAILURE;
    const QString stubs = QDir(QCoreApplication::applicationDirPath() + QStringLiteral("/../stubs")).canonicalPath();
    qputenv("ICLOUD_NOTES_SYNC_BIN", (stubs + QStringLiteral("/icloud-notes-sync")).toUtf8());
    qputenv("ICLOUD_NOTES_SYNC_STUB_LOG", (scratch.path() + QStringLiteral("/engine.log")).toUtf8());
    qputenv("XDG_RUNTIME_DIR", scratch.path().toUtf8());
    for (const char *dir : { "XDG_CONFIG_HOME", "XDG_DATA_HOME" }) {
        const QString path = scratch.path() + u'/' + QLatin1StringView(dir).toString().toLower();
        QDir().mkpath(path);
        qputenv(dir, path.toUtf8());
    }
    qputenv("ICLOUD_NOTES_VAULT", (scratch.path() + QStringLiteral("/vault")).toUtf8());
    // Text matches only: the scratch notes never go to a Recall installed here.
    QStringList path;
    for (const QString &dir : qEnvironmentVariable("PATH").split(u':'))
        if (!QFileInfo(dir + QStringLiteral("/recall")).isExecutable())
            path << dir;
    qputenv("PATH", path.join(u':').toUtf8());
    run();
    return report();
}
