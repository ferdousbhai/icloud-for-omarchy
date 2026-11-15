#ifndef NOTETREE_H
#define NOTETREE_H

#include <QHash>
#include <QSet>
#include <QString>
#include <QStringList>
#include <QVariantList>
#include <QVariantMap>

#include <functional>

// The one-pane sidebar of a narrow window: the folder list and the note list
// as one tree, flattened into the rows a ListView shows. Pure: nothing here
// touches disk or QML.
namespace NoteTree {

// The folder a folder (vault-relative, "" for the vault root) sits in.
inline QString parentOf(const QString &folder)
{
    return folder.section(u'/', 0, -2);
}

// `folders`: the backend's folder list ("" first, every parent before its
// children). `counts`: its folderNoteCounts. `notes`: every note of the vault,
// newest first, as maps with at least path (vault-relative) and folder (the
// directory it is in). `expanded`: the folders shown open ("" is All Notes).
//
// The rows: All Notes, open on every note; then each top-level folder, open
// on its subfolders, then its own notes. A folder row is {kind: "folder",
// folder, depth, count, expandable, expanded}; a note row is the note's map
// plus kind "note" and depth. While `searching`, only the notes in `matches`
// (vault-relative paths) show, and only the folders that hold one somewhere
// below them, all of them open; All Notes shows when anything matched.
// Every row carries every key (path, file, title and modifiedMs empty on a
// folder; count 0 and not expandable on a note), as a ListModel wants.
inline QVariantList rows(const QStringList &folders, const QVariantMap &counts, const QVariantList &notes,
                         const QStringList &expanded, bool searching, const QStringList &matches)
{
    const QSet<QString> open(expanded.begin(), expanded.end());
    const QSet<QString> hits(matches.begin(), matches.end());
    QVariantList shown;
    QHash<QString, QVariantList> notesIn;
    QSet<QString> holding; // folders with a shown note somewhere below them
    for (const QVariant &value : notes) {
        const QVariantMap note = value.toMap();
        if (searching && !hits.contains(note.value(QStringLiteral("path")).toString()))
            continue;
        shown << note;
        const QString folder = note.value(QStringLiteral("folder")).toString();
        notesIn[folder] << note;
        for (QString f = folder; !f.isEmpty(); f = parentOf(f))
            holding.insert(f);
    }
    QHash<QString, QStringList> childrenOf;
    for (const QString &folder : folders) {
        if (!folder.isEmpty())
            childrenOf[parentOf(folder)] << folder;
    }

    QVariantList out;
    const auto addNotes = [&](const QVariantList &list, int depth) {
        for (const QVariant &value : list) {
            QVariantMap row = value.toMap();
            row.insert(QStringLiteral("kind"), QStringLiteral("note"));
            row.insert(QStringLiteral("depth"), depth);
            row.insert(QStringLiteral("count"), 0);
            row.insert(QStringLiteral("expandable"), false);
            row.insert(QStringLiteral("expanded"), false);
            out << row;
        }
    };
    const auto addFolder = [&](const QString &folder, int depth, bool expandable) {
        const bool isOpen = expandable && (searching || open.contains(folder));
        out << QVariantMap{ { QStringLiteral("kind"), QStringLiteral("folder") },
                            { QStringLiteral("folder"), folder },
                            { QStringLiteral("depth"), depth },
                            { QStringLiteral("count"), counts.value(folder).toInt() },
                            { QStringLiteral("expandable"), expandable },
                            { QStringLiteral("expanded"), isOpen },
                            { QStringLiteral("path"), QString() },
                            { QStringLiteral("file"), QString() },
                            { QStringLiteral("title"), QString() },
                            { QStringLiteral("modifiedMs"), 0.0 } };
        return isOpen;
    };
    std::function<void(const QString &, int)> addSubtree = [&](const QString &folder, int depth) {
        if (searching && !holding.contains(folder))
            return;
        const QStringList children = childrenOf.value(folder);
        const QVariantList own = notesIn.value(folder);
        if (!addFolder(folder, depth, !children.isEmpty() || !own.isEmpty()))
            return;
        for (const QString &child : children)
            addSubtree(child, depth + 1);
        addNotes(own, depth + 1);
    };

    if (!searching || !shown.isEmpty()) {
        if (addFolder(QString(), 0, !shown.isEmpty()))
            addNotes(shown, 1);
    }
    for (const QString &top : childrenOf.value(QString()))
        addSubtree(top, 0);
    return out;
}

} // namespace NoteTree

#endif
