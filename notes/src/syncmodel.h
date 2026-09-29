#ifndef SYNCMODEL_H
#define SYNCMODEL_H

#include <QCollator>
#include <QHash>
#include <QJsonArray>
#include <QJsonDocument>
#include <QJsonObject>
#include <QRegularExpression>
#include <QSet>
#include <QString>
#include <QStringList>
#include <QVariantList>
#include <QVariantMap>

#include <algorithm>

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
inline bool isConflictMarker(QStringView line)
{
    const QStringView s = line.trimmed();
    return s.startsWith(u"<<<<<<<") || s.startsWith(u"|||||||") || s.startsWith(u">>>>>>>") || s == u"=======";
}
inline bool hasConflictMarkers(const QString &text)
{
    for (const QStringView line : QStringView(text).split(u'\n'))
        if (isConflictMarker(line))
            return true;
    return false;
}

// One block of conflict markers: the lines it spans (marker lines
// included) and its three sides. "local" is this computer's edit,
// "remote" the one from iCloud, "base" what both started from.
struct ConflictHunk {
    qsizetype first = 0;
    qsizetype last = 0;
    QStringList local;
    QStringList base;
    QStringList remote;
};

// The conflict blocks of a text, in order, over its '\n'-split lines.
// Markers out of order or left open mean the text is not a clean merge
// result, so none are returned and it stays editable only as text.
inline QList<ConflictHunk> parseConflicts(const QString &text)
{
    enum Part { Outside, Local, Base, Remote };
    const QStringList lines = text.split(u'\n');
    QList<ConflictHunk> hunks;
    ConflictHunk hunk;
    Part part = Outside;
    for (qsizetype i = 0; i < lines.size(); ++i) {
        const QString &line = lines.at(i);
        const QStringView s = QStringView(line).trimmed();
        if (s.startsWith(u"<<<<<<<")) {
            if (part != Outside)
                return {};
            hunk = ConflictHunk{ i, i, {}, {}, {} };
            part = Local;
        } else if (s.startsWith(u"|||||||")) {
            if (part != Local)
                return {};
            part = Base;
        } else if (s == u"=======") {
            if (part != Local && part != Base)
                return {};
            part = Remote;
        } else if (s.startsWith(u">>>>>>>")) {
            if (part != Remote)
                return {};
            hunk.last = i;
            hunks << hunk;
            part = Outside;
        } else if (part == Local) {
            hunk.local << line;
        } else if (part == Base) {
            hunk.base << line;
        } else if (part == Remote) {
            hunk.remote << line;
        }
    }
    return part == Outside ? hunks : QList<ConflictHunk>();
}

// The text with each conflict block replaced by the side chosen for it:
// "local", "remote" or "both" (this computer's lines, then iCloud's).
// Anything but one valid choice per block leaves the text unchanged.
inline QString resolveConflicts(const QString &text, const QStringList &choices)
{
    const QList<ConflictHunk> hunks = parseConflicts(text);
    if (hunks.isEmpty() || hunks.size() != choices.size())
        return text;
    const QStringList lines = text.split(u'\n');
    QStringList out;
    qsizetype next = 0;
    for (qsizetype h = 0; h < hunks.size(); ++h) {
        const ConflictHunk &hunk = hunks.at(h);
        const QString &choice = choices.at(h);
        if (choice != u"local" && choice != u"remote" && choice != u"both")
            return text;
        out << lines.mid(next, hunk.first - next);
        if (choice != u"remote")
            out << hunk.local;
        if (choice != u"local")
            out << hunk.remote;
        next = hunk.last + 1;
    }
    out << lines.mid(next);
    return out.join(u'\n');
}

// The text as a plain-text editor holds it (see editorChar below).
inline QChar editorChar(QChar c);
inline QString editorForm(QString text)
{
    for (QChar &c : text)
        c = editorChar(c);
    return text;
}

