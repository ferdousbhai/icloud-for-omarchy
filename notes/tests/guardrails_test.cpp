// Guardrail logic tests: the pure parsing/classification behind safe sync.
// Run with bin/test.
#include "../src/syncmodel.h"
#include "check.h"

int main()
{
    // extractNoteId
    check(SyncModel::extractNoteId(QStringLiteral("---\napple-note-id: 12345678-1234-1234-1234-123456789abc\n---\n# T\n"))
              == QStringLiteral("12345678-1234-1234-1234-123456789abc"),
          "id plain");
    check(SyncModel::extractNoteId(QStringLiteral("---\napple-note-id: \"abc-123\"\n---\n"))
              == QStringLiteral("abc-123"),
          "id double-quoted");
    check(SyncModel::extractNoteId(QStringLiteral("---\napple-note-id: 'abc-123'\ntags: [x]\n---\n"))
              == QStringLiteral("abc-123"),
          "id single-quoted");
    check(SyncModel::extractNoteId(QStringLiteral("---\napple-note-id: x\n---")) == QStringLiteral("x"),
          "id fence without trailing newline");
    check(SyncModel::extractNoteId(QStringLiteral("# No envelope\n")).isEmpty(), "id no envelope");
    check(SyncModel::extractNoteId(QStringLiteral("---\ntags: [x]\n---\n")).isEmpty(), "id missing key");
    check(SyncModel::extractNoteId(QStringLiteral("---\n  apple-note-id: nested\n---\n")).isEmpty(),
          "id nested key ignored");
    check(SyncModel::extractNoteId(QStringLiteral("---\napple-note-id: |\n  not a scalar\n---\n")).isEmpty(),
          "id literal block ignored");
    check(SyncModel::extractNoteId(QStringLiteral("---\nunclosed\n")).isEmpty(), "id unterminated ignored");

    // hasConflictMarkers
    check(SyncModel::hasConflictMarkers(QStringLiteral("a\n<<<<<<< local\nx\n=======\ny\n>>>>>>> remote\n")),
          "markers diff3");
    check(!SyncModel::hasConflictMarkers(QStringLiteral("# Title\n\n- [ ] task\n")), "markers clean note");
    check(!SyncModel::hasConflictMarkers(QStringLiteral("a == b\nx === y\n")), "markers equals signs");
    check(SyncModel::hasConflictMarkers(QStringLiteral("x\n||||||| base\n")), "markers ancestor");

    // readTitleMode
    check(SyncModel::readTitleMode(QByteArrayLiteral(R"({"titleMode":"filename"})")) == QStringLiteral("filename"),
          "mode filename");
    check(SyncModel::readTitleMode(QByteArrayLiteral(R"({"titleMode":"in-body"})")) == QStringLiteral("in-body"),
          "mode in-body");
    check(SyncModel::readTitleMode(QByteArrayLiteral(R"({"notes":{}})")) == QStringLiteral("in-body"),
          "mode absent defaults");
    check(SyncModel::readTitleMode(QByteArrayLiteral("not json")) == QStringLiteral("in-body"),
          "mode garbage defaults");

    // trackedFiles
    {
        const QSet<QString> t = SyncModel::trackedFiles(
            QByteArrayLiteral(R"({"notes":{"uuid-1":{"file":"A.md"},"uuid-2":{"file":"Sub/B.md"}}})"));
        check(t.size() == 2 && t.contains(QStringLiteral("A.md")) && t.contains(QStringLiteral("Sub/B.md")),
              "tracked set");
    }
    check(SyncModel::trackedFiles(QByteArrayLiteral(R"({})")).isEmpty(), "tracked empty");

    // parseStatusJson
    {
        const QByteArray payload = QByteArrayLiteral(
            R"({"entries":[{"kind":"update","file":"A.md","resolution":"ready"},{"kind":"update","file":"B.md","resolution":"refused","reason":"B.md: has attachments"},{"kind":"delete","file":"C.md","resolution":"ready"}],"unchanged":4,"notices":[{"level":"warn","message":"careful"}]})");
        const QVariantMap r = SyncModel::parseStatusJson(payload);
        check(!r.contains(QStringLiteral("error")), "status no error");
        check(r.value(QStringLiteral("entries")).toList().size() == 3, "status entry count");
        check(r.value(QStringLiteral("entries")).toList().at(1).toMap().value(QStringLiteral("reason")).toString()
                  == QStringLiteral("B.md: has attachments"),
              "status reason kept");
        check(r.value(QStringLiteral("unchanged")).toInt() == 4, "status unchanged");
        check(r.value(QStringLiteral("notices")).toStringList() == QStringList{ QStringLiteral("careful") },
              "status notices");
    }
    check(SyncModel::parseStatusJson(QByteArrayLiteral("not json")).contains(QStringLiteral("error")),
          "status garbage errors");
    check(SyncModel::parseStatusJson(QByteArrayLiteral(R"({"unchanged":1})")).contains(QStringLiteral("error")),
          "status missing entries errors");

    // parseHistoryJson
    {
        const QVariantMap r = SyncModel::parseHistoryJson(
            QByteArrayLiteral(R"({"mode":"epochs","epochs":[{"id":"e3","timestamp":"2026-09-01","changed":["Note"],"carriedOver":[]}]})"));
        check(!r.contains(QStringLiteral("error")), "history no error");
        check(r.value(QStringLiteral("epochs")).toList().size() == 1, "history epoch count");
        check(r.value(QStringLiteral("epochs")).toList().at(0).toMap().value(QStringLiteral("id")).toString()
                  == QStringLiteral("e3"),
              "history epoch id");
    }
    check(SyncModel::parseHistoryJson(QByteArrayLiteral(R"({"mode":"records"})")).contains(QStringLiteral("error")),
          "history wrong shape errors");

    // splitEnvelope
    {
        const SyncModel::EnvelopeSplit s =
            SyncModel::splitEnvelope(QStringLiteral("---\napple-note-id: x\n---\n# T\nbody\n"));
        check(s.envelope == QStringLiteral("---\napple-note-id: x\n---\n"), "envelope kept");
        check(s.body == QStringLiteral("# T\nbody\n"), "body split");
    }
    {
        const SyncModel::EnvelopeSplit s = SyncModel::splitEnvelope(QStringLiteral("# T\n"));
        check(s.envelope.isEmpty() && s.body == QStringLiteral("# T\n"), "no envelope");
    }
    check(SyncModel::splitEnvelope(QStringLiteral("---\nunclosed\n")).envelope.isEmpty(), "unterminated is body");

    // retitleInBody
    check(SyncModel::retitleInBody(QStringLiteral("# Old\nbody\n"), QStringLiteral("New"))
              == QStringLiteral("# New\nbody\n"),
          "retitle heading");
    check(SyncModel::retitleInBody(QStringLiteral("---\napple-note-id: x\n---\n# Old\nbody\n"), QStringLiteral("New"))
              == QStringLiteral("---\napple-note-id: x\n---\n# New\nbody\n"),
          "retitle keeps envelope");
    check(SyncModel::retitleInBody(QStringLiteral("Old\nbody\n"), QStringLiteral("New"))
              == QStringLiteral("New\nbody\n"),
          "retitle bare line stays bare");
    check(SyncModel::retitleInBody(QStringLiteral("---\napple-note-id: x\n---\n"), QStringLiteral("New"))
              == QStringLiteral("---\napple-note-id: x\n---\n# New\n"),
          "retitle empty body gains heading");

    // splitTitle
    check(SyncModel::splitTitle(QStringLiteral("# T\nbody\n")).titleLine == QStringLiteral("# T\n")
              && SyncModel::splitTitle(QStringLiteral("# T\nbody\n")).rest == QStringLiteral("body\n"),
          "title split heading");
    check(SyncModel::splitTitle(QStringLiteral("# T")).titleLine == QStringLiteral("# T\n"), "title split lone heading");
    check(SyncModel::splitTitle(QStringLiteral("Bare\nbody\n")).titleLine.isEmpty(), "title split bare line stays");
    check(SyncModel::splitTitle(QString()).rest.isEmpty(), "title split empty");

    // previewNote
    {
        const SyncModel::NotePreview p =
            SyncModel::previewNote(QStringLiteral("---\napple-note-id: x\n---\n# Groceries\nmilk and eggs\n"),
                                   QStringLiteral("File"), QStringLiteral("in-body"));
        check(p.title == QStringLiteral("Groceries"), "preview title heading");
        check(p.snippet == QStringLiteral("milk and eggs"), "preview snippet next line");
    }
    {
        const SyncModel::NotePreview p =
            SyncModel::previewNote(QStringLiteral("Just text\n- [ ] task one\n"), QStringLiteral("File"),
                                   QStringLiteral("in-body"));
        check(p.title == QStringLiteral("Just text"), "preview bare title");
        check(p.snippet == QStringLiteral("task one"), "preview snippet strips checkbox");
    }
    {
        const SyncModel::NotePreview p =
            SyncModel::previewNote(QStringLiteral("body only\n"), QStringLiteral("File"),
                                   QStringLiteral("filename"));
        check(p.title == QStringLiteral("File"), "preview filename mode keeps file title");
        check(p.snippet == QStringLiteral("body only"), "preview filename snippet");
    }
    {
        const SyncModel::NotePreview p =
            SyncModel::previewNote(QString(), QStringLiteral("File"), QStringLiteral("in-body"));
        check(p.title == QStringLiteral("File") && p.snippet.isEmpty(), "preview empty falls back");
    }

    // stripMarkdownLead
    check(SyncModel::stripMarkdownLead(QStringLiteral("- [ ] **milk** from [the shop](http://x) and `eggs`"))
              == QStringLiteral("milk from the shop and eggs"),
          "strip inline markup");
    check(SyncModel::stripMarkdownLead(QStringLiteral("## Title *here*")) == QStringLiteral("Title here"),
          "strip heading and emphasis");

    // toggleCheckbox
    check(SyncModel::toggleCheckbox(QStringLiteral("a\n- [ ] milk\nb"), 1) == QStringLiteral("a\n- [x] milk\nb"),
          "checkbox check");
    check(SyncModel::toggleCheckbox(QStringLiteral("- [x] milk"), 0) == QStringLiteral("- [ ] milk"),
          "checkbox uncheck");
    check(SyncModel::toggleCheckbox(QStringLiteral("  2. [X] eggs"), 0) == QStringLiteral("  2. [ ] eggs"),
          "checkbox numbered indent");
    check(SyncModel::toggleCheckbox(QStringLiteral("- milk"), 0) == QStringLiteral("- [ ] milk"),
          "checkbox plant");
    check(SyncModel::toggleCheckbox(QStringLiteral("# Title"), 0) == QStringLiteral("# Title"),
          "checkbox non-list noop");
    check(SyncModel::toggleCheckbox(QStringLiteral("a"), 5) == QStringLiteral("a"), "checkbox range noop");

    // defaultFolderDir / sortFolders
    check(SyncModel::defaultFolderDir(R"({"folders":{"DefaultFolder-CloudKit":{"name":"Notizen","dirName":"Notizen"}}})")
              == QStringLiteral("Notizen"),
          "default folder read from state");
    check(SyncModel::defaultFolderDir("{}").isEmpty(), "default folder absent");
    QStringList folders{ QStringLiteral("Work"), QStringLiteral("bets"), QStringLiteral("Notes/Old"),
                         QStringLiteral("Year 10"), QString(), QStringLiteral("Notes"), QStringLiteral("Work/A"),
                         QStringLiteral("Year 2"), QStringLiteral("Work Stuff") };
    SyncModel::sortFolders(folders, QStringLiteral("Notes"));
    check(folders == QStringList{ QString(), QStringLiteral("Notes"), QStringLiteral("Notes/Old"), QStringLiteral("bets"),
                                  QStringLiteral("Work"), QStringLiteral("Work/A"), QStringLiteral("Work Stuff"),
                                  QStringLiteral("Year 2"), QStringLiteral("Year 10") },
          "folders sort like Apple Notes");

    // restoreEditorChars
    const QString apple = QStringLiteral("Bank\u00a0Name\u2028IBAN\u2028\nBIC here");
    check(SyncModel::restoreEditorChars(apple, QStringLiteral("Bank Name\nIBAN\n\nBIC here")) == apple,
          "editor chars untouched text restored");
    check(SyncModel::restoreEditorChars(apple, QStringLiteral("Bank Name\nIBAN\n\nBIC code here"))
              == QStringLiteral("Bank\u00a0Name\u2028IBAN\u2028\nBIC code here"),
          "editor chars kept around an insertion");
    check(SyncModel::restoreEditorChars(apple, QStringLiteral("Bank Name\nBIC here"))
              == QStringLiteral("Bank\u00a0Name\u2028BIC here"),
          "editor chars kept around a deletion");
    check(SyncModel::restoreEditorChars(QStringLiteral("a\u00a0a"), QStringLiteral("a a a"))
              == QStringLiteral("a\u00a0a a"),
          "editor chars repeated text");
    check(SyncModel::restoreEditorChars(QString(), QStringLiteral("new")) == QStringLiteral("new"),
          "editor chars empty original");

    // hasTable
    check(SyncModel::hasTable(QStringLiteral("| a | b |\n| c | d |\n")), "table two rows");
    check(!SyncModel::hasTable(QStringLiteral("text\n| a |\nmore\n| b |\n")), "table broken rows ignored");
    check(!SyncModel::hasTable(QStringLiteral("a | b\nplain\n")), "table single pipe");
    check(!SyncModel::hasTable(QStringLiteral("# T\n- [ ] x\n")), "table clean note");

    return report();
}
