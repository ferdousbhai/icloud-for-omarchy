// The pure logic no command reaches (the merge of unsaved edits with a note
// that changed on disk, the editor's special characters, the sidebar tree)
// and the pure logic the commands do reach, pinned as data: fixtures/
// syncmodel.json holds calls of SyncModel and NoteTree, each with the result
// this C++ gave. Run as a test it recomputes every result and fails on any
// that differs, so the file cannot drift from the code; a port of the logic
// reads the same file and must give the same results, to the byte.
//
// A case is {"function", "args", "result"}, one a line. To add one, write
// it without its result and run `syncmodel_fixtures --write`, which fills in
// (and rewrites) every result from the C++.
#include "../src/notetree.h"
#include "../src/syncmodel.h"
#include "check.h"

#include <QFile>
#include <QJsonArray>
#include <QJsonDocument>
#include <QJsonObject>
#include <QMap>

namespace {

QStringList strings(const QJsonValue &v)
{
    QStringList out;
    for (const QJsonValue &item : v.toArray())
        out << item.toString();
    return out;
}

QJsonArray conflictsJson(const QList<SyncModel::ConflictHunk> &hunks)
{
    QJsonArray out;
    for (const SyncModel::ConflictHunk &h : hunks)
        out << QJsonObject{ { QStringLiteral("first"), h.first },
                            { QStringLiteral("last"), h.last },
                            { QStringLiteral("local"), QJsonArray::fromStringList(h.local) },
                            { QStringLiteral("base"), QJsonArray::fromStringList(h.base) },
                            { QStringLiteral("remote"), QJsonArray::fromStringList(h.remote) } };
    return out;
}

// One call. Undefined for a function this file does not know.
QJsonValue call(const QString &fn, const QJsonArray &a)
{
    const auto s = [&](int i) { return a.at(i).toString(); };
    if (fn == u"conflictBody")
        return SyncModel::conflictBody(s(0), s(1), s(2));
    if (fn == u"twoWayConflictBody")
        return SyncModel::twoWayConflictBody(s(0), s(1), s(2));
    if (fn == u"restoreEditorChars")
        return SyncModel::restoreEditorChars(s(0), s(1));
    if (fn == u"editorForm")
        return SyncModel::editorForm(s(0));
    if (fn == u"splitEnvelope") {
        const SyncModel::EnvelopeSplit split = SyncModel::splitEnvelope(s(0));
        return QJsonObject{ { QStringLiteral("envelope"), split.envelope }, { QStringLiteral("body"), split.body } };
    }
    if (fn == u"extractNoteId")
        return SyncModel::extractNoteId(s(0));
    if (fn == u"isSyncConflictName")
        return SyncModel::isSyncConflictName(s(0));
    if (fn == u"hasConflictMarkers")
        return SyncModel::hasConflictMarkers(s(0));
    if (fn == u"parseConflicts")
        return conflictsJson(SyncModel::parseConflicts(s(0)));
    if (fn == u"hasUnreadableConflicts")
        return SyncModel::hasUnreadableConflicts(s(0));
    if (fn == u"stripConflictMarkers")
        return SyncModel::stripConflictMarkers(s(0));
    if (fn == u"resolveConflicts")
        return SyncModel::resolveConflicts(s(0), strings(a.at(1)));
    if (fn == u"linesMissingFrom") {
        QJsonArray out;
        for (const bool missing : SyncModel::linesMissingFrom(strings(a.at(0)), strings(a.at(1))))
            out << missing;
        return out;
    }
    if (fn == u"hasTable")
        return SyncModel::hasTable(s(0));
    if (fn == u"sortFolders") {
        QStringList folders = strings(a.at(0));
        SyncModel::sortFolders(folders, s(1));
        return QJsonArray::fromStringList(folders);
    }
    if (fn == u"retitleInBody")
        return SyncModel::retitleInBody(s(0), s(1));
    if (fn == u"parseStatusJson")
        return QJsonObject::fromVariantMap(SyncModel::parseStatusJson(s(0).toUtf8()));
    if (fn == u"parseHistoryJson")
        return QJsonObject::fromVariantMap(SyncModel::parseHistoryJson(s(0).toUtf8()));
    if (fn == u"stripMarkdownLead")
        return SyncModel::stripMarkdownLead(s(0));
    if (fn == u"previewNote") {
        const SyncModel::NotePreview p = SyncModel::previewNote(s(0), s(1), s(2));
        return QJsonObject{ { QStringLiteral("title"), p.title }, { QStringLiteral("snippet"), p.snippet } };
    }
    if (fn == u"toggleCheckbox")
        return SyncModel::toggleCheckbox(s(0), a.at(1).toInteger());
    if (fn == u"parentOf")
        return NoteTree::parentOf(s(0));
    if (fn == u"noteTreeRows")
        return QJsonArray::fromVariantList(NoteTree::rows(strings(a.at(0)), a.at(1).toObject().toVariantMap(),
                                                          a.at(2).toArray().toVariantList(), strings(a.at(3)), a.at(4).toBool(),
                                                          strings(a.at(5))));
    return QJsonValue::Undefined;
}

} // namespace

int main(int argc, char *argv[])
{
    // sortFolders collates by the system locale: the same one wherever this runs.
    qputenv("LC_ALL", "C.UTF-8");
    const bool write = argc > 1 && qstrcmp(argv[1], "--write") == 0;
    QFile file(QStringLiteral(SYNCMODEL_FIXTURES));
    if (!file.open(QIODevice::ReadOnly)) {
        check(false, "syncmodel fixtures: fixtures/syncmodel.json opens");
        return report();
    }
    QJsonArray cases = QJsonDocument::fromJson(file.readAll()).array();
    file.close();
    check(!cases.isEmpty(), "syncmodel fixtures: the file holds cases");

    // Per function: how many cases, and the first one whose result differs.
    QMap<QString, int> counted;
    QMap<QString, int> differing;
    for (qsizetype i = 0; i < cases.size(); ++i) {
        QJsonObject c = cases.at(i).toObject();
        const QString fn = c.value(QStringLiteral("function")).toString();
        const QJsonValue result = call(fn, c.value(QStringLiteral("args")).toArray());
        ++counted[fn];
        if (result.isUndefined() || (!write && result != c.value(QStringLiteral("result")))) {
            if (!differing.contains(fn))
                differing.insert(fn, int(i));
            continue;
        }
        c.insert(QStringLiteral("result"), result);
        cases.replace(i, c);
    }
    for (auto it = counted.cbegin(); it != counted.cend(); ++it) {
        const QString name = differing.contains(it.key())
            ? QStringLiteral("syncmodel fixtures: %1 (case %2 of the file differs)").arg(it.key()).arg(differing.value(it.key()))
            : QStringLiteral("syncmodel fixtures: %1 (%2 cases)").arg(it.key()).arg(it.value());
        check(!differing.contains(it.key()), qPrintable(name));
    }
    if (write && failures == 0) {
        // One case a line, so a changed result is one changed line.
        QByteArray text("[\n");
        for (qsizetype i = 0; i < cases.size(); ++i)
            text += QJsonDocument(cases.at(i).toObject()).toJson(QJsonDocument::Compact) + (i + 1 < cases.size() ? ",\n" : "\n");
        text += "]\n";
        if (!file.open(QIODevice::WriteOnly | QIODevice::Truncate) || file.write(text) < 0)
            check(false, "syncmodel fixtures: fixtures/syncmodel.json is written");
    }
    return report();
}
