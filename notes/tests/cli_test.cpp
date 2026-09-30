// The command line (`icloud-notes <command>`), run as a separate process
// the way an agent runs it: this binary starts itself with --as-cli, which
// is cliMain, the same entry the app's main() hands commands to. A scratch
// vault under a temporary directory, the icloud-notes-sync stub as the engine and
// a fake icloud-session on the private bus bin/test starts: never the real
// notes, account or daemon. Checks exit codes and the JSON shapes docs/CLI.md
// documents.
#include "../src/cli.h"
#include "../src/notesbackend.h"
#include "../src/vaultlock.h"
#include "check.h"
#include "fake_session.h"

#include <QCoreApplication>
#include <QDir>
#include <QElapsedTimer>
#include <QFile>
#include <QJsonArray>
#include <QJsonDocument>
#include <QJsonObject>
#include <QProcess>
#include <QStandardPaths>
#include <QTemporaryDir>
#include <QThread>

namespace {

QString g_vault;
QString g_scratch;

bool exists(const QString &rel)
{
    return QFile::exists(g_vault + QLatin1Char('/') + rel);
}

struct Run {
    int code = -1;
    QByteArray out;
    QByteArray err;
    qint64 ms = 0;

    QJsonValue json() const
    {
        const QJsonDocument doc = QJsonDocument::fromJson(out);
        return doc.isArray() ? QJsonValue(doc.array()) : QJsonValue(doc.object());
    }
    // The --json error object: the last line of stderr.
    QJsonObject error() const
    {
        const QList<QByteArray> lines = err.trimmed().split('\n');
        return QJsonDocument::fromJson(lines.last()).object().value(QStringLiteral("error")).toObject();
    }
    QString errorCode() const { return error().value(QStringLiteral("code")).toString(); }
};

// Runs `icloud-notes <args>` with stdin not a terminal, spinning the event
// loop meanwhile so the fake icloud-session answers it.
Run cli(const QStringList &args, const QHash<QString, QString> &env = {})
{
    QProcess p;
    QProcessEnvironment e = QProcessEnvironment::systemEnvironment();
    for (auto it = env.cbegin(); it != env.cend(); ++it)
        e.insert(it.key(), it.value());
    p.setProcessEnvironment(e);
    p.setProgram(QCoreApplication::applicationFilePath());
    p.setArguments(QStringList{ QStringLiteral("--as-cli") } + args);
    p.setStandardInputFile(QProcess::nullDevice());
    QElapsedTimer timer;
    timer.start();
    p.start();
    while (p.state() != QProcess::NotRunning && timer.elapsed() < 60000) {
        QCoreApplication::processEvents(QEventLoop::AllEvents, 20);
        p.waitForFinished(10);
    }
    Run r;
    r.code = p.exitStatus() == QProcess::NormalExit ? p.exitCode() : -1;
    r.out = p.readAllStandardOutput();
    r.err = p.readAllStandardError();
    r.ms = timer.elapsed();
    return r;
}

Run cliJson(QStringList args, const QHash<QString, QString> &env = {})
{
    args.prepend(QStringLiteral("--json"));
    return cli(args, env);
}

QStringList paths(const QJsonArray &notes)
{
    QStringList out;
    for (const QJsonValue &v : notes)
        out << v.toObject().value(QStringLiteral("path")).toString();
    return out;
}

QStringList stubLog()
{
    QString text = readFile(QStringLiteral("stub.log"), g_scratch);
    return text.split(u'\n', Qt::SkipEmptyParts);
}

void seedVault()
{
    QDir(g_vault).removeRecursively();
    writeFile(QStringLiteral(".icloud-md/state.json"),
              stateJson(QStringLiteral("in-body"),
                        { { "id-a", "Notes/Alpha.md" },
                          { "id-b", "Work/Beta.md" },
                          { "id-g", "Work/Grid.md", "is too large" },
                          { "id-p", "Work/Pics.md" },
                          { "id-u", "Work/Tangle.md" } },
                        QStringLiteral("Notes")));
    writeFile(QStringLiteral("Notes/Alpha.md"),
              QStringLiteral("---\napple-note-id: id-a\n---\n# Alpha\n- [ ] milk\n- eggs\nplain line\n"));
    writeFile(QStringLiteral("Work/Beta.md"),
              QStringLiteral("---\napple-note-id: id-b\n---\n# Beta\nshared\n<<<<<<< local\nmine 1\n=======\ntheirs 1\n>>>>>>> remote\n"
                             "middle\n<<<<<<< local\nmine 2\n=======\ntheirs 2\n>>>>>>> remote\n"));
    writeFile(QStringLiteral("Work/Grid.md"), QStringLiteral("---\napple-note-id: id-g\n---\n# Grid\nhuge\n"));
    writeFile(QStringLiteral("Work/Pics.md"),
              QStringLiteral("---\napple-note-id: id-p\n---\n# Pics\n![pic.png](attachments/pic.png)\n"));
    writeFile(QStringLiteral("Work/attachments/pic.png"), QStringLiteral("png"));
    writeFile(QStringLiteral("Work/Tangle.md"),
              QStringLiteral("---\napple-note-id: id-u\n---\n# Tangle\n<<<<<<< local\na\n<<<<<<< local\nb\n=======\nc\n"
                             ">>>>>>> remote\n=======\nd\n>>>>>>> remote\n"));
    writeFile(QStringLiteral(".icloud-md/base/id-u.md"), QStringLiteral("# Tangle\nsynced text\n"));
    writeFile(QStringLiteral("Twin.md"), QStringLiteral("# Same title\n"));
    writeFile(QStringLiteral("Notes/Twin.md"), QStringLiteral("# Same title\n"));
}

} // namespace

