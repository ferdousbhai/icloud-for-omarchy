#ifndef SYNCMODEL_H
#define SYNCMODEL_H

#include <QJsonArray>
#include <QJsonDocument>
#include <QJsonObject>
#include <QRegularExpression>
#include <QSet>
#include <QString>
#include <QStringList>
#include <QVariantList>
#include <QVariantMap>

// Pure text/JSON logic behind safe sync. Nothing here touches disk or QML.
namespace SyncModel {

// Split a file into its frontmatter envelope (fences included, byte-exact)
// and the editable body. No envelope means envelope == "" and the whole
// file is body; an unterminated fence is content, not an envelope.
struct EnvelopeSplit {
    QString envelope;
    QString body;
};
inline EnvelopeSplit splitEnvelope(const QString &text)
{
    EnvelopeSplit out{ {}, text };
    qsizetype pos = 0;
    for (int line = 0;; ++line) {
        const qsizetype nl = text.indexOf(u'\n', pos);
        const qsizetype end = nl < 0 ? text.size() : nl + 1;
        if (QStringView(text).mid(pos, end - pos).trimmed() != u"---") {
            if (line == 0)
                return out;
        } else if (line > 0) {
            out.envelope = text.left(end);
            out.body = text.mid(end);
            return out;
        }
        if (nl < 0)
            return out; // unterminated: all body
        pos = end;
    }
}

// Lines of the envelope without the fences; empty when there is none.
inline QStringList frontmatterLines(const QString &text)
{
    const QString envelope = splitEnvelope(text).envelope;
    if (envelope.isEmpty())
        return {};
    QStringList lines = envelope.split(u'\n');
    lines.removeFirst(); // opening fence
    if (lines.last().isEmpty())
        lines.removeLast(); // the newline after the closing fence
    lines.removeLast(); // closing fence
    return lines;
}

// The apple-note-id (the note's CloudKit recordName, icloud-md's identity
// key) recorded in the envelope, or empty. Only a top-level scalar counts:
// a nested key or a literal/folded block is not the note's identity.
inline QString extractNoteId(const QString &text)
{
    for (const QString &raw : frontmatterLines(text)) {
        if (!raw.isEmpty() && raw.front().isSpace())
            continue; // nested key, or a line of a block scalar
        const qsizetype colon = raw.indexOf(u':');
        if (colon < 0 || raw.left(colon).trimmed() != u"apple-note-id")
            continue;
        QString value = raw.mid(colon + 1).trimmed();
        if (value.endsWith(u'|') || value.endsWith(u'>'))
            return {};
        if (value.size() >= 2 && (value.front() == u'"' || value.front() == u'\'')
            && value.back() == value.front())
            value = value.mid(1, value.size() - 2);
        return value;
    }
    return {};
}

// What mergeNoteVersions writes on overlapping edits (diff3 shape). Text
// carrying these must never be uploaded; both push and pull gate on it.
inline bool hasConflictMarkers(const QString &text)
{
    for (const QStringView line : QStringView(text).split(u'\n')) {
        const QStringView s = line.trimmed();
        if (s.startsWith(u"<<<<<<<") || s.startsWith(u"|||||||") || s.startsWith(u">>>>>>>")
            || s == u"=======")
            return true;
    }
    return false;
}

// Two or more consecutive lines carrying '|' read as a markdown table.
// Single-pipe prose lines do not count.
inline bool hasTable(const QString &text)
{
    int run = 0;
    for (const QStringView line : QStringView(text).split(u'\n')) {
        const QStringView s = line.trimmed();
        run = (s.size() > 2 && s.contains(u'|')) ? run + 1 : 0;
        if (run >= 2)
            return true;
    }
    return false;
}

// CloneState.titleMode from state.json. Absent (pre-mode vaults) or
// unrecognized means in-body, the shape that never renames files.
inline QString readTitleMode(const QByteArray &stateJson)
{
    const QJsonDocument doc = QJsonDocument::fromJson(stateJson);
    const bool filename = doc.isObject()
        && doc.object().value(QStringLiteral("titleMode")).toString() == u"filename";
    return filename ? QStringLiteral("filename") : QStringLiteral("in-body");
}

// Vault-relative paths of tracked notes from state.json's notes index
// (keyed by recordName, each carrying its file). An unreadable state
// file tracks nothing rather than misclassifying everything.
inline QSet<QString> trackedFiles(const QByteArray &stateJson)
{
    QSet<QString> files;
    const QJsonObject index = QJsonDocument::fromJson(stateJson).object()
                                  .value(QStringLiteral("notes")).toObject();
    for (const QJsonValue &note : index) {
        const QString file = note.toObject().value(QStringLiteral("file")).toString();
        if (!file.isEmpty())
            files.insert(file);
    }
    return files;
}

// Retitle for in-body vaults: replace the first body line, keeping its
// heading marks ("# ") when it had them and leaving a bare line bare.
inline QString retitleInBody(const QString &text, const QString &newTitle)
{
    const EnvelopeSplit split = splitEnvelope(text);
    if (split.body.trimmed().isEmpty())
        return split.envelope + u"# " + newTitle + u'\n';
    QStringList lines = split.body.split(u'\n');
    qsizetype hashes = 0;
    while (hashes < lines.first().size() && lines.first().at(hashes) == u'#')
        ++hashes;
    lines.first() = (hashes > 0 ? QString(hashes, u'#') + u' ' : QString()) + newTitle;
    return split.envelope + lines.join(u'\n');
}

// The objects of the array `key` in a parsed document, as maps; or an
// "error" entry. A document without the array is an error, never an
// empty plan.
inline QVariantMap arrayItems(const QJsonDocument &doc, const QString &key, const QString &what)
{
    if (!doc.isObject())
        return { { QStringLiteral("error"), what + QStringLiteral(" output is not JSON") } };
    const QJsonValue array = doc.object().value(key);
    if (!array.isArray())
        return { { QStringLiteral("error"), what + QStringLiteral(" output has no ") + key } };
    QVariantList items;
    for (const QJsonValue &v : array.toArray())
        if (v.isObject())
            items << v.toObject().toVariantMap();
    return { { key, items } };
}

// `icloud-md status --json`: {entries:[{kind,file,resolution,reason?,
// remark?,...}], unchanged, notices:[{level,message}]}.
inline QVariantMap parseStatusJson(const QByteArray &bytes)
{
    const QJsonDocument doc = QJsonDocument::fromJson(bytes);
    QVariantMap result = arrayItems(doc, QStringLiteral("entries"), QStringLiteral("status"));
    if (result.contains(QStringLiteral("error")))
        return result;
    result[QStringLiteral("unchanged")] = doc.object().value(QStringLiteral("unchanged")).toInt();
    QStringList notices;
    for (const QJsonValue &v : doc.object().value(QStringLiteral("notices")).toArray())
        notices << (v.isObject() ? v.toObject().value(QStringLiteral("message")).toString() : v.toString());
    result[QStringLiteral("notices")] = notices;
    return result;
}

// `icloud-md history --json`: {mode:"epochs",epochs:[{id,timestamp,changed[]}]}.
inline QVariantMap parseHistoryJson(const QByteArray &bytes)
{
    return arrayItems(QJsonDocument::fromJson(bytes), QStringLiteral("epochs"), QStringLiteral("history"));
}

// In-body vaults keep the title as the first line of the body. When that
// line is a heading, the editor shows only what follows it and the heading
// is edited through the title field; a bare first line stays in the editor.
struct TitleSplit {
    QString titleLine; // heading line with its newline, or empty
    QString rest;
};
inline TitleSplit splitTitle(const QString &body)
{
    if (!body.startsWith(u'#'))
        return { {}, body };
    const qsizetype nl = body.indexOf(u'\n');
    if (nl < 0)
        return { body + u'\n', {} };
    return { body.left(nl + 1), body.mid(nl + 1) };
}

// Length of a list marker ("- ", "* ", "+ ", "12. ", "3) ") at the start
// of `s`, or 0 when the line is not a list item.
inline qsizetype listMarkerLength(QStringView s)
{
    if (s.size() >= 2 && (s[0] == u'-' || s[0] == u'*' || s[0] == u'+') && s[1].isSpace())
        return 2;
    qsizetype d = 0;
    while (d < s.size() && s[d].isDigit())
        ++d;
    if (d > 0 && d + 1 < s.size() && (s[d] == u'.' || s[d] == u')') && s[d + 1].isSpace())
        return d + 2;
    return 0;
}

// Length of a "[ ]"/"[x]" checkbox at the start of `s`, or 0.
inline qsizetype checkboxLength(QStringView s)
{
    const bool box = s.size() >= 3 && s[0] == u'[' && s[2] == u']'
        && (s[1] == u' ' || s[1] == u'x' || s[1] == u'X');
    return box && (s.size() == 3 || s[3].isSpace()) ? 3 : 0;
}

// The prose of a line for titles and snippets: heading/quote marks, list
// markers and checkboxes stripped, and inline markup (emphasis, code,
// links, images) reduced to its text.
inline QString stripMarkdownLead(const QString &line)
{
    static const QRegularExpression link(QStringLiteral(R"(!?\[([^\]]*)\]\([^)]*\))"));
    static const QRegularExpression marks(QStringLiteral(R"((\*\*|__|~~|[*_`]))"));
    QString s = line.trimmed();
    while (!s.isEmpty() && (s.startsWith(u'#') || s.startsWith(u'>')))
        s = s.mid(1).trimmed();
    s = s.mid(listMarkerLength(s)).trimmed();
    s = s.mid(checkboxLength(s)).trimmed();
    s.replace(link, QStringLiteral("\\1"));
    return s.remove(marks).trimmed();
}

struct NotePreview {
    QString title;
    QString snippet;
};

// Title + snippet for the note list. In in-body vaults the first content
// line is the title and the next one the snippet; in filename vaults the
// title is the file name (fallbackTitle) and the first line the snippet.
inline NotePreview previewNote(const QString &text, const QString &fallbackTitle, const QString &mode)
{
    NotePreview p{ fallbackTitle, {} };
    QStringList lines = splitEnvelope(text).body.split(u'\n');
    auto nextProse = [&lines]() {
        while (!lines.isEmpty()) {
            const QString s = stripMarkdownLead(lines.takeFirst());
            if (!s.isEmpty())
                return s;
        }
        return QString();
    };
    if (mode != u"filename") {
        const QString title = nextProse();
        if (!title.isEmpty())
            p.title = title;
    }
    p.snippet = nextProse().left(140);
    return p;
}

// Flip (or plant) the checkbox on one 0-based line. Bare list items gain
// "[ ]"; anything else is returned unchanged.
inline QString toggleCheckbox(const QString &text, qsizetype lineIndex)
{
    QStringList lines = text.split(u'\n');
    if (lineIndex < 0 || lineIndex >= lines.size())
        return text;
    const QString &line = lines.at(lineIndex);
    qsizetype lead = 0;
    while (lead < line.size() && line.at(lead).isSpace())
        ++lead;
    const qsizetype marker = listMarkerLength(QStringView(line).mid(lead));
    if (marker == 0)
        return text;
    const qsizetype at = lead + marker;
    const QString rest = line.mid(at);
    const QString box = rest.startsWith(u"[ ] ") ? QStringLiteral("[x] ") : QStringLiteral("[ ] ");
    const bool hadBox = checkboxLength(rest) == 3 && rest.size() > 3;
    lines[lineIndex] = line.left(at) + box + (hadBox ? rest.mid(4) : rest);
    return lines.join(u'\n');
}

} // namespace SyncModel

#endif