// The two-way fallback of conflictBody: one conflict block around the
// lines where mine and theirs part ways, with the base section only when
// base shares the lines kept around the block. Theirs keeps its own
// characters: editorForm maps one character to one, so offsets found in
// its editor form hold in the original.
inline QString twoWayConflictBody(const QString &theirs, const QString &base, const QString &mine)
{
    const QString theirsE = editorForm(theirs);
    const QStringList t = theirsE.split(u'\n'), m = mine.split(u'\n'), b = base.split(u'\n');
    const qsizetype most = qMin(t.size(), m.size());
    qsizetype head = 0;
    while (head < most && t.at(head) == m.at(head))
        ++head;
    qsizetype tail = 0;
    while (tail < most - head && t.at(t.size() - 1 - tail) == m.at(m.size() - 1 - tail))
        ++tail;
    // Where line i starts in a text split into lines, capped at its end.
    auto offset = [](const QStringList &lines, qsizetype i, qsizetype size) {
        qsizetype at = 0;
        for (qsizetype k = 0; k < i; ++k)
            at += lines.at(k).size() + 1;
        return qMin(at, size);
    };
    auto middle = [&](const QString &text, const QStringList &lines) {
        const qsizetype from = offset(lines, head, text.size());
        QString mid = text.mid(from, offset(lines, lines.size() - tail, text.size()) - from);
        if (!mid.isEmpty() && !mid.endsWith(u'\n'))
            mid += u'\n';
        return mid;
    };
    // The base section only when base shares the lines kept around the block.
    const bool baseFits = b.size() >= head + tail && b.mid(0, head) == t.mid(0, head)
        && b.mid(b.size() - tail) == t.mid(t.size() - tail);
    const qsizetype tailAt = offset(t, t.size() - tail, theirs.size());
    // A block that ends theirs without a final newline ends without one
    // too, so picking theirs gives it back byte for byte.
    const bool openEnd = tailAt >= theirs.size() && !theirs.endsWith(u'\n');
    return theirs.left(offset(t, head, theirs.size())) + u"<<<<<<< local\n" + middle(mine, m)
         + (baseFits ? u"||||||| base\n" + middle(base, b) : QString()) + u"=======\n" + middle(theirs, t)
         + (openEnd ? QStringLiteral(">>>>>>> remote") : QStringLiteral(">>>>>>> remote\n")) + theirs.mid(tailAt);
}

// Index pairs of the lines a and b share, in order (Myers' diff); defined below.
inline QList<std::pair<qsizetype, qsizetype>> matchLines(const QList<QStringView> &a, const QList<QStringView> &b,
                                                         bool &ok);
// Editor text with the original's special characters put back; defined below.
inline QString restoreEditorChars(const QString &original, const QString &edited);