int main(int argc, char *argv[])
{
    if (argc > 1 && qstrcmp(argv[1], "--as-cli") == 0)
        return cliMain(argc - 1, argv + 1);

    QCoreApplication app(argc, argv);
    if (qEnvironmentVariable("ICLOUD_NOTES_PRIVATE_BUS") != QStringLiteral("1")) {
        QTextStream(stderr) << "cli_test: run it through bin/test (a private D-Bus session bus)\n";
        return EXIT_FAILURE;
    }
    // On the home filesystem (under the cache dir), like backend_test, so
    // moving to the trash works; the children's trash is in the scratch
    // directory too (XDG_DATA_HOME).
    QDir().mkpath(QStandardPaths::writableLocation(QStandardPaths::CacheLocation));
    QTemporaryDir scratch(QStandardPaths::writableLocation(QStandardPaths::CacheLocation)
                          + QStringLiteral("/icloud-notes-cli-test-XXXXXX"));
    if (!scratch.isValid())
        return EXIT_FAILURE;
    g_scratch = scratch.path();
    g_vault = g_scratch + QStringLiteral("/vault");
    qputenv("ICLOUD_NOTES_VAULT", g_vault.toUtf8());
    qputenv("XDG_RUNTIME_DIR", g_scratch.toUtf8());
    // Qt trashes into $XDG_DATA_HOME/Trash only when that directory exists.
    QDir().mkpath(g_scratch + QStringLiteral("/xdg-data"));
    qputenv("XDG_DATA_HOME", (g_scratch + QStringLiteral("/xdg-data")).toUtf8());
    qputenv("ICLOUD_NOTES_SYNC_STUB_LOG", (g_scratch + QStringLiteral("/stub.log")).toUtf8());
    const QString stubs = QDir(QCoreApplication::applicationDirPath() + QStringLiteral("/../stubs")).canonicalPath();
    // The engine the app runs, whatever /usr/lib or PATH hold.
    qputenv("ICLOUD_NOTES_SYNC_BIN", (stubs + QStringLiteral("/icloud-notes-sync")).toUtf8());
    check(QFile::exists(stubs + QStringLiteral("/icloud-notes-sync")), "cli stub is the engine");

    QDBusConnection fakeBus = QDBusConnection::connectToBus(QDBusConnection::SessionBus, QStringLiteral("fake-session"));
    FakeSession fake(fakeBus);
    fake.signedIn = true;
    fake.appleId = QStringLiteral("someone@example.com");
    fake.dsid = QStringLiteral("1006081438");
    check(fake.claimName(), "cli fake icloud-session owns the bus name");
    seedVault();

    // ---- help, usage, exit codes -------------------------------------------
    {
        const Run help = cli({ QStringLiteral("help") });
        check(help.code == 0 && help.out.contains("Exit codes"), "cli help");
        const QStringList commands{ "status", "folders", "list", "read", "search", "new", "write", "rename", "move",
                                    "delete", "toggle", "resolve", "recover", "export-pdf", "new-folder",
                                    "rename-folder", "delete-folder", "sync", "pull", "push", "clone", "history",
                                    "diff", "restore" };
        bool all = true;
        for (const QString &c : commands) {
            const Run r = cli({ c, QStringLiteral("--help") });
            const Run h = cli({ QStringLiteral("help"), c });
            all = all && r.code == 0 && r.out.startsWith(("Usage: icloud-notes " + c).toUtf8()) && r.out.contains("Exit codes")
                && help.out.contains(("  " + c).toUtf8()) && h.out == r.out;
        }
        check(all, "cli every command has --help, and help lists it");
        const Run bogus = cliJson({ QStringLiteral("bogus") });
        check(bogus.code == 64 && bogus.out.isEmpty() && bogus.errorCode() == QStringLiteral("usage")
                  && bogus.error().value(QStringLiteral("exit_code")).toInt() == 64,
              "cli unknown command: exit 64, JSON usage error");
        check(cli({ QStringLiteral("list"), QStringLiteral("extra") }).code == 64, "cli extra argument is usage");
        check(cli({ QStringLiteral("read"), QStringLiteral("Alpha"), QStringLiteral("--yes") }).code == 64,
              "cli an option the command does not take is usage");
        check(cliJson({ QStringLiteral("--version") }).json().toObject().contains(QStringLiteral("version")), "cli --version");
    }

    // ---- reading ---------------------------------------------------------------
    {
        const Run st = cliJson({ QStringLiteral("status") });
        const QJsonObject s = st.json().toObject();
        check(st.code == 0 && s.value(QStringLiteral("cloned")).toBool() && s.value(QStringLiteral("signed_in")).toBool()
                  && s.value(QStringLiteral("apple_id")).toString() == QStringLiteral("someone@example.com")
                  && s.value(QStringLiteral("vault")).toString() == g_vault
                  && s.value(QStringLiteral("title_mode")).toString() == QStringLiteral("in-body")
                  && s.value(QStringLiteral("lock")).toObject().value(QStringLiteral("held_by")).isNull()
                  && s.value(QStringLiteral("notes")).toInt() == 7,
              "cli status JSON");
        const QJsonObject flagged = s.value(QStringLiteral("flagged")).toObject();
        check(flagged.value(QStringLiteral("conflict")).toArray().contains(QStringLiteral("Work/Beta.md"))
                  && flagged.value(QStringLiteral("read-only")).toArray().contains(QStringLiteral("Work/Grid.md"))
                  && flagged.value(QStringLiteral("new")).toArray().contains(QStringLiteral("Twin.md")),
              "cli status lists the flagged notes");

        const QJsonArray folders = cliJson({ QStringLiteral("folders") }).json().toArray();
        check(folders.size() == 3 && folders.at(0).toObject().value(QStringLiteral("folder")).toString().isEmpty()
                  && folders.at(0).toObject().value(QStringLiteral("notes")).toInt() == 7
                  && folders.at(1).toObject().value(QStringLiteral("folder")).toString() == QStringLiteral("Notes")
                  && folders.at(1).toObject().value(QStringLiteral("default")).toBool(),
              "cli folders: root, then the default folder first");

        const QJsonArray all = cliJson({ QStringLiteral("list") }).json().toArray();
        const QJsonObject alpha = [&] {
            for (const QJsonValue &v : all)
                if (v.toObject().value(QStringLiteral("path")) == QStringLiteral("Notes/Alpha.md"))
                    return v.toObject();
            return QJsonObject();
        }();
        check(all.size() == 7 && alpha.value(QStringLiteral("title")).toString() == QStringLiteral("Alpha")
                  && alpha.value(QStringLiteral("id")).toString() == QStringLiteral("id-a")
                  && alpha.value(QStringLiteral("folder")).toString() == QStringLiteral("Notes")
                  && alpha.value(QStringLiteral("file")).toString() == QStringLiteral("Alpha.md")
                  && alpha.value(QStringLiteral("modified")).toString().endsWith(u'Z')
                  && alpha.value(QStringLiteral("flags")).toArray().isEmpty()
                  && alpha.contains(QStringLiteral("snippet")) && alpha.contains(QStringLiteral("modified_ms")),
              "cli list JSON");
        check(paths(cliJson({ QStringLiteral("list"), QStringLiteral("--folder"), QStringLiteral("work") }).json().toArray()).size() == 4,
              "cli list --folder (any case)");
        check(paths(cliJson({ QStringLiteral("list"), QStringLiteral("--flag"), QStringLiteral("conflict") }).json().toArray())
                  == QStringList{ QStringLiteral("Work/Beta.md"), QStringLiteral("Work/Tangle.md") }
                  || paths(cliJson({ QStringLiteral("list"), QStringLiteral("--flag"), QStringLiteral("conflict") }).json().toArray())
                         == QStringList{ QStringLiteral("Work/Tangle.md"), QStringLiteral("Work/Beta.md") },
              "cli list --flag conflict");

        const Run rd = cliJson({ QStringLiteral("read"), QStringLiteral("beta") });
        const QJsonObject beta = rd.json().toObject();
        const QJsonArray conflicts = beta.value(QStringLiteral("conflicts")).toArray();
        check(rd.code == 0 && beta.value(QStringLiteral("path")).toString() == QStringLiteral("Work/Beta.md")
                  && beta.value(QStringLiteral("body")).toString().startsWith(QStringLiteral("# Beta\nshared\n"))
                  && !beta.contains(QStringLiteral("text")) && conflicts.size() == 2
                  && conflicts.at(0).toObject().value(QStringLiteral("local")).toArray() == QJsonArray{ QStringLiteral("mine 1") }
                  && conflicts.at(1).toObject().value(QStringLiteral("remote")).toArray() == QJsonArray{ QStringLiteral("theirs 2") }
                  && !beta.value(QStringLiteral("conflicts_unreadable")).toBool()
                  && beta.value(QStringLiteral("read_only")).isNull(),
              "cli read JSON by title, with its conflict blocks");
        check(cli({ QStringLiteral("read"), QStringLiteral("Notes/Alpha") }).out
                  == "# Alpha\n- [ ] milk\n- eggs\nplain line\n",
              "cli read by path without .md prints the body");
        check(cli({ QStringLiteral("read"), QStringLiteral("id-a"), QStringLiteral("--raw") }).out.startsWith("---\napple-note-id: id-a\n"),
              "cli read --raw by id prints the whole file");
        check(cli({ QStringLiteral("read"), g_vault + QStringLiteral("/Work/Grid.md") }).code == 0, "cli read by absolute path");
        const QJsonObject grid = cliJson({ QStringLiteral("read"), QStringLiteral("grid") }).json().toObject();
        check(grid.value(QStringLiteral("read_only")).toString() == QStringLiteral("is too large"), "cli read says read-only");
        const QJsonObject pics = cliJson({ QStringLiteral("read"), QStringLiteral("pics") }).json().toObject();
        check(pics.value(QStringLiteral("attachments")).toArray().size() == 1
                  && pics.value(QStringLiteral("attachments")).toArray().at(0).toObject().value(QStringLiteral("path")).toString()
                         == g_vault + QStringLiteral("/Work/attachments/pic.png"),
              "cli read lists attachments with their paths");
        const Run missing = cliJson({ QStringLiteral("read"), QStringLiteral("nothing like it") });
        check(missing.code == 1 && missing.errorCode() == QStringLiteral("not_found") && missing.out.isEmpty(),
              "cli read unknown note: not_found");
        const Run twin = cliJson({ QStringLiteral("read"), QStringLiteral("same title") });
        check(twin.code == 1 && twin.errorCode() == QStringLiteral("ambiguous")
                  && twin.error().value(QStringLiteral("message")).toString().contains(QStringLiteral("Notes/Twin.md")),
              "cli read ambiguous title lists the paths");
        check(cli({ QStringLiteral("read"), QStringLiteral("Notes/Twin.md") }).code == 0, "cli a path settles it");

        const QJsonArray found = cliJson({ QStringLiteral("search"), QStringLiteral("EGGS") }).json().toArray();
        check(found.size() == 1 && found.at(0).toObject().value(QStringLiteral("path")).toString() == QStringLiteral("Notes/Alpha.md")
                  && found.at(0).toObject().value(QStringLiteral("snippet")).toString() == QStringLiteral("eggs"),
              "cli search JSON");
        check(cli({ QStringLiteral("search"), QStringLiteral("e") }).code == 64, "cli search wants 2 characters");
    }

    // ---- changing notes ----------------------------------------------------
    {
        const Run made = cliJson({ QStringLiteral("new"), QStringLiteral("Groceries"), QStringLiteral("--body"),
                                   QStringLiteral("bread\ncheese") });
        const QJsonObject m = made.json().toObject();
        check(made.code == 0 && m.value(QStringLiteral("path")).toString() == QStringLiteral("Notes/Groceries.md")
                  && m.value(QStringLiteral("action")).toString() == QStringLiteral("new")
                  && readFile(QStringLiteral("Notes/Groceries.md")) == QStringLiteral("# Groceries\nbread\ncheese\n"),
              "cli new goes to the default folder, title first, then the body");
        const Run again = cliJson({ QStringLiteral("new"), QStringLiteral("Groceries") });
        check(again.code == 1 && again.errorCode() == QStringLiteral("exists"), "cli new refuses an existing note");
        check(cli({ QStringLiteral("new"), QStringLiteral("Root note"), QStringLiteral("--folder"), QString() }).code == 0
                  && readFile(QStringLiteral("Root note.md")) == QStringLiteral("# Root note\n"),
              "cli new --folder \"\" is the vault root");
        check(cliJson({ QStringLiteral("new"), QStringLiteral("X"), QStringLiteral("--folder"), QStringLiteral("Nope") }).errorCode()
                  == QStringLiteral("not_found"),
              "cli new into a missing folder: not_found");
        {
            QProcess p;
            p.setProgram(QCoreApplication::applicationFilePath());
            p.setArguments({ QStringLiteral("--as-cli"), QStringLiteral("new"), QStringLiteral("Piped"), QStringLiteral("--stdin") });
            p.start();
            p.write("from stdin\n");
            p.closeWriteChannel();
            p.waitForFinished(30000);
            check(p.exitCode() == 0 && readFile(QStringLiteral("Notes/Piped.md")) == QStringLiteral("# Piped\nfrom stdin\n"),
                  "cli new --stdin");
        }

        check(cli({ QStringLiteral("write"), QStringLiteral("groceries"), QStringLiteral("--append"), QStringLiteral("--body"),
                    QStringLiteral("jam") })
                      .code
                  == 0
                  && readFile(QStringLiteral("Notes/Groceries.md")) == QStringLiteral("# Groceries\nbread\ncheese\njam\n"),
              "cli write --append");
        writeFile(QStringLiteral("body.txt"), QStringLiteral("# Alpha\nreplaced"), g_scratch);
        const Run w = cliJson({ QStringLiteral("write"), QStringLiteral("alpha"), QStringLiteral("--file"),
                                g_scratch + QStringLiteral("/body.txt") });
        check(w.code == 0 && w.json().toObject().value(QStringLiteral("changed")).toBool()
                  && readFile(QStringLiteral("Notes/Alpha.md")) == QStringLiteral("---\napple-note-id: id-a\n---\n# Alpha\nreplaced\n"),
              "cli write --file keeps the ID block");
        const QString before = readFile(QStringLiteral("Notes/Alpha.md"));
        const Run guarded = cliJson({ QStringLiteral("write"), QStringLiteral("alpha"), QStringLiteral("--body"),
                                      QStringLiteral("# Alpha\n<<<<<<< local\nx\n=======\ny\n>>>>>>> remote\n") });
        check(guarded.code == 1 && guarded.errorCode() == QStringLiteral("guardrail")
                  && readFile(QStringLiteral("Notes/Alpha.md")) == before,
              "cli write refuses new conflict markers");
        check(cli({ QStringLiteral("write"), QStringLiteral("alpha"), QStringLiteral("--force"), QStringLiteral("--body"),
                    QStringLiteral("# Alpha\n<<<<<<< local\nx\n=======\ny\n>>>>>>> remote\n") })
                      .code
                  == 0
                  && readFile(QStringLiteral("Notes/Alpha.md")).contains(QStringLiteral("<<<<<<< local")),
              "cli write --force writes them anyway");
        writeFile(QStringLiteral("Notes/Alpha.md"), QStringLiteral("---\napple-note-id: id-a\n---\n# Alpha\n- [ ] milk\n- eggs\n"));
        const Run ro = cliJson({ QStringLiteral("write"), QStringLiteral("grid"), QStringLiteral("--body"), QStringLiteral("x") });
        check(ro.code == 1 && ro.errorCode() == QStringLiteral("read_only")
                  && readFile(QStringLiteral("Work/Grid.md")).contains(QStringLiteral("huge")),
              "cli write refuses a read-only note");
        check(cli({ QStringLiteral("write"), QStringLiteral("alpha") }).code == 64, "cli write needs the text");

        const Run t1 = cliJson({ QStringLiteral("toggle"), QStringLiteral("alpha"), QStringLiteral("2") });
        const Run t2 = cliJson({ QStringLiteral("toggle"), QStringLiteral("alpha"), QStringLiteral("3") });
        check(t1.code == 0 && t1.json().toObject().value(QStringLiteral("text")).toString() == QStringLiteral("- [x] milk")
                  && t2.json().toObject().value(QStringLiteral("text")).toString() == QStringLiteral("- [ ] eggs"),
              "cli toggle ticks a box and plants one on a list item");
        check(cliJson({ QStringLiteral("toggle"), QStringLiteral("alpha"), QStringLiteral("1") }).errorCode()
                  == QStringLiteral("not_a_list_item"),
              "cli toggle leaves a non-list line alone");

        const Run rn = cliJson({ QStringLiteral("rename"), QStringLiteral("groceries"), QStringLiteral("Shopping") });
        check(rn.code == 0 && readFile(QStringLiteral("Notes/Groceries.md")).startsWith(QStringLiteral("# Shopping\n"))
                  && rn.json().toObject().value(QStringLiteral("title")).toString() == QStringLiteral("Shopping"),
              "cli rename retitles the first line (in-body vault)");
        check(cliJson({ QStringLiteral("rename"), QStringLiteral("grid"), QStringLiteral("Other") }).errorCode()
                  == QStringLiteral("read_only"),
              "cli rename refuses a read-only note");

        const Run mv = cliJson({ QStringLiteral("move"), QStringLiteral("Notes/Groceries.md"), QStringLiteral("Work") });
        check(mv.code == 0 && exists(QStringLiteral("Work/Groceries.md")) && !exists(QStringLiteral("Notes/Groceries.md"))
                  && mv.json().toObject().value(QStringLiteral("path")).toString() == QStringLiteral("Work/Groceries.md")
                  && mv.json().toObject().value(QStringLiteral("from")).toString() == QStringLiteral("Notes/Groceries.md"),
              "cli move");
        check(cliJson({ QStringLiteral("move"), QStringLiteral("pics"), QStringLiteral("Notes") }).errorCode()
                  == QStringLiteral("has_attachments"),
              "cli move refuses a note with attachments");
        check(cliJson({ QStringLiteral("move"), QStringLiteral("grid"), QStringLiteral("Notes") }).errorCode()
                  == QStringLiteral("read_only"),
              "cli move refuses a read-only note");

        const Run noYes = cliJson({ QStringLiteral("delete"), QStringLiteral("Work/Groceries.md") });
        check(noYes.code == 64 && exists(QStringLiteral("Work/Groceries.md")), "cli delete without --yes and no terminal: refused");
        const Run del = cliJson({ QStringLiteral("delete"), QStringLiteral("Work/Groceries.md"), QStringLiteral("--yes") });
        check(del.code == 0 && !exists(QStringLiteral("Work/Groceries.md"))
                  && QDir(g_scratch + QStringLiteral("/xdg-data/Trash/files")).entryList(QDir::Files).contains(QStringLiteral("Groceries.md")),
              "cli delete --yes moves it to the trash");
    }

    // ---- conflicts ---------------------------------------------------------
    {
        const Run bad = cliJson({ QStringLiteral("resolve"), QStringLiteral("beta"), QStringLiteral("--choices"),
                                  QStringLiteral("local") });
        check(bad.code == 64 && bad.errorCode() == QStringLiteral("choices_mismatch"), "cli resolve wants one choice per block");
        check(cli({ QStringLiteral("resolve"), QStringLiteral("beta"), QStringLiteral("--choices"), QStringLiteral("local,mine") }).code == 64,
              "cli resolve rejects an unknown choice");
        const Run ok = cliJson({ QStringLiteral("resolve"), QStringLiteral("beta"), QStringLiteral("--choices"),
                                 QStringLiteral("local,both") });
        check(ok.code == 0
                  && readFile(QStringLiteral("Work/Beta.md"))
                         == QStringLiteral("---\napple-note-id: id-b\n---\n# Beta\nshared\nmine 1\nmiddle\nmine 2\ntheirs 2\n"),
              "cli resolve picks per block, as the version picker does");
        check(cliJson({ QStringLiteral("resolve"), QStringLiteral("beta"), QStringLiteral("--all"), QStringLiteral("remote") }).errorCode()
                  == QStringLiteral("no_conflicts"),
              "cli resolve with nothing left: no_conflicts");
        check(cliJson({ QStringLiteral("resolve"), QStringLiteral("tangle"), QStringLiteral("--all"), QStringLiteral("local") }).errorCode()
                  == QStringLiteral("conflicts_unreadable"),
              "cli resolve points unreadable markers at recover");
        const QJsonObject tangle = cliJson({ QStringLiteral("read"), QStringLiteral("tangle") }).json().toObject();
        check(tangle.value(QStringLiteral("conflicts_unreadable")).toBool() && tangle.value(QStringLiteral("has_synced_copy")).toBool(),
              "cli read flags unreadable markers and the synced copy");
        const Run strip = cliJson({ QStringLiteral("recover"), QStringLiteral("tangle"), QStringLiteral("--strip") });
        const QString backup = strip.json().toObject().value(QStringLiteral("backup")).toString();
        check(strip.code == 0
                  && readFile(QStringLiteral("Work/Tangle.md")) == QStringLiteral("---\napple-note-id: id-u\n---\n# Tangle\na\nb\nc\nd\n")
                  && backup.contains(QStringLiteral("/.icloud-md/conflict-backups/Work/")) && QFile::exists(backup),
              "cli recover --strip keeps every line, after a backup");
        writeFile(QStringLiteral("Work/Tangle.md"),
                  QStringLiteral("---\napple-note-id: id-u\n---\n# Tangle\n<<<<<<< local\n<<<<<<< local\nb\n"));
        const Run synced = cliJson({ QStringLiteral("recover"), QStringLiteral("tangle"), QStringLiteral("--synced") });
        check(synced.code == 0
                  && readFile(QStringLiteral("Work/Tangle.md")) == QStringLiteral("---\napple-note-id: id-u\n---\n# Tangle\nsynced text\n"),
              "cli recover --synced goes back to the last synced text");
        check(cliJson({ QStringLiteral("recover"), QStringLiteral("tangle"), QStringLiteral("--strip") }).errorCode()
                  == QStringLiteral("no_conflicts"),
              "cli recover on a clean note: no_conflicts");
        check(cli({ QStringLiteral("recover"), QStringLiteral("tangle") }).code == 64, "cli recover needs --strip or --synced");
    }

    // ---- folders -------------------------------------------------------------
    {
        check(cliJson({ QStringLiteral("new-folder"), QStringLiteral("Archive"), QStringLiteral("--in"), QStringLiteral("Work") })
                          .json()
                          .toObject()
                          .value(QStringLiteral("folder"))
                      == QStringLiteral("Work/Archive")
                  && QDir(g_vault + QStringLiteral("/Work/Archive")).exists(),
              "cli new-folder --in");
        check(cliJson({ QStringLiteral("new-folder"), QStringLiteral("Work") }).errorCode() == QStringLiteral("exists"),
              "cli new-folder refuses an existing one");
        const Run rf = cliJson({ QStringLiteral("rename-folder"), QStringLiteral("Work/Archive"), QStringLiteral("Old") });
        check(rf.code == 0 && QDir(g_vault + QStringLiteral("/Work/Old")).exists()
                  && rf.json().toObject().value(QStringLiteral("folder")).toString() == QStringLiteral("Work/Old"),
              "cli rename-folder");
        check(cli({ QStringLiteral("delete-folder"), QStringLiteral("Work/Old") }).code == 64, "cli delete-folder asks for --yes");
        check(cli({ QStringLiteral("delete-folder"), QStringLiteral("Work/Old"), QStringLiteral("--yes") }).code == 0
                  && !QDir(g_vault + QStringLiteral("/Work/Old")).exists(),
              "cli delete-folder --yes");
        check(cliJson({ QStringLiteral("delete-folder"), QString(), QStringLiteral("--yes") }).code == 1,
              "cli delete-folder refuses the vault root");
    }

    // ---- the vault's lock ------------------------------------------------------
    {
        VaultLock app(NotesBackend::lockPath());
        check(app.tryLock(QStringLiteral("Notes (pid 4242)")) == VaultLock::Locked, "cli test holds the lock as the window");
        const Run busy = cliJson({ QStringLiteral("new"), QStringLiteral("While open") });
        check(busy.code == 1 && busy.errorCode() == QStringLiteral("vault_busy") && busy.ms < 10000
                  && busy.error().value(QStringLiteral("message")).toString().contains(QStringLiteral("Notes (pid 4242)"))
                  && !exists(QStringLiteral("Notes/While open.md")),
              "cli refuses at once while the window holds the vault");
        check(cli({ QStringLiteral("read"), QStringLiteral("alpha") }).code == 0, "cli reading needs no lock");
        // history, diff and push --dry-run are the engine's own, and only read:
        // no lock (the engine takes it itself where it writes).
        check(cli({ QStringLiteral("history"), QStringLiteral("alpha") }).code == 0
                  && cli({ QStringLiteral("push"), QStringLiteral("--dry-run") }).code == 3,
              "cli the engine's read-only commands run while the window is open");
        const QJsonObject s = cliJson({ QStringLiteral("status") }).json().toObject().value(QStringLiteral("lock")).toObject();
        check(s.value(QStringLiteral("app_open")).toBool() && s.value(QStringLiteral("held_by")).toString() == QStringLiteral("Notes (pid 4242)"),
              "cli status names the lock's holder");
        app.release();
        check(app.tryLock(QStringLiteral("a background sync (icloud-notes --sync, pid 7)")) == VaultLock::Locked,
              "cli test holds the lock as a background sync");
        const Run waited = cliJson({ QStringLiteral("--wait"), QStringLiteral("1"), QStringLiteral("new"), QStringLiteral("Later") });
        check(waited.code == 1 && waited.errorCode() == QStringLiteral("vault_busy") && waited.ms >= 900,
              "cli waits --wait SECS for a background sync, then gives up");
        QTimer::singleShot(700, [&] { app.release(); });
        const Run after = cli({ QStringLiteral("--wait"), QStringLiteral("10"), QStringLiteral("new"), QStringLiteral("Later") });
        check(after.code == 0 && exists(QStringLiteral("Notes/Later.md")), "cli goes ahead once the lock is free");
    }

    // ---- syncing ---------------------------------------------------------------
    {
        QFile::remove(g_scratch + QStringLiteral("/stub.log"));
        const Run sync = cliJson({ QStringLiteral("sync") });
        const QJsonObject s = sync.json().toObject();
        const QJsonArray runs = s.value(QStringLiteral("runs")).toArray();
        check(sync.code == 0 && s.value(QStringLiteral("ok")).toBool() && runs.size() == 2
                  && runs.at(0).toObject().value(QStringLiteral("command")).toString() == QStringLiteral("push")
                  && runs.at(1).toObject().value(QStringLiteral("command")).toString() == QStringLiteral("pull")
                  && runs.at(1).toObject().value(QStringLiteral("exit_code")).toInt() == 0
                  && runs.at(1).toObject().value(QStringLiteral("result")).toObject().value(QStringLiteral("stub")).toString()
                         == QStringLiteral("pull")
                  && s.value(QStringLiteral("log")).toString().contains(QStringLiteral("$ icloud-notes-sync --json push"))
                  && stubLog() == QStringList{ QStringLiteral("push"), QStringLiteral("pull") },
              "cli sync: push, then pull, each with icloud-notes-sync's JSON");
        QFile::remove(g_scratch + QStringLiteral("/stub.log"));
        check(cli({ QStringLiteral("pull") }).code == 0 && stubLog() == QStringList{ QStringLiteral("pull") }, "cli pull");
        QFile::remove(g_scratch + QStringLiteral("/stub.log"));
        check(cli({ QStringLiteral("push") }).code == 0 && stubLog() == QStringList{ QStringLiteral("push") }, "cli push");

        const Run dry = cliJson({ QStringLiteral("push"), QStringLiteral("--dry-run") });
        check(dry.code == 3 && dry.json().toObject().value(QStringLiteral("entries")).toArray().size() == 1,
              "cli push --dry-run passes icloud-notes-sync's preview through, exit 3");
        check(cli({ QStringLiteral("history"), QStringLiteral("alpha") }).out.contains("changed: Notes/Alpha.md"),
              "cli history names the note by its vault path");
        check(cli({ QStringLiteral("history"), QStringLiteral("alpha"), QStringLiteral("--records") })
                  .out.contains("r1  2026-09-01T00:00:00Z  Notes/Alpha.md"),
              "cli history --records passes --records through");
        check(cli({ QStringLiteral("diff"), QStringLiteral("alpha"), QStringLiteral("e9") }).code == 3, "cli diff exit 3");
        check(cli({ QStringLiteral("restore"), QStringLiteral("alpha") }).code == 64
                  && !readFile(QStringLiteral("Notes/Alpha.md")).contains(QStringLiteral("restored")),
              "cli restore asks for --yes");
        check(cli({ QStringLiteral("restore"), QStringLiteral("alpha"), QStringLiteral("--yes") }).code == 0
                  && readFile(QStringLiteral("Notes/Alpha.md")).endsWith(QStringLiteral("restored\n")),
              "cli restore --yes");

        // A change with --push syncs after it.
        QFile::remove(g_scratch + QStringLiteral("/stub.log"));
        const Run pushed = cliJson({ QStringLiteral("new"), QStringLiteral("Pushed"), QStringLiteral("--push") });
        check(pushed.code == 0 && exists(QStringLiteral("Notes/Pushed.md"))
                  && pushed.json().toObject().value(QStringLiteral("sync")).toObject().value(QStringLiteral("ok")).toBool()
                  && stubLog() == QStringList{ QStringLiteral("push"), QStringLiteral("pull") },
              "cli new --push syncs afterwards");

        // A refused session: exit 2, and icloud-session is told.
        fake.reportCalls = 0;
        const Run refused = cliJson({ QStringLiteral("pull") }, { { QStringLiteral("ICLOUD_NOTES_SYNC_STUB_EXPIRED"), QStringLiteral("1") } });
        check(refused.code == 2 && refused.errorCode() == QStringLiteral("sign_in_required") && fake.reportCalls == 1,
              "cli pull refused by iCloud: exit 2, reported to icloud-session");

        // Signed out: nothing runs.
        fake.set({ { QStringLiteral("SignedIn"), false } });
        QFile::remove(g_scratch + QStringLiteral("/stub.log"));
        const Run out = cliJson({ QStringLiteral("sync") });
        check(out.code == 2 && out.errorCode() == QStringLiteral("sign_in_required") && stubLog().isEmpty(),
              "cli sync signed out: exit 2, icloud-notes-sync never runs");
        const Run st = cliJson({ QStringLiteral("status") });
        check(st.code == 2 && !st.json().toObject().value(QStringLiteral("signed_in")).toBool(), "cli status signed out exits 2");
        const Run local = cliJson({ QStringLiteral("write"), QStringLiteral("pushed"), QStringLiteral("--body"),
                                    QStringLiteral("# Pushed\nkept\n"), QStringLiteral("--push") });
        check(local.code == 2 && readFile(QStringLiteral("Notes/Pushed.md")) == QStringLiteral("# Pushed\nkept\n")
                  && local.json().toObject().value(QStringLiteral("action")).toString() == QStringLiteral("write"),
              "cli write --push signed out: written here, exit 2 for the push");

        // clone: into an empty vault, as the signed-in account; never a window.
        const QString fresh = g_scratch + QStringLiteral("/fresh");
        const int signIns = fake.signInCalls;
        const Run noClone = cliJson({ QStringLiteral("--vault"), fresh, QStringLiteral("clone") });
        check(noClone.code == 2 && fake.signInCalls == signIns && !QFile::exists(fresh + QStringLiteral("/.icloud-md")),
              "cli clone signed out: exit 2, no sign-in window");
        fake.set({ { QStringLiteral("SignedIn"), true } });
        const Run cloned = cliJson({ QStringLiteral("--vault"), fresh, QStringLiteral("clone") },
                                   { { QStringLiteral("ICLOUD_NOTES_SYNC_STUB_CLONE"), QStringLiteral("1") } });
        check(cloned.code == 0 && QFile::exists(fresh + QStringLiteral("/Notes/Cloned.md"))
                  && cloned.json().toObject().value(QStringLiteral("log")).toString().contains(QStringLiteral("clone --account 1006081438")),
              "cli clone the signed-in account by dsid");
        check(cliJson({ QStringLiteral("--vault"), fresh, QStringLiteral("clone") }).errorCode() == QStringLiteral("already_cloned"),
              "cli clone refuses a cloned vault");
        const Run nothing = cliJson({ QStringLiteral("--vault"), g_scratch + QStringLiteral("/none"), QStringLiteral("list") });
        check(nothing.code == 1 && nothing.errorCode() == QStringLiteral("not_cloned"), "cli list without a vault: not_cloned");
    }

    // ---- export-pdf (a GUI application, offscreen) -----------------------------
    {
        const Run pdf = cliJson({ QStringLiteral("export-pdf"), QStringLiteral("alpha") });
        check(pdf.code == 0 && QFile::exists(g_vault + QStringLiteral("/Notes/Alpha.pdf"))
                  && pdf.json().toObject().value(QStringLiteral("pdf")).toString() == g_vault + QStringLiteral("/Notes/Alpha.pdf"),
              "cli export-pdf");
        check(cliJson({ QStringLiteral("export-pdf"), QStringLiteral("alpha") }).errorCode() == QStringLiteral("exists"),
              "cli export-pdf never overwrites");
    }

    return report();
}