// Unsaved editor text (mine) whose note changed on disk (theirs) since the
// editor loaded it (base; mine and base in editor form), merged line by
// line the way diff3 does: a stretch only one side changed takes that
// side, a stretch both changed alike takes it once, and only a stretch
// both changed differently becomes a conflict block in the shape
// parseConflicts reads (local, base, remote), to pick from instead of
// being overwritten. No block means a clean merge, free of markers.
// Theirs keeps its own characters (Apple's no-break spaces and soft
// breaks) wherever its lines are used, and a clean merge also around the
// lines taken from mine (restoreEditorChars), as a save would. A base that shares no line with
// both sides, or notes too large or too far apart to align, fall back to
// one block around where mine and theirs part ways (twoWayConflictBody).
inline QString conflictBody(const QString &theirs, const QString &base, const QString &mine)
{
    const QString theirsE = editorForm(theirs);
    const QStringList t = theirsE.split(u'\n'), m = mine.split(u'\n'), b = base.split(u'\n');
    if (t.size() + m.size() + b.size() > 30000)
        return twoWayConflictBody(theirs, base, mine);
    auto views = [](const QStringList &lines) {
        QList<QStringView> out;
        out.reserve(lines.size());
        for (const QString &line : lines)
            out << QStringView(line);
        return out;
    };
    const QList<QStringView> bv = views(b), mv = views(m), tv = views(t);
    bool okMine = false, okTheirs = false;
    const auto pairsMine = matchLines(bv, mv, okMine);
    const auto pairsTheirs = matchLines(bv, tv, okTheirs);
    if (!okMine || !okTheirs)
        return twoWayConflictBody(theirs, base, mine);
    // For each base line, the line of mine (of theirs) it stayed as, or -1.
    QList<qsizetype> inMine(b.size(), -1), inTheirs(b.size(), -1);
    for (const auto &[bi, mi] : pairsMine)
        inMine[bi] = mi;
    for (const auto &[bi, ti] : pairsTheirs)
        inTheirs[bi] = ti;
    auto anchor = [&](qsizetype bi) { return inMine.at(bi) >= 0 && inTheirs.at(bi) >= 0; };
    bool related = false;
    for (qsizetype bi = 0; bi < b.size() && !related; ++bi)
        related = anchor(bi) && !b.at(bi).trimmed().isEmpty();
    if (!related)
        return twoWayConflictBody(theirs, base, mine);

    // Theirs line by line in its own characters, with the character that
    // ended each line there ('\n', or a separator the editor form split at).
    QStringList theirsLines;
    QString theirsEnds;
    for (qsizetype k = 0, at = 0; k < t.size(); at += t.at(k).size() + 1, ++k) {
        theirsLines << theirs.mid(at, t.at(k).size());
        theirsEnds += at + t.at(k).size() < theirs.size() ? theirs.at(at + t.at(k).size()) : QChar(u'\n');
    }
    struct Line {
        QString text;
        qsizetype theirsIndex = -1; // a line of theirs, joined to the next one as theirs joins them
    };
    QList<Line> out;
    auto takeTheirs = [&](qsizetype from, qsizetype to) {
        for (qsizetype k = from; k < to; ++k)
            out << Line{ theirsLines.at(k), k };
    };
    auto take = [&](const QStringList &lines) {
        for (const QString &line : lines)
            out << Line{ line };
    };
    bool conflicted = false;
    qsizetype ib = 0, im = 0, it = 0;
    for (;;) {
        qsizetype next = ib;
        while (next < b.size() && !anchor(next))
            ++next;
        const qsizetype mEnd = next < b.size() ? inMine.at(next) : m.size();
        const qsizetype tEnd = next < b.size() ? inTheirs.at(next) : t.size();
        if (next == ib && mEnd == im && tEnd == it) { // a line nobody changed
            if (next == b.size())
                break;
            takeTheirs(it, it + 1);
            ++ib, ++im, ++it;
            continue;
        }
        const QStringList bc = b.mid(ib, next - ib), mc = m.mid(im, mEnd - im), tc = t.mid(it, tEnd - it);
        if (mc == bc || mc == tc) {
            takeTheirs(it, tEnd);
        } else if (tc == bc) {
            take(mc);
        } else {
            conflicted = true;
            out << Line{ QStringLiteral("<<<<<<< local") };
            take(mc);
            out << Line{ QStringLiteral("||||||| base") };
            take(bc);
            out << Line{ QStringLiteral("=======") };
            takeTheirs(it, tEnd);
            out << Line{ QStringLiteral(">>>>>>> remote") };
        }
        ib = next, im = mEnd, it = tEnd;
    }
    QString merged;
    for (qsizetype i = 0; i < out.size(); ++i) {
        merged += out.at(i).text;
        if (i + 1 < out.size()) {
            const qsizetype k = out.at(i).theirsIndex;
            merged += k >= 0 && out.at(i + 1).theirsIndex == k + 1 ? theirsEnds.at(k) : QChar(u'\n');
        }
    }
    // Lines taken from mine are in editor form: a soft break or no-break
    // space of theirs next to or inside them would become a paragraph or a
    // plain space. A clean merge is one text, so restore it like a save.
    return conflicted ? merged : restoreEditorChars(theirs, editorForm(merged));
}

// For each line of a, whether b lacks it (a line diff by longest common
// subsequence). Past a size cap every line counts as changed.
inline QList<bool> linesMissingFrom(const QStringList &a, const QStringList &b)
{
    const qsizetype n = a.size(), m = b.size();
    QList<bool> missing(n, true);
    if (n * m > 4'000'000)
        return missing;
    QList<int> lcs((n + 1) * (m + 1), 0);
    auto at = [&](qsizetype i, qsizetype j) -> int & { return lcs[i * (m + 1) + j]; };
    for (qsizetype i = n - 1; i >= 0; --i)
        for (qsizetype j = m - 1; j >= 0; --j)
            at(i, j) = a.at(i) == b.at(j) ? at(i + 1, j + 1) + 1 : std::max(at(i + 1, j), at(i, j + 1));
    for (qsizetype i = 0, j = 0; i < n && j < m;) {
        if (a.at(i) == b.at(j)) {
            missing[i] = false;
            ++i;
            ++j;
        } else if (at(i + 1, j) >= at(i, j + 1)) {
            ++i;
        } else {
            ++j;
        }
    }
    return missing;
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

// Directory of the account's default folder ("Notes", or whatever it was
// renamed or localized to), which Apple Notes lists first. Empty when the
// state file does not say.
inline QString defaultFolderDir(const QByteArray &stateJson)
{
    return QJsonDocument::fromJson(stateJson).object()
        .value(QStringLiteral("folders")).toObject()
        .value(QStringLiteral("DefaultFolder-CloudKit")).toObject()
        .value(QStringLiteral("dirName")).toString();
}

// Folder paths in Apple Notes order: iCloud keeps no folder positions, so
// Notes lists the default folder first and the rest by name, "2" before
// "10", each folder directly followed by its own subfolders.
inline void sortFolders(QStringList &folders, const QString &defaultDir)
{
    QCollator collator;
    collator.setNumericMode(true);
    collator.setCaseSensitivity(Qt::CaseInsensitive);
    std::sort(folders.begin(), folders.end(), [&](const QString &a, const QString &b) {
        const QStringList as = a.split(u'/', Qt::SkipEmptyParts);
        const QStringList bs = b.split(u'/', Qt::SkipEmptyParts);
        if (as.isEmpty() || bs.isEmpty())
            return as.size() < bs.size(); // the vault root heads the list
        const bool aDefault = as.first() == defaultDir, bDefault = bs.first() == defaultDir;
        if (aDefault != bDefault)
            return aDefault;
        for (qsizetype i = 0; i < qMin(as.size(), bs.size()); ++i) {
            if (const int c = collator.compare(as[i], bs[i]))
                return c < 0;
            if (const int c = as[i].compare(bs[i])) // "work" and "Work" stay in a stable order
                return c < 0;
        }
        return as.size() < bs.size();
    });
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

// Tracked notes the sync tool reads but will never push, keyed by vault-relative
// file, with its reason ("is so large that ...", phrased to follow "this
// note"). A very large note, or formatting it cannot round-trip, lands here.
inline QHash<QString, QString> readOnlyReasons(const QByteArray &stateJson)
{
    QHash<QString, QString> reasons;
    const QJsonObject index = QJsonDocument::fromJson(stateJson).object()
                                  .value(QStringLiteral("notes")).toObject();
    for (const QJsonValue &note : index) {
        const QJsonObject entry = note.toObject();
        const QString file = entry.value(QStringLiteral("file")).toString();
        const QString reason = entry.value(QStringLiteral("unpublishableReason")).toString();
        if (!file.isEmpty() && !reason.isEmpty())
            reasons.insert(file, reason);
    }
    return reasons;
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

// `icloud-notes-sync --json status`: {entries:[{kind,file,resolution,reason?,
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

// `icloud-notes-sync --json history`: {mode:"epochs",epochs:[{id,timestamp,changed[]}]}.
inline QVariantMap parseHistoryJson(const QByteArray &bytes)
{
    return arrayItems(QJsonDocument::fromJson(bytes), QStringLiteral("epochs"), QStringLiteral("history"));
}

// A plain-text editor gives back Apple's no-break spaces as spaces and its
// line and paragraph separators (U+2028/U+2029) as newlines. Writing that
// back would reformat the note in Notes (soft breaks become paragraphs), so
// every stretch the edit left alone is taken from the original instead:
// the mapping is one character for one, so positions line up.
inline QChar editorChar(QChar c)
{
    if (c == QChar::Nbsp)
        return u' ';
    if (c == QChar::LineSeparator || c == QChar::ParagraphSeparator)
        return u'\n';
    return c;
}

// Within one changed stretch: the original around the edited run.
inline QString restoreAroundEdit(QStringView original, QStringView edited)
{
    const qsizetype limit = qMin(original.size(), edited.size());
    qsizetype prefix = 0;
    while (prefix < limit && editorChar(original[prefix]) == edited[prefix])
        ++prefix;
    qsizetype suffix = 0;
    while (suffix < limit - prefix
           && editorChar(original[original.size() - 1 - suffix]) == edited[edited.size() - 1 - suffix])
        ++suffix;
    return original.left(prefix).toString() + edited.mid(prefix, edited.size() - prefix - suffix)
         + original.right(suffix);
}

// Lines split after each newline, the newline kept, so they rejoin exactly.
inline QList<QStringView> splitKeepingNewlines(QStringView text)
{
    QList<QStringView> lines;
    qsizetype start = 0;
    for (qsizetype i = 0; i < text.size(); ++i) {
        if (text[i] == u'\n') {
            lines << text.mid(start, i + 1 - start);
            start = i + 1;
        }
    }
    if (start < text.size())
        lines << text.mid(start);
    return lines;
}

// Index pairs of the lines a and b share, in order (Myers' diff). Empty with
// ok false when the two differ in more lines than is worth aligning.
inline QList<std::pair<qsizetype, qsizetype>> matchLines(const QList<QStringView> &a, const QList<QStringView> &b,
                                                         bool &ok)
{
    const qsizetype n = a.size(), m = b.size(), max = qMin<qsizetype>(n + m, 2000);
    QList<qsizetype> v(2 * max + 2, 0);
    QList<QList<qsizetype>> trace;
    auto at = [&](QList<qsizetype> &vec, qsizetype k) -> qsizetype & { return vec[k + max]; };
    ok = false;
    for (qsizetype d = 0; d <= max && !ok; ++d) {
        trace << v;
        for (qsizetype k = -d; k <= d; k += 2) {
            qsizetype x = (k == -d || (k != d && at(v, k - 1) < at(v, k + 1))) ? at(v, k + 1) : at(v, k - 1) + 1;
            qsizetype y = x - k;
            while (x < n && y < m && a[x] == b[y])
                ++x, ++y;
            at(v, k) = x;
            if (x >= n && y >= m) {
                ok = true;
                break;
            }
        }
    }
    QList<std::pair<qsizetype, qsizetype>> pairs;
    if (!ok)
        return pairs;
    qsizetype x = n, y = m;
    for (qsizetype d = trace.size() - 1; d >= 0; --d) {
        QList<qsizetype> &tv = trace[d];
        const qsizetype k = x - y;
        const qsizetype prevK = (k == -d || (k != d && at(tv, k - 1) < at(tv, k + 1))) ? k + 1 : k - 1;
        const qsizetype prevX = at(tv, prevK), prevY = prevX - prevK;
        while (x > prevX && y > prevY)
            pairs.prepend({ --x, --y });
        if (d > 0)
            x = prevX, y = prevY;
    }
    return pairs;
}

inline QString restoreEditorChars(const QString &original, const QString &edited)
{
    const bool special = std::any_of(original.cbegin(), original.cend(), [](QChar c) { return editorChar(c) != c; });
    if (!special)
        return edited;
    QString normalized = original;
    for (QChar &c : normalized)
        c = editorChar(c);

    // Lines untouched by the edit keep the original's characters; each run
    // of changed lines keeps them around its own edited stretch.
    const QList<QStringView> origLines = splitKeepingNewlines(normalized);
    const QList<QStringView> editLines = splitKeepingNewlines(edited);
    bool ok = false;
    const QList<std::pair<qsizetype, qsizetype>> pairs = matchLines(origLines, editLines, ok);
    if (!ok)
        return restoreAroundEdit(original, edited);

    auto offsetOf = [&](qsizetype line) {
        return line < origLines.size() ? origLines[line].data() - normalized.constData() : normalized.size();
    };
    auto editOffsetOf = [&](qsizetype line) {
        return line < editLines.size() ? editLines[line].data() - edited.constData() : edited.size();
    };
    QString result;
    result.reserve(edited.size());
    qsizetype oi = 0, ei = 0;
    auto flushHunk = [&](qsizetype oEnd, qsizetype eEnd) {
        const qsizetype o0 = offsetOf(oi), o1 = offsetOf(oEnd), e0 = editOffsetOf(ei), e1 = editOffsetOf(eEnd);
        result += restoreAroundEdit(QStringView(original).mid(o0, o1 - o0), QStringView(edited).mid(e0, e1 - e0));
    };
    for (const auto &[o, e] : pairs) {
        if (o > oi || e > ei)
            flushHunk(o, e);
        result += QStringView(original).mid(offsetOf(o), origLines[o].size());
        oi = o + 1;
        ei = e + 1;
    }
    if (oi < origLines.size() || ei < editLines.size())
        flushHunk(origLines.size(), editLines.size());
    return result;
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
            const QString line = lines.takeFirst();
            const QString s = isConflictMarker(line) ? QString() : stripMarkdownLead(line);
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
