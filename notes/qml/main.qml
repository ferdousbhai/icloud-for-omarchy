import QtQuick
import QtQuick.Controls
import QtQuick.Dialogs
import QtQuick.Layouts
import QtCore

ApplicationWindow {
    id: root
    visible: true
    width: 1100
    height: 700
    minimumWidth: 440 // the toolbar, whole
    title: backend.currentNote.length > 0 ? noteLabel(backend.currentNote) + " - Notes" : "Notes"

    // ---- Theme: the active Omarchy palette, or the system palette off Omarchy.
    SystemPalette { id: sys }
    readonly property var theme: backend.theme
    function tone(key, fallback) { return theme[key] ? theme[key] : fallback; }
    readonly property color colBg: tone("background", sys.window)
    readonly property color colPanel: colBg
    readonly property color colSidebar: Qt.tint(colBg, Qt.alpha(colText, 0.025))
    readonly property color colRaised: Qt.tint(colBg, Qt.alpha(colText, 0.04))
    readonly property color colText: tone("foreground", sys.text)
    readonly property color colTextDim: Qt.tint(colBg, Qt.alpha(colText, 0.72))
    readonly property color colTextMuted: Qt.tint(colBg, Qt.alpha(colText, 0.55))
    readonly property color colLine: Qt.alpha(colText, 0.14)
    readonly property color colSelection: Qt.tint(colBg, Qt.alpha(colAccent, 0.12))
    readonly property color colAccent: tone("accent", sys.highlight)
    readonly property color colRed: tone("red", "#e06c75")
    readonly property color colYellow: tone("yellow", "#e5c07b")
    readonly property color colGreen: tone("green", "#98c379")
    readonly property string iconFont: backend.iconFont.length > 0 ? backend.iconFont : Qt.application.font.family
    function pt(n) { return Math.round(n * backend.uiScale); }

    // Built-in controls (fields, dialogs, menus) follow the same palette.
    palette {
        window: root.colBg
        windowText: root.colText
        base: root.colRaised
        alternateBase: root.colPanel
        text: root.colText
        button: root.colRaised
        buttonText: root.colText
        highlight: root.colAccent
        highlightedText: root.colBg
        mid: root.colLine
        dark: root.colLine
        light: root.colRaised
        placeholderText: root.colTextMuted
        toolTipBase: root.colRaised
        toolTipText: root.colText
    }
    font.family: "monospace"
    color: colBg

    // ---- State
    // What the editor was last loaded from or saved as; edits diverge from it.
    property string savedText: ""
    property bool dirty: editor.text !== savedText
    property bool filenameTitles: backend.vaultTitleMode === "filename"
    property string notice: ""
    // The sync tool reads this note but will never push it, so it opens locked.
    readonly property bool noteLocked: backend.readOnlyReason.length > 0
    // A copy Syncthing made of a note two devices changed at once: the sync
    // tool never syncs it, and the open note says so.
    readonly property bool noteIsConflictCopy: (backend.noteStates[backend.currentNote] || []).indexOf("conflict-copy") >= 0
    // A note both sides edited opens on its conflict blocks, one version
    // picked per block, instead of on the raw markers ("Edit as text").
    readonly property var conflicts: backend.noteConflicts
    property var conflictChoices: []
    property bool conflictAsText: false
    readonly property bool resolving: conflicts.length > 0 && !conflictAsText && !noteLocked
    // The formatting buttons work on the open note's text in the editor.
    readonly property bool canFormat: backend.currentNote.length > 0 && !noteLocked && !resolving
    // The note body's font zoom (Ctrl++ / Ctrl+- / Ctrl+0), kept across restarts.
    property real editorZoom: 1.0
    // Markers with no versions to pick from (nested or out of order): the
    // note opens on a way out instead, read-only until "Edit as text".
    readonly property bool unreadableConflict: backend.noteConflictsUnreadable && !conflictAsText && !noteLocked
    onConflictsChanged: conflictChoices = conflicts.map(function () { return ""; })
    // One query for both layouts' search boxes.
    property string searchText: ""
    // Read from the text itself: this runs before `searching` may have caught up.
    onSearchTextChanged: refreshSearch()
    property bool searching: searchText.trim().length >= 2
    property var searchResults: []
    // The results as vault-relative paths, which the sidebar's tree filters on.
    readonly property var searchPaths: searchResults.map(function (r) { return (r.folder ? r.folder + "/" : "") + r.file; })
    function refreshSearch() {
        if (searchText.trim().length >= 2)
            searchResults = backend.searchVault(searchText, true);
    }
    // The line under a search box: what it found, or how many notes are listed.
    function listSummary() {
        if (searching) {
            var related = searchResults.filter(function (r) { return r.related; }).length;
            var matches = searchResults.length - related;
            return matches + " result" + (matches === 1 ? "" : "s")
                + (related > 0 ? " · " + related + " related" : "")
                + (backend.searchTruncated ? " · searched the first 2000 notes" : "");
        }
        var n = backend.notes.length;
        return n + " note" + (n === 1 ? "" : "s");
    }

    // ---- Layout
    // A wide window shows the folder list and the note list as two panes
    // beside the editor; a narrow one (a half-screen split) shows one
    // sidebar instead, the folders and their notes as a tree. The two panes
    // give way to the tree before the editor would get less than a
    // comfortable width beside them at their default widths: about 60
    // characters of the note's text plus its margins (1062 px in all, so
    // half of a 1920 px screen is one-pane). Narrower than the tree's and
    // the editor's minimums together, the window shows one of them at a
    // time, and the toolbar's sidebar button switches between them.
    readonly property int folderPaneWidth: 210
    readonly property int notePaneWidth: 290
    readonly property int editorMinWidth: 320
    readonly property int editorComfortWidth: 560
    readonly property int treePaneMinWidth: 200
    readonly property bool narrow: width < folderPaneWidth + notePaneWidth + editorComfortWidth + 2
    readonly property bool cramped: narrow && width < treePaneMinWidth + editorMinWidth + 1
    property bool sidebarShown: true
    readonly property bool sidebarVisible: !narrow || sidebarShown
    readonly property bool editorVisible: !cramped || !sidebarShown
    onCrampedChanged: { if (cramped) sidebarShown = backend.currentNote.length === 0; }
    // Each layout keeps its own sidebar width.
    property bool panesReady: false
    function applySidebarWidth() {
        sidebarPane.SplitView.preferredWidth = narrow ? settings.treeWidth : settings.folderListWidth;
    }
    function keepSidebarWidth(wasNarrow) {
        if (wasNarrow)
            settings.treeWidth = sidebarPane.SplitView.preferredWidth;
        else
            settings.folderListWidth = sidebarPane.SplitView.preferredWidth;
    }
    onNarrowChanged: {
        sidebarShown = true;
        if (panesReady) {
            keepSidebarWidth(!narrow);
            applySidebarWidth();
        }
        if (narrow)
            revealCurrent();
    }

    // ---- The sidebar's tree (narrow window)
    // Open folders ("" is All Notes); the rows come from backend.noteTree.
    property var expandedFolders: []
    readonly property var treeRows: narrow ? backend.noteTree(backend.folders, backend.folderNoteCounts, backend.allNotes,
                                                              expandedFolders, searching, searchPaths) : []
    onTreeRowsChanged: syncTree(treeRows)
    ListModel { id: treeModel }
    function treeKey(row) { return row.kind + ":" + row.folder + ":" + row.path; }
    // The model follows the rows in place when only their details moved
    // (a save's new date), so the tree keeps its scroll position and the
    // row the arrows are on; otherwise it is rebuilt with both kept.
    function syncTree(rows) {
        var same = treeModel.count === rows.length;
        for (var i = 0; same && i < rows.length; i++)
            same = treeKey(treeModel.get(i)) === treeKey(rows[i]);
        if (same) {
            for (var j = 0; j < rows.length; j++) {
                var had = treeModel.get(j), row = rows[j];
                if (had.title !== row.title || had.modifiedMs !== row.modifiedMs || had.count !== row.count
                        || had.expandable !== row.expandable || had.expanded !== row.expanded)
                    treeModel.set(j, row);
            }
            return;
        }
        var current = treeView.currentIndex >= 0 && treeView.currentIndex < treeModel.count
            ? treeKey(treeModel.get(treeView.currentIndex)) : "";
        var y = treeView.contentY;
        treeModel.clear();
        var at = -1;
        for (var k = 0; k < rows.length; k++) {
            treeModel.append(rows[k]);
            if (treeKey(rows[k]) === current)
                at = k;
        }
        treeView.currentIndex = at;
        treeView.contentY = Math.max(0, Math.min(y, treeView.contentHeight - treeView.height));
    }
    function parentFolder(f) { return f.split("/").slice(0, -1).join("/"); }
    function setExpanded(folder, open) {
        var list = expandedFolders.filter(function (f) { return f !== folder; });
        if (open)
            list.push(folder);
        expandedFolders = list;
    }
    // The open note's vault-relative path, or "".
    readonly property string openPath: backend.currentNote.length === 0 ? ""
        : backend.currentFolder.length === 0 ? backend.currentNote : backend.currentFolder + "/" + backend.currentNote
    // Unfold what it takes for the open note (or, with none, the selected
    // folder) to show in the tree, then scroll to it.
    function revealCurrent() {
        var list = expandedFolders.slice();
        var add = function (f) { if (list.indexOf(f) < 0) list.push(f); };
        var folder = backend.currentFolder;
        if (backend.currentNote.length > 0 && folder.length === 0)
            add(""); // a note at the top of the vault is only under All Notes
        else if (backend.currentNote.length > 0 && list.indexOf("") < 0)
            for (var f = folder; f.length > 0; f = parentFolder(f))
                add(f);
        else if (backend.currentNote.length === 0)
            for (var p = parentFolder(folder); p.length > 0; p = parentFolder(p))
                add(p);
        if (list.length !== expandedFolders.length)
            expandedFolders = list;
        Qt.callLater(function () {
            var i = treeIndexOfCurrent();
            if (i >= 0)
                treeView.positionViewAtIndex(i, ListView.Contain);
        });
    }
    // The row of the open note (else of the selected folder), or -1.
    function treeIndexOfCurrent() {
        var folderRow = -1;
        for (var i = 0; i < treeModel.count; i++) {
            var row = treeModel.get(i);
            if (row.kind === "note" && openPath.length > 0 && row.path === openPath)
                return i;
            if (row.kind === "folder" && row.folder === backend.currentFolder && folderRow < 0)
                folderRow = i;
        }
        return folderRow;
    }
    // A click on a row (onChevron: on its arrow). A folder is selected as
    // in the folder list and unfolds, a click on the selected one folds it,
    // and the arrow only folds or unfolds; a note opens in the editor.
    function treeActivate(index, onChevron) {
        if (index < 0 || index >= treeModel.count)
            return;
        var row = treeModel.get(index);
        if (row.kind === "folder") {
            var folder = row.folder, expandable = row.expandable, expanded = row.expanded;
            if (onChevron && expandable && !searching) {
                setExpanded(folder, !expanded);
                return;
            }
            var wasSelected = folder === backend.currentFolder;
            openFolder(folder);
            if (backend.currentFolder === folder && expandable && !searching)
                setExpanded(folder, !(wasSelected && expanded));
            return;
        }
        var path = row.path;
        if (path !== openPath)
            openNote(row.folder, row.file);
        if (cramped && path === openPath)
            sidebarShown = false;
    }
    property string historyEpoch: ""
    // file → resolution, for entries the last push preview would not simply apply.
    property var statusByFile: {
        var m = {};
        for (var i = 0; i < backend.statusEntries.length; i++) {
            var e = backend.statusEntries[i];
            if (e.resolution !== "ready" && e.resolution !== "noop")
                m[e.file] = e.resolution;
        }
        return m;
    }

    // ---- Building blocks
    component Glyph: Text {
        font.family: root.iconFont
        font.pixelSize: 15
        color: root.colText
        verticalAlignment: Text.AlignVCenter
        horizontalAlignment: Text.AlignHCenter
    }
    component IconButton: AbstractButton {
        id: iconButton
        property string glyph
        property string tip
        implicitWidth: 34
        implicitHeight: 34
        Accessible.name: tip
        activeFocusOnTab: true
        hoverEnabled: true
        opacity: enabled ? 1 : 0.3
        background: Rectangle {
            radius: 6
            color: iconButton.down || iconButton.checked ? root.colSelection
                 : iconButton.hovered || iconButton.activeFocus ? root.colRaised : "transparent"
            border.width: iconButton.activeFocus ? 1 : 0
            border.color: root.colAccent
        }
        contentItem: Glyph {
            text: iconButton.glyph
            color: iconButton.checked ? root.colAccent : root.colText
        }
        ToolTip.visible: hovered && tip.length > 0
        ToolTip.text: tip
        ToolTip.delay: 500
    }
    // Basic draws a highlighted Button in palette.dark (our divider colour),
    // which reads as disabled; the one action a banner or dialog offers
    // wears the accent instead, and fades only when it really is disabled.
    component PrimaryButton: Button {
        highlighted: true
        opacity: enabled ? 1 : 0.4
        palette.dark: root.colAccent
        palette.brightText: root.colBg
    }
    component Separator: Rectangle {
        implicitWidth: 1
        implicitHeight: 18
        color: root.colLine
        opacity: 0.7
    }
    component Pill: Rectangle {
        property string label
        property color tint: root.colTextMuted
        implicitHeight: 16
        implicitWidth: pillText.implicitWidth + 12
        radius: 8
        color: Qt.rgba(tint.r, tint.g, tint.b, 0.2)
        Text {
            textFormat: Text.PlainText
            id: pillText
            anchors.centerIn: parent
            text: parent.label
            color: parent.tint
            font.pixelSize: 10
            font.bold: true
        }
    }
    // The search box of either layout: both edit root.searchText.
    component SearchBox: FocusScope {
        readonly property alias text: boxField.text
        implicitHeight: 36
        Rectangle {
            anchors.fill: parent
            radius: 6
            color: root.colRaised
            border.color: boxField.activeFocus ? root.colAccent : root.colLine
            border.width: 1
            RowLayout {
                anchors.fill: parent
                anchors.leftMargin: 10
                anchors.rightMargin: 6
                spacing: 6
                Glyph { text: ""; font.pixelSize: 12; color: root.colTextMuted }
                TextField {
                    id: boxField
                    focus: true
                    Layout.fillWidth: true
                    placeholderText: "Search notes"
                    placeholderTextColor: root.colTextMuted
                    selectByMouse: true
                    background: null
                    padding: 0
                    color: root.colText
                    font.pixelSize: root.pt(12)
                    text: root.searchText
                    onTextChanged: root.searchText = text
                    Keys.onEscapePressed: { root.searchText = ""; editor.forceActiveFocus(); }
                    Keys.onDownPressed: root.kbFocus("notes")
                }
            }
        }
    }
    // Picks a note row up to drop it on a folder (dragProxy is what moves).
    component NoteDrag: DragHandler {
        property string noteFolder
        property string noteFile
        property string noteTitle
        target: null
        acceptedButtons: Qt.LeftButton
        // Before the list takes the drag for a scroll.
        grabPermissions: PointerHandler.CanTakeOverFromAnything
        onActiveChanged: active ? dragProxy.begin(this) : dragProxy.end()
        onTranslationChanged: { if (active) dragProxy.follow(this); }
    }
    // One side of a conflict block: its lines, the ones the other side
    // lacks tinted, the whole card a click target.
    component VersionCard: AbstractButton {
        id: card
        property string label
        property string glyph
        property var lines: []
        property bool selected: false
        readonly property int changedCount: lines.filter(function (l) { return l.changed; }).length
        Layout.fillWidth: true
        Layout.fillHeight: true
        Layout.preferredWidth: 100
        hoverEnabled: true
        padding: 12
        background: Rectangle {
            radius: 10
            color: card.selected ? Qt.rgba(root.colAccent.r, root.colAccent.g, root.colAccent.b, 0.08)
                 : card.hovered ? root.colRaised : root.colPanel
            border.width: card.selected ? 2 : 1
            border.color: card.selected ? root.colAccent : root.colLine
        }
        contentItem: ColumnLayout {
            spacing: 8
            RowLayout {
                spacing: 8
                Glyph { text: card.selected ? "" : ""; color: card.selected ? root.colAccent : root.colTextMuted }
                Glyph { text: card.glyph; color: root.colTextDim; font.pixelSize: 13 }
                Label { textFormat: Text.PlainText; text: card.label; font.bold: true; color: root.colText; font.pixelSize: root.pt(13) }
                Item { Layout.fillWidth: true }
                Label {
                    textFormat: Text.PlainText
                    text: card.changedCount === 0 ? "" : card.changedCount === 1 ? "1 line differs"
                        : card.changedCount + " lines differ"
                    color: root.colTextMuted
                    font.pixelSize: root.pt(11)
                }
            }
            Label {
                Layout.fillWidth: true
                visible: card.lines.length === 0
                wrapMode: Text.Wrap
                text: "Empty. This version removed these lines."
                color: root.colTextMuted
                font.italic: true
                font.pixelSize: root.pt(12)
            }
            Repeater {
                model: card.lines
                delegate: Rectangle {
                    required property var modelData
                    Layout.fillWidth: true
                    implicitHeight: lineText.implicitHeight + 4
                    radius: 3
                    color: modelData.changed ? Qt.rgba(root.colAccent.r, root.colAccent.g, root.colAccent.b, 0.14) : "transparent"
                    Rectangle {
                        visible: parent.modelData.changed
                        width: 2
                        height: parent.height
                        radius: 1
                        color: root.colAccent
                    }
                    Text {
                        id: lineText
                        x: 8
                        y: 2
                        width: parent.width - 12
                        // The Markdown as written: rendered, a line from iCloud could
                        // load remote images. Blank lines keep their height.
                        text: parent.modelData.text.trim().length > 0 ? parent.modelData.text : " "
                        textFormat: Text.PlainText
                        wrapMode: Text.WrapAtWordBoundaryOrAnywhere
                        color: parent.modelData.changed ? root.colText : root.colTextDim
                        font.pixelSize: root.pt(13)
                    }
                }
            }
            Item { Layout.fillHeight: true }
        }
    }
    component AppDialog: Dialog {
        anchors.centerIn: parent
        modal: true
        background: Rectangle {
            color: root.colPanel
            radius: 10
            border.color: root.colLine
        }
        Overlay.modal: Rectangle { color: Qt.rgba(0, 0, 0, 0.45) }
    }
    component PromptDialog: AppDialog {
        id: prompt
        property alias placeholder: field.placeholderText
        property alias hint: hintLabel.text
        property string initial: ""
        readonly property string value: field.text.trim()
        standardButtons: Dialog.Ok | Dialog.Cancel
        ColumnLayout {
            TextField {
                id: field
                Layout.preferredWidth: 300
                onAccepted: prompt.accept()
            }
            Label {
                textFormat: Text.PlainText
                id: hintLabel
                Layout.preferredWidth: 300
                wrapMode: Text.WordWrap
                color: root.colTextMuted
                visible: text.length > 0
            }
        }
        onOpened: { field.text = initial; field.forceActiveFocus(); }
    }

    // ---- Helpers
    function noteLabel(n) { return n.replace(/\.md$/i, ""); }
    function folderLabel(f) { return f.length === 0 ? "All Notes" : f.split("/").pop(); }
    function vaultRel(name) {
        return backend.currentFolder.length === 0 ? name : backend.currentFolder + "/" + name;
    }
    function detail(name) { return backend.noteDetails[name] || {}; }
    function displayTitle(name) { return detail(name).title || noteLabel(name); }
    function shortDate(ms) { return new Date(ms).toLocaleDateString(Qt.locale(), Locale.ShortFormat); }
    // Guardrail badges: [label, tint] pairs.
    function badges(name) {
        var labels = { "conflict": ["conflict", root.colRed], "new": ["new", root.colGreen],
                       "missing-id": ["no id", root.colYellow], "foreign-id": ["foreign id", root.colYellow],
                       "conflict-copy": ["Syncthing copy, not synced", root.colYellow],
                       "tables": ["tables", root.colTextMuted], "read-only": ["read-only", root.colTextMuted] };
        var out = (backend.noteStates[name] || []).map(function (flag) { return labels[flag] || [flag, root.colTextMuted]; });
        var st = statusByFile[vaultRel(name)];
        if (st === "refused")
            out.push(["push refused", root.colRed]);
        else if (st === "conflict")
            out.push(["remote changed", root.colYellow]);
        return out;
    }

    function loadEditor() {
        // Compare against what the editor holds, not the file: TextArea turns
        // Apple's no-break spaces and U+2028 line separators into plain ones,
        // and treating that as an edit rewrote (and pushed) notes just opened.
        // A reload of the open note (a pull) keeps the cursor where it was.
        var body = backend.noteBody;
        if (editor.text !== body) {
            var pos = editor.cursorPosition;
            editor.text = body;
            editor.cursorPosition = Math.min(pos, editor.length);
        }
        savedText = editor.text;
    }
    // False when the edits could not be written: they stay in the editor,
    // or, when the note changed on disk meanwhile, in a conflict to pick from.
    function doSave() {
        var text = editor.text;
        if (!backend.saveCurrentNote(text, savedText)) {
            // Under a running sync the backend writes them once it is done.
            if (dirty)
                notice = backend.syncRunning ? "Saving once the sync is done. Your edits are still in the editor."
                                             : "Could not save the note. Your edits are still in the editor.";
            return false;
        }
        savedText = text;
        notice = "";
        return true;
    }
    // Explicit save (Ctrl+S): risky edits go through the "Save anyway?" dialog.
    function save() {
        if (backend.currentNote.length === 0)
            return;
        var w = backend.saveWarning(editor.text);
        if (w.length > 0) {
            saveWarnText.text = w;
            saveWarnDialog.open();
            return;
        }
        doSave();
    }
    // Autosave before leaving the note. Edits that fail the sync guardrails
    // are never discarded: they stay in the editor and this returns false.
    function flushEdits() {
        if (!dirty)
            return true;
        if (backend.saveWarning(editor.text).length > 0) {
            notice = "Kept your edits in the editor. Resolve the warning (Ctrl+S) before moving on.";
            return false;
        }
        return doSave();
    }
    // Closing saves what autosave has not written yet. Under a running
    // sync the save is queued, and the backend writes it as it closes; a
    // save that found the note changed on disk has merged the edits into
    // it (nothing is unsaved then). Edits a guardrail holds back stop the
    // first close, so they are not dropped unseen, and so do edits queued
    // behind another program's sync, which closing would not write;
    // closing again discards them.
    property bool closeWarned: false
    onClosing: function (close) {
        if (!dirty || closeWarned)
            return;
        var held = backend.saveWarning(editor.text).length > 0;
        if (!held && (doSave() || !dirty || backend.closeWritesQueuedSave()))
            return;
        closeWarned = true;
        close.accepted = false;
        notice = !held && backend.syncRunning
            ? "Your edits are not saved yet: another sync has the notes. Wait for it, or close again to discard them."
            : "Your edits are not saved (Ctrl+S shows why). Close again to discard them.";
    }
    function openNote(folder, name) {
        if (selectNote(folder, name))
            pullIfStale();
    }
    // Make it the open note without pulling first: for acting on it (its
    // menu, a drag), not for editing it. False when edits that cannot be
    // saved yet keep the open one.
    function selectNote(folder, name) {
        if (!flushEdits())
            return false;
        notice = "";
        backend.currentFolder = folder;
        backend.openNote(name);
        return openPath === (folder.length === 0 ? name : folder + "/" + name);
    }
    // A note's right-click menu acts on the open note, once it is that one.
    function noteMenuFor(folder, name) {
        if (selectNote(folder, name))
            noteMenu.popup();
    }
    // Move the open note to another folder (vault-relative), as a mv does:
    // the next push moves it in iCloud.
    function moveOpenNote(folder) {
        if (!flushEdits())
            return;
        if (backend.syncRunning) { // as the toolbar's buttons: never under a running sync
            notice = "Wait for the sync to finish, then move the note.";
            return;
        }
        var err = backend.moveCurrentNote(folder);
        if (err.length > 0)
            notice = err;
    }
    // A note dropped on a folder, in either layout.
    function moveNoteTo(fromFolder, name, folder) {
        if (folder !== fromFolder && selectNote(fromFolder, name))
            moveOpenNote(folder);
    }
    readonly property string noteDragKey: "application/x-icloud-notes-note"
    // The folders the open note can move to.
    function moveTargets() {
        return backend.folders.filter(function (f) { return f.length > 0 && f !== backend.currentFolder; });
    }
    // Edit the latest copy: opening a note, focusing the editor or typing
    // into an untouched note pulls first when the last pull is over a
    // minute old. The editor waits (read-only) until the pull is in, so an
    // edit never starts on a copy iCloud has already moved past.
    property bool freshening: false
    property string keptNotice: ""
    function pullIfStale() {
        if (!backend.cloned || backend.authExpired || autoSyncBlocked() || Date.now() - lastFocusSync < 60 * 1000)
            return false;
        freshening = true;
        syncNow(); // pending edits go up first, then the pull
        return true;
    }
    // What every sync that starts on its own waits for: a running sync,
    // unsaved edits, an open dialog.
    function autoSyncBlocked() { return backend.syncRunning || dirty || dialogOpen(); }
    // Both directions, and the minute the focus and stale-note syncs keep
    // between syncs counts from here.
    function syncNow() {
        lastFocusSync = Date.now();
        backend.runSync();
    }
    function openFolder(folder) {
        if (folder !== backend.currentFolder && flushEdits())
            backend.currentFolder = folder;
    }
    // A folder's right-click menu acts on the open folder: only once the
    // switch to the clicked one went through (edits that cannot be saved
    // yet keep the folder open, and the menu would rename or delete it).
    function folderMenuFor(folder) {
        root.openFolder(folder);
        if (backend.currentFolder === folder)
            folderMenu.popup();
    }

    // ----- Arrow-key navigation -----
    // kbPane tracks which pane owns the arrow keys: the folder list, the
    // note list or the editor. "" means mouse mode. Up/Down walk the
    // active list (opening rows as they pass), Right/Enter walk deeper
    // (folders -> notes -> editor) and Left/Escape walk back out.
    property string kbPane: ""

    function kbSyncIndex(view) {
        if (view.count === 0)
            return;
        if (view.currentIndex < 0 || view.currentIndex >= view.count)
            view.currentIndex = 0;
        view.positionViewAtIndex(view.currentIndex, ListView.Contain);
    }

    // Enter a pane: land on the row that is open (the current folder or
    // note, however it was opened), else keep the row the arrows left,
    // then move focus there.
    function kbFocus(pane) {
        if (narrow && pane !== "editor") { // one pane, the tree, holds both lists
            if (!sidebarVisible)
                sidebarShown = true;
            var at = treeIndexOfCurrent();
            if (at >= 0)
                treeView.currentIndex = at;
            root.kbSyncIndex(treeView);
            kbPane = "tree";
            treeView.forceActiveFocus();
            return;
        }
        var view = pane === "folders" ? folderView : noteView;
        var lookFor = pane === "folders" || (!root.searching && backend.currentNote.length > 0);
        for (var i = 0; lookFor && i < view.count; i++) {
            var md = view.model[i];
            if (pane === "folders" ? md === backend.currentFolder : rowNote(md).file === backend.currentNote) {
                view.currentIndex = i;
                break;
            }
        }
        root.kbSyncIndex(view);
        kbPane = pane;
        view.forceActiveFocus();
    }

    // A row of the note list as {folder, file}: a note of the open folder
    // (its file name), or a search result (its folder is "" at the vault root).
    function rowNote(md) { return typeof md === "string" ? { folder: backend.currentFolder, file: md } : md; }

    function kbEnterEditor() {
        if (cramped)
            sidebarShown = false;
        kbPane = "editor";
        editor.forceActiveFocus();
    }

    function kbOpenFolderAt(index) {
        if (index >= 0 && index < folderView.count)
            root.openFolder(backend.folders[index]);
    }
    function kbOpenNoteAt(index) {
        if (index < 0 || index >= noteView.count)
            return;
        var note = rowNote(noteView.model[index]);
        root.openNote(note.folder, note.file);
    }

    function kbKey(event, pane) {
        var key = event.key;
        var view = pane === "folders" ? folderView : noteView;
        var last = view.count - 1;
        if (key === Qt.Key_Up || key === Qt.Key_Down || key === Qt.Key_Home
                || key === Qt.Key_End || key === Qt.Key_PageUp || key === Qt.Key_PageDown) {
            if (view.count > 0) {
                var i = view.currentIndex < 0 ? 0 : view.currentIndex;
                if (key === Qt.Key_Up)
                    i = Math.max(0, i - 1);
                else if (key === Qt.Key_Down)
                    i = Math.min(last, i + 1);
                else if (key === Qt.Key_Home)
                    i = 0;
                else if (key === Qt.Key_End)
                    i = last;
                else if (key === Qt.Key_PageUp)
                    i = Math.max(0, i - 10);
                else
                    i = Math.min(last, i + 10);
                view.currentIndex = i;
                view.positionViewAtIndex(i, ListView.Contain);
                if (pane === "folders")
                    root.kbOpenFolderAt(i);
                else
                    root.kbOpenNoteAt(i);
            }
            event.accepted = true;
        } else if (key === Qt.Key_Right || key === Qt.Key_Return || key === Qt.Key_Enter) {
            if (pane === "folders") {
                root.kbOpenFolderAt(view.currentIndex);
                root.kbFocus("notes");
            } else if (view.count > 0) {
                var note = rowNote(view.model[Math.max(0, view.currentIndex)]);
                if (!(note.file === backend.currentNote && note.folder === backend.currentFolder))
                    root.kbOpenNoteAt(view.currentIndex);
                root.kbEnterEditor();
            }
            event.accepted = true;
        } else if (key === Qt.Key_Left || key === Qt.Key_Escape) {
            if (pane === "notes")
                root.kbFocus("folders"); // already at the root: keep the current pane
            event.accepted = true;
        }
    }
    // The tree: Up/Down walk its rows (opening the notes they pass), Right
    // unfolds a folder and then steps into it, or takes a note to the
    // editor; Enter does what a click does; Left folds a folder, or steps
    // out to the folder above.
    function kbTreeKey(event) {
        var key = event.key, last = treeModel.count - 1;
        event.accepted = true;
        if (last < 0)
            return;
        var i = Math.max(0, Math.min(treeView.currentIndex, last));
        var row = treeModel.get(i);
        var to = -1;
        if (key === Qt.Key_Up)
            to = Math.max(0, i - 1);
        else if (key === Qt.Key_Down)
            to = Math.min(last, i + 1);
        else if (key === Qt.Key_Home)
            to = 0;
        else if (key === Qt.Key_End)
            to = last;
        else if (key === Qt.Key_PageUp)
            to = Math.max(0, i - 10);
        else if (key === Qt.Key_PageDown)
            to = Math.min(last, i + 10);
        else if (key === Qt.Key_Right && row.kind === "folder") {
            if (row.expandable && !row.expanded && !searching)
                setExpanded(row.folder, true);
            else if (row.expanded && i < last)
                to = i + 1;
        } else if ((key === Qt.Key_Right || key === Qt.Key_Return || key === Qt.Key_Enter) && row.kind === "note") {
            if (row.path !== openPath)
                openNote(row.folder, row.file);
            kbEnterEditor();
        } else if (key === Qt.Key_Return || key === Qt.Key_Enter) {
            treeActivate(i, false);
        } else if (key === Qt.Key_Left) {
            if (row.kind === "folder" && row.expanded && !searching) {
                setExpanded(row.folder, false);
            } else {
                for (var up = i - 1; up >= 0; up--)
                    if (treeModel.get(up).depth < row.depth) {
                        to = up;
                        break;
                    }
            }
        } else {
            event.accepted = key === Qt.Key_Escape;
            return;
        }
        if (to < 0)
            return;
        treeView.currentIndex = to;
        treeView.positionViewAtIndex(to, ListView.Contain);
        var landed = treeModel.get(to);
        if (landed.kind === "note" && (key === Qt.Key_Up || key === Qt.Key_Down || key === Qt.Key_Home
                || key === Qt.Key_End || key === Qt.Key_PageUp || key === Qt.Key_PageDown) && landed.path !== openPath)
            openNote(landed.folder, landed.file);
    }
    // ----- end arrow-key navigation -----
    function commitTitle(title) {
        title = title.trim();
        if (title.length === 0 || title === titleField.current || !flushEdits()) {
            titleField.text = titleField.current;
            return;
        }
        if (backend.syncRunning) { // as the toolbar's buttons: never under a running sync
            notice = "Wait for the sync to finish, then rename the note.";
            titleField.text = titleField.current;
            return;
        }
        var err = backend.renameCurrentNote(title);
        if (err.length > 0) {
            notice = err;
            titleField.text = titleField.current;
        }
    }

    function chooseConflict(index, side) {
        var picked = conflictChoices.slice();
        picked[index] = side;
        conflictChoices = picked;
    }
    function chooseAllConflicts(side) { conflictChoices = conflicts.map(function () { return side; }); }
    function recoverConflictedNote(how) {
        var result = backend.recoverConflictedNote(how);
        notice = result.message;
        if (result.ok)
            loadEditor();
    }
    function applyConflictChoices() {
        var err = backend.resolveConflicts(conflictChoices);
        notice = err;
        if (err.length === 0)
            loadEditor();
    }

    // Rename: select the title text, in its field or on the first line.
    function selectTitle() {
        if (filenameTitles) {
            titleField.forceActiveFocus();
            titleField.selectAll();
            return;
        }
        var line = editor.text.split("\n")[0];
        var marks = line.match(/^#{1,6}\s+/);
        editor.forceActiveFocus();
        editor.select(marks ? marks[0].length : 0, line.length);
    }
    function trackCursor() { backend.setEditorCursor(editor.activeFocus ? editor.cursorPosition : -1); }

    function toggleTask() {
        if (root.noteLocked)
            return;
        var before = editor.text, pos = editor.cursorPosition;
        var line = before.slice(0, pos).split("\n").length - 1;
        var updated = backend.toggleCheckbox(before, line);
        if (updated === before)
            return;
        editor.text = updated;
        editor.cursorPosition = Math.min(pos + updated.length - before.length, updated.length);
    }
    function wrapSelection(before, after) {
        var s = editor.selectionStart, sel = editor.selectedText;
        if (backend.currentNote.length === 0 || root.noteLocked || sel.length === 0)
            return;
        editor.remove(s, editor.selectionEnd);
        editor.insert(s, before + sel + after);
        editor.select(s + before.length, s + before.length + sel.length);
        editor.forceActiveFocus();
    }
    function insertLink() {
        if (backend.currentNote.length === 0 || root.noteLocked)
            return;
        var s = editor.selectionStart, sel = editor.selectedText || "text", proto = "https://";
        editor.remove(s, editor.selectionEnd);
        editor.insert(s, "[" + sel + "](" + proto + ")");
        editor.select(s + sel.length + 3, s + sel.length + 3 + proto.length);
        editor.forceActiveFocus();
    }

    // Files become attachments of the open note: each is copied beside it
    // and linked on a line of its own, at `at` (a drop's place in the text)
    // or the caret, and the next sync uploads it.
    readonly property bool canAttach: canFormat && !editor.readOnly
    function attachFiles(paths, at) {
        if (!canAttach || paths.length === 0)
            return;
        var links = [], errors = [];
        paths.forEach(function (path) {
            var added = backend.addAttachment(path);
            if (added.ok)
                links.push(added.link);
            else
                errors.push(added.message);
        });
        if (links.length > 0) {
            var text = editor.text;
            // An editor never clicked into has its caret at the top: the
            // end of the note is the better place.
            var pos = at !== undefined ? at : editor.activeFocus || editor.cursorPosition > 0 ? editor.selectionEnd : text.length;
            if (at !== undefined) {
                var lineEnd = text.indexOf("\n", pos);
                pos = lineEnd < 0 ? text.length : lineEnd;
            }
            var block = (pos > 0 && text.charAt(pos - 1) !== "\n" ? "\n" : "") + links.join("\n")
                + (text.charAt(pos) === "\n" ? "" : "\n");
            editor.insert(pos, block);
            editor.cursorPosition = pos + block.length;
        }
        notice = errors.join(" ");
        editor.forceActiveFocus();
    }
    function localPaths(urls) {
        return urls.map(function (url) { return backend.localFile(url); }).filter(function (path) { return path.length > 0; });
    }

    function zoomIn() { editorZoom = Math.min(4.0, editorZoom * 1.2); }
    function zoomOut() { editorZoom = Math.max(0.5, editorZoom / 1.2); }
    function zoomReset() { editorZoom = 1.0; }

    // ---- Toolbar
    header: Rectangle {
        height: 50
        color: root.colPanel
        RowLayout {
            anchors.fill: parent
            anchors.leftMargin: 10
            anchors.rightMargin: 10
            spacing: 2

            IconButton {
                id: sidebarButton
                visible: root.narrow
                glyph: "\uf0c9"; tip: root.sidebarShown ? "Hide the sidebar" : "Show the sidebar"
                checked: root.sidebarShown
                onClicked: root.sidebarShown = !root.sidebarShown
            }
            Separator { visible: root.narrow; Layout.leftMargin: 6; Layout.rightMargin: 6 }
            IconButton { glyph: "\uf07b"; tip: "New folder"; enabled: !backend.syncRunning; onClicked: newFolderDialog.open() }
            IconButton { glyph: "\uf044"; tip: "New note (Ctrl+N)"; enabled: !backend.syncRunning; onClicked: newNoteDialog.open() }
            Separator { Layout.leftMargin: 6; Layout.rightMargin: 6 }
            IconButton {
                glyph: "\uf1f8"; tip: "Delete note"
                enabled: backend.currentNote.length > 0 && !backend.syncRunning
                onClicked: deleteDialog.open()
            }
            IconButton {
                glyph: "\uf040"; tip: "Rename note"
                enabled: backend.currentNote.length > 0 && !root.noteLocked && !backend.syncRunning
                onClicked: root.selectTitle()
            }
            Item { Layout.fillWidth: true }
            IconButton { glyph: "\uf046"; tip: "Checklist (Ctrl+Enter)"; enabled: root.canFormat; onClicked: root.toggleTask() }
            // Very narrow, these three keep only their shortcuts, so the toolbar fits.
            IconButton { visible: !root.cramped; glyph: "\uf032"; tip: "Bold (Ctrl+B)"; enabled: root.canFormat; onClicked: root.wrapSelection("**", "**") }
            IconButton { visible: !root.cramped; glyph: "\uf033"; tip: "Italic (Ctrl+I)"; enabled: root.canFormat; onClicked: root.wrapSelection("*", "*") }
            IconButton { visible: !root.cramped; glyph: "\uf0c1"; tip: "Link (Ctrl+K)"; enabled: root.canFormat; onClicked: root.insertLink() }
            IconButton { glyph: "\uf0c6"; tip: "Attach a file (or drop or paste one)"; enabled: root.canAttach; onClicked: attachDialog.open() }
            Separator { Layout.leftMargin: 6; Layout.rightMargin: 6 }
            IconButton {
                // Sync is automatic; this only shows what is pending or refused.
                glyph: "\uf021"; tip: "Sync status"
                enabled: backend.cloned && !backend.syncRunning
                onClicked: { if (root.flushEdits()) backend.refreshPushPreview(); }
            }
            IconButton {
                id: moreButton
                glyph: "\uf141"; tip: "More"
                onClicked: moreMenu.open()
                Menu {
                    id: moreMenu
                    y: moreButton.height + 4
                    x: moreButton.width - width
                MenuItem {
                    text: "Note history"
                    enabled: backend.currentNote.length > 0 && backend.cloned && !backend.syncRunning
                    onTriggered: backend.runHistory()
                }
                MenuItem {
                    text: "Export PDF"
                    enabled: backend.currentNote.length > 0
                    onTriggered: {
                        if (!root.flushEdits())
                            return;
                        var err = backend.exportPdf();
                        if (err.length > 0)
                            root.notice = err;
                    }
                }
                MenuSeparator {}
                MenuItem { text: "Refresh"; onTriggered: backend.refresh() }
                MenuItem { text: "Sync log"; onTriggered: logDialog.open() }
                }
            }
        }
        Rectangle { anchors.bottom: parent.bottom; width: parent.width; height: 1; color: root.colLine; opacity: 0.6 }
    }

    // ---- Status line
    footer: Rectangle {
        height: 26
        color: root.colPanel
        Rectangle { width: parent.width; height: 1; color: root.colLine; opacity: 0.6 }
        RowLayout {
            anchors.fill: parent
            anchors.leftMargin: 14
            anchors.rightMargin: 14
            spacing: 8
            Glyph {
                text: "\uf021"
                font.pixelSize: 11
                color: root.colTextMuted
                visible: backend.syncRunning
                RotationAnimation on rotation { from: 0; to: 360; duration: 1200; loops: Animation.Infinite; running: backend.syncRunning }
            }
            Label {
                textFormat: Text.PlainText
                text: backend.syncMessage
                color: root.colTextMuted
                font.pixelSize: 11
                elide: Text.ElideRight
                Layout.fillWidth: true
            }
            Label {
                textFormat: Text.PlainText
                visible: root.notice.length > 0
                text: root.notice
                color: root.colAccent
                font.pixelSize: 11
                elide: Text.ElideRight
                Layout.maximumWidth: 480
            }
            Label {
                visible: root.dirty
                text: "● unsaved"
                color: root.colYellow
                font.pixelSize: 11
            }
        }
    }

    // ---- Panes
    ColumnLayout {
        anchors.fill: parent
        spacing: 0

        Rectangle {
            visible: !backend.syncToolAvailable || !backend.cloned
            Layout.fillWidth: true
            Layout.margins: 10
            implicitHeight: bannerRow.implicitHeight + 20
            radius: 8
            color: root.colRaised
            RowLayout {
                id: bannerRow
                anchors.fill: parent
                anchors.margins: 10
                spacing: 12
                Glyph { text: backend.syncToolAvailable ? "\uf0c2" : "\uf071"; color: backend.syncToolAvailable ? root.colAccent : root.colYellow }
                Label {
                    textFormat: Text.PlainText
                    Layout.fillWidth: true
                    wrapMode: Text.WordWrap
                    color: root.colTextDim
                    text: backend.syncToolAvailable
                          ? "This folder is not linked to iCloud yet. Clone to download your Apple Notes."
                          : "The sync engine (icloud-notes-sync) is missing. Reinstall icloud-notes (sudo pacman -S icloud-notes) and restart to enable sync."
                }
                PrimaryButton {
                    visible: backend.syncToolAvailable
                    text: "Clone…"
                    enabled: !backend.syncRunning
                    onClicked: onboardDialog.open()
                }
            }
        }

        // The notes folder could not move to ~/Documents/Notes: a folder of
        // the user's own is there. Gone once they move it and restart.
        Rectangle {
            visible: backend.vaultNotice.length > 0
            Layout.fillWidth: true
            Layout.margins: 10
            implicitHeight: vaultNoticeRow.implicitHeight + 20
            radius: 8
            color: root.colRaised
            RowLayout {
                id: vaultNoticeRow
                anchors.fill: parent
                anchors.margins: 10
                spacing: 12
                Glyph { text: ""; color: root.colYellow }
                Label {
                    textFormat: Text.PlainText
                    Layout.fillWidth: true
                    wrapMode: Text.WordWrap
                    color: root.colTextDim
                    text: backend.vaultNotice
                }
            }
        }

        // A push refused because most of the notes' files are gone: it would
        // move that many notes to Recently Deleted, which only a person
        // decides. Nothing was pushed, and nothing is until they say so here
        // (the syncs that run on their own never do) or the files are back.
        Rectangle {
            visible: backend.deletionsRefused.length > 0
            Layout.fillWidth: true
            Layout.margins: 10
            implicitHeight: deletionsRow.implicitHeight + 20
            radius: 8
            color: root.colRaised
            RowLayout {
                id: deletionsRow
                anchors.fill: parent
                anchors.margins: 10
                spacing: 12
                Glyph { text: "\uf071"; color: root.colYellow }
                Label {
                    textFormat: Text.PlainText
                    Layout.fillWidth: true
                    wrapMode: Text.WordWrap
                    color: root.colTextDim
                    text: backend.deletionsRefused + " If the files went missing by accident (a folder moved, a disk not mounted), put them back and Notes syncs as before."
                }
                Button {
                    text: "Show them"
                    enabled: !backend.syncRunning
                    onClicked: { if (root.flushEdits()) backend.refreshPushPreview(); }
                }
                Button {
                    text: "Delete them…"
                    enabled: !backend.syncRunning
                    onClicked: deleteMissingDialog.open()
                }
            }
        }

        // One banner for the sign-in: expired, too short to last (a phone QR
        // sign-in never offers "Keep me signed in"), or lapsing within days.
        Rectangle {
            id: signInBanner
            readonly property int daysLeft: backend.signInDaysLeft
            readonly property bool shortSignIn: backend.cloned && backend.syncToolAvailable && daysLeft === -1 // -2: unknown
            readonly property string howTo: "In Apple's window, use your Apple ID and password (not the iPhone QR code), tick Keep me signed in, and click Trust."
            visible: backend.authExpired || shortSignIn || (daysLeft >= 0 && daysLeft <= 5)
            Layout.fillWidth: true
            Layout.margins: 10
            implicitHeight: authRow.implicitHeight + 20
            radius: 8
            color: root.colRaised
            RowLayout {
                id: authRow
                anchors.fill: parent
                anchors.margins: 10
                spacing: 12
                Glyph { text: "\uf071"; color: root.colYellow }
                Label {
                    textFormat: Text.PlainText
                    Layout.fillWidth: true
                    wrapMode: Text.WordWrap
                    color: root.colTextDim
                    text: backend.authExpired
                          ? "Your iCloud sign-in expired, so syncing is paused. Your edits are safe on this computer and go up once you sign in. " + signInBanner.howTo
                          : signInBanner.shortSignIn
                          ? "This iCloud sign-in only lasts a few hours unused. To stay signed in, sign in again. " + signInBanner.howTo
                          : "Your iCloud sign-in ends " + (signInBanner.daysLeft === 0 ? "today" : signInBanner.daysLeft === 1 ? "tomorrow" : "in " + signInBanner.daysLeft + " days")
                            + ". Sign in again now to keep syncing without a pause."
                }
                PrimaryButton {
                    text: backend.signingIn ? "Signing in…" : "Sign in"
                    enabled: !backend.signingIn
                    onClicked: backend.signIn()
                }
            }
        }

        SplitView {
            id: panes
            Layout.fillWidth: true
            Layout.fillHeight: true
            orientation: Qt.Horizontal
            handle: Rectangle { implicitWidth: 1; color: root.colLine; opacity: 0.6 }
            Component.onCompleted: {
                if (settings.panes)
                    restoreState(settings.panes);
                root.applySidebarWidth();
                root.panesReady = true;
            }

            // Folders, or on a narrow window the tree of folders and notes
            Rectangle {
                id: sidebarPane
                visible: root.sidebarVisible
                SplitView.preferredWidth: root.folderPaneWidth
                SplitView.minimumWidth: root.narrow ? root.treePaneMinWidth : 150
                SplitView.fillWidth: !root.editorVisible
                color: root.colSidebar
                ColumnLayout {
                    anchors.fill: parent
                    visible: !root.narrow
                    spacing: 0
                    Label {
                        Layout.leftMargin: 18
                        Layout.topMargin: 14
                        Layout.bottomMargin: 6
                        text: "iCloud"
                        color: root.colTextMuted
                        font.pixelSize: 11
                        font.bold: true
                        font.capitalization: Font.AllUppercase
                        font.letterSpacing: 0.5
                    }
                    ListView {
                        id: folderView
                        Layout.fillWidth: true
                        Layout.fillHeight: true
                        model: backend.folders
                        clip: true
                        spacing: 1
                        keyNavigationEnabled: false // arrows are handled by root.kbKey
                        property bool kbActive: root.kbPane === "folders"
                        Keys.onPressed: function (event) { root.kbKey(event, "folders"); }
                        delegate: Item {
                            id: folderRow
                            width: folderView.width
                            height: 30
                            property bool selected: modelData === backend.currentFolder
                            property int depth: modelData.length === 0 ? 0 : modelData.split("/").length - 1
                            HoverHandler { id: folderHover }
                            DropArea {
                                id: folderDrop
                                anchors.fill: parent
                                enabled: modelData.length > 0
                                keys: [root.noteDragKey]
                                onDropped: function (drop) { root.moveNoteTo(drop.source.noteFolder, drop.source.noteFile, modelData); }
                            }
                            Rectangle {
                                anchors.fill: parent
                                anchors.leftMargin: 8
                                anchors.rightMargin: 8
                                radius: 6
                                border.width: folderDrop.containsDrag ? 1 : 0
                                border.color: root.colAccent
                                color: folderRow.selected || folderDrop.containsDrag ? root.colSelection
                                     : folderHover.hovered ? Qt.rgba(root.colSelection.r, root.colSelection.g, root.colSelection.b, 0.5)
                                     : folderView.kbActive && folderView.currentIndex === index ? Qt.rgba(root.colSelection.r, root.colSelection.g, root.colSelection.b, 0.35)
                                     : "transparent"
                            }
                            RowLayout {
                                anchors.fill: parent
                                anchors.leftMargin: 18 + folderRow.depth * 14
                                anchors.rightMargin: 18
                                spacing: 8
                                Glyph { text: modelData.length === 0 ? "\uf07c" : "\uf07b"; font.pixelSize: 13; color: root.colAccent }
                                Label {
                                    textFormat: Text.PlainText
                                    Layout.fillWidth: true
                                    elide: Text.ElideRight
                                    text: root.folderLabel(modelData)
                                    color: root.colText
                                    font.pixelSize: root.pt(13)
                                }
                                Label {
                                    textFormat: Text.PlainText
                                    text: backend.folderNoteCounts[modelData] || ""
                                    color: root.colTextMuted
                                    font.pixelSize: 11
                                }
                            }
                            TapHandler { acceptedButtons: Qt.LeftButton; onTapped: { root.kbPane = "folders"; folderView.currentIndex = index; folderView.forceActiveFocus(); root.openFolder(modelData); } }
                            TapHandler {
                                acceptedButtons: Qt.RightButton
                                enabled: modelData.length > 0
                                onTapped: root.folderMenuFor(modelData)
                            }
                        }
                    }
                }

                // The tree: All Notes, then each folder, unfolding in place
                // on its subfolders and notes.
                ColumnLayout {
                    anchors.fill: parent
                    visible: root.narrow
                    spacing: 0
                    SearchBox {
                        id: treeSearch
                        Layout.fillWidth: true
                        Layout.margins: 10
                    }
                    Label {
                        textFormat: Text.PlainText
                        visible: root.searching
                        Layout.leftMargin: 16
                        Layout.bottomMargin: 4
                        text: treeModel.count === 0 ? "No matching notes" : root.listSummary()
                        color: root.colTextMuted
                        font.pixelSize: 11
                    }
                    ListView {
                        id: treeView
                        Layout.fillWidth: true
                        Layout.fillHeight: true
                        model: treeModel
                        clip: true
                        keyNavigationEnabled: false // arrows are handled by root.kbTreeKey
                        property bool kbActive: root.kbPane === "tree"
                        Keys.onPressed: function (event) { root.kbTreeKey(event); }
                        ScrollBar.vertical: ScrollBar {}
                        delegate: Item {
                            id: treeRow
                            required property int index
                            required property var model
                            readonly property string kind: model.kind
                            readonly property string folder: model.folder
                            readonly property string path: model.path
                            readonly property string file: model.file
                            readonly property string title: model.title
                            readonly property real modifiedMs: model.modifiedMs
                            readonly property int depth: model.depth
                            readonly property int count: model.count
                            readonly property bool expandable: model.expandable
                            readonly property bool expanded: model.expanded
                            readonly property bool isFolder: kind === "folder"
                            readonly property bool selected: isFolder ? folder === backend.currentFolder
                                                                      : path === root.openPath
                            // A selected folder with a note open is marked by its label alone.
                            readonly property bool filled: selected && (!isFolder || backend.currentNote.length === 0)
                            readonly property real indent: 8 + depth * 14
                            width: treeView.width
                            height: 28
                            HoverHandler { id: treeHover }
                            DropArea {
                                id: treeDrop
                                anchors.fill: parent
                                enabled: treeRow.isFolder && treeRow.folder.length > 0
                                keys: [root.noteDragKey]
                                onDropped: function (drop) { root.moveNoteTo(drop.source.noteFolder, drop.source.noteFile, treeRow.folder); }
                            }
                            Rectangle {
                                anchors.fill: parent
                                anchors.leftMargin: 6
                                anchors.rightMargin: 6
                                radius: 6
                                border.width: treeDrop.containsDrag ? 1 : 0
                                border.color: root.colAccent
                                color: treeRow.filled || treeDrop.containsDrag ? root.colSelection
                                     : treeHover.hovered ? Qt.rgba(root.colSelection.r, root.colSelection.g, root.colSelection.b, 0.5)
                                     : treeView.kbActive && treeView.currentIndex === treeRow.index ? Qt.rgba(root.colSelection.r, root.colSelection.g, root.colSelection.b, 0.35)
                                     : "transparent"
                            }
                            RowLayout {
                                anchors.fill: parent
                                anchors.leftMargin: treeRow.indent
                                anchors.rightMargin: 14
                                spacing: 6
                                Glyph {
                                    Layout.preferredWidth: 12
                                    text: !treeRow.expandable ? "" : treeRow.expanded ? "" : ""
                                    font.pixelSize: 9
                                    color: root.colTextMuted
                                }
                                Glyph {
                                    visible: treeRow.isFolder
                                    text: treeRow.folder.length === 0 || treeRow.expanded ? "" : ""
                                    font.pixelSize: 13
                                    color: root.colAccent
                                }
                                Label {
                                    textFormat: Text.PlainText
                                    Layout.fillWidth: true
                                    elide: Text.ElideRight
                                    text: treeRow.isFolder ? root.folderLabel(treeRow.folder)
                                                           : treeRow.title || root.noteLabel(treeRow.file)
                                    color: treeRow.selected && !treeRow.filled ? root.colAccent : root.colText
                                    font.pixelSize: root.pt(13)
                                }
                                Label {
                                    textFormat: Text.PlainText
                                    text: treeRow.isFolder ? (treeRow.count || "")
                                                           : treeRow.modifiedMs > 0 ? root.shortDate(treeRow.modifiedMs) : ""
                                    color: root.colTextMuted
                                    font.pixelSize: 11
                                }
                            }
                            TapHandler {
                                acceptedButtons: Qt.LeftButton
                                onTapped: function (point) {
                                    root.kbPane = "tree";
                                    treeView.currentIndex = treeRow.index;
                                    treeView.forceActiveFocus();
                                    root.treeActivate(treeRow.index, point.position.x < treeRow.indent + 18);
                                }
                            }
                            TapHandler {
                                acceptedButtons: Qt.RightButton
                                enabled: !treeRow.isFolder || treeRow.folder.length > 0
                                onTapped: treeRow.isFolder ? root.folderMenuFor(treeRow.folder)
                                                           : root.noteMenuFor(treeRow.folder, treeRow.file)
                            }
                            NoteDrag {
                                enabled: !treeRow.isFolder
                                noteFolder: treeRow.folder
                                noteFile: treeRow.file
                                noteTitle: treeRow.title
                            }
                        }
                    }
                }
            }

            // Notes
            Rectangle {
                visible: !root.narrow
                SplitView.preferredWidth: root.notePaneWidth
                SplitView.minimumWidth: 200
                color: root.colPanel
                ColumnLayout {
                    anchors.fill: parent
                    spacing: 0
                    Label {
                        textFormat: Text.PlainText
                        Layout.fillWidth: true
                        Layout.leftMargin: 20
                        Layout.rightMargin: 20
                        Layout.topMargin: 12
                        elide: Text.ElideRight
                        text: root.folderLabel(backend.currentFolder)
                        color: root.colText
                        font.pixelSize: root.pt(17)
                        font.bold: true
                    }
                    SearchBox {
                        id: searchField
                        Layout.fillWidth: true
                        Layout.margins: 12
                        Layout.topMargin: 8
                    }
                    Label {
                        textFormat: Text.PlainText
                        Layout.leftMargin: 20
                        Layout.bottomMargin: 4
                        text: root.listSummary()
                        color: root.colTextMuted
                        font.pixelSize: 11
                    }
                    ListView {
                        id: noteView
                        Layout.fillWidth: true
                        Layout.fillHeight: true
                        model: root.searching ? root.searchResults : backend.notes
                        clip: true
                        keyNavigationEnabled: false // arrows are handled by root.kbKey
                        property bool kbActive: root.kbPane === "notes"
                        Keys.onPressed: function (event) { root.kbKey(event, "notes"); }
                        delegate: Item {
                            id: noteRow
                            width: noteView.width
                            height: noteColumn.implicitHeight + 18
                            property bool isResult: typeof modelData !== "string"
                            property string folder: root.rowNote(modelData).folder
                            property string fileName: root.rowNote(modelData).file
                            property bool selected: fileName === backend.currentNote && folder === backend.currentFolder
                            property var pills: isResult ? [] : root.badges(fileName)
                            property bool firstRelated: isResult && modelData.related === true
                                && (index === 0 || noteView.model[index - 1].related !== true)
                            HoverHandler { id: noteHover }
                            Rectangle {
                                anchors.fill: parent
                                anchors.leftMargin: 8
                                anchors.rightMargin: 8
                                radius: 8
                                color: noteRow.selected ? root.colSelection
                                     : noteHover.hovered ? Qt.rgba(root.colSelection.r, root.colSelection.g, root.colSelection.b, 0.5)
                                     : noteView.kbActive && noteView.currentIndex === index ? Qt.rgba(root.colSelection.r, root.colSelection.g, root.colSelection.b, 0.35)
                                     : "transparent"
                            }
                            Rectangle {
                                anchors.bottom: parent.bottom
                                anchors.left: parent.left
                                anchors.right: parent.right
                                anchors.leftMargin: 20
                                anchors.rightMargin: 20
                                height: 1
                                color: root.colLine
                                opacity: noteRow.selected ? 0 : 0.5
                            }
                            ColumnLayout {
                                id: noteColumn
                                anchors.left: parent.left
                                anchors.right: parent.right
                                anchors.verticalCenter: parent.verticalCenter
                                anchors.leftMargin: 20
                                anchors.rightMargin: 20
                                spacing: 3
                                Label {
                                    Layout.bottomMargin: 6
                                    visible: noteRow.firstRelated
                                    text: "Related by meaning"
                                    color: root.colTextDim
                                    font.pixelSize: root.pt(11)
                                    font.bold: true
                                }
                                Label {
                                    textFormat: Text.PlainText
                                    Layout.fillWidth: true
                                    elide: Text.ElideRight
                                    text: noteRow.isResult ? modelData.title : root.displayTitle(noteRow.fileName)
                                    color: root.colText
                                    font.pixelSize: root.pt(13)
                                    font.bold: true
                                }
                                RowLayout {
                                    Layout.fillWidth: true
                                    spacing: 8
                                    Label {
                                        textFormat: Text.PlainText
                                        visible: text.length > 0
                                        text: noteRow.isResult ? (modelData.folder || "All Notes")
                                                               : ((root.detail(noteRow.fileName).modifiedMs || 0) > 0
                                                                  ? root.shortDate(root.detail(noteRow.fileName).modifiedMs) : "")
                                        color: root.colTextDim
                                        font.pixelSize: root.pt(11)
                                    }
                                    Label {
                                        textFormat: Text.PlainText
                                        Layout.fillWidth: true
                                        elide: Text.ElideRight
                                        text: noteRow.isResult ? (modelData.snippet || "") : (root.detail(noteRow.fileName).snippet || "")
                                        color: root.colTextMuted
                                        font.pixelSize: root.pt(11)
                                    }
                                }
                                Flow {
                                    Layout.fillWidth: true
                                    visible: noteRow.pills.length > 0
                                    spacing: 4
                                    Repeater {
                                        model: noteRow.pills
                                        Pill { label: modelData[0]; tint: modelData[1] }
                                    }
                                }
                            }
                            TapHandler {
                                onTapped: {
                                    root.kbPane = "notes";
                                    noteView.currentIndex = index;
                                    noteView.forceActiveFocus();
                                    if (!noteRow.selected)
                                        root.openNote(noteRow.folder, noteRow.fileName);
                                }
                            }
                            TapHandler {
                                acceptedButtons: Qt.RightButton
                                onTapped: root.noteMenuFor(noteRow.folder, noteRow.fileName)
                            }
                            NoteDrag {
                                noteFolder: noteRow.folder
                                noteFile: noteRow.fileName
                                noteTitle: noteRow.isResult ? modelData.title : root.displayTitle(noteRow.fileName)
                            }
                        }
                    }
                }
            }

            // Editor
            Rectangle {
                visible: root.editorVisible
                SplitView.fillWidth: true
                SplitView.minimumWidth: root.editorMinWidth
                color: root.colBg

                ColumnLayout {
                    anchors.centerIn: parent
                    visible: backend.currentNote.length === 0
                    spacing: 10
                    Glyph { Layout.alignment: Qt.AlignHCenter; text: "\uf24a"; font.pixelSize: 40; color: root.colLine }
                    Label {
                        Layout.alignment: Qt.AlignHCenter
                        text: "Select a note, or create a new one."
                        color: root.colTextMuted
                        font.pixelSize: root.pt(13)
                    }
                }

                ColumnLayout {
                    anchors.fill: parent
                    visible: backend.currentNote.length > 0
                    spacing: 0

                    Label {
                        textFormat: Text.PlainText
                        Layout.fillWidth: true
                        Layout.topMargin: 16
                        horizontalAlignment: Text.AlignHCenter
                        color: root.colTextMuted
                        font.pixelSize: root.pt(11)
                        text: {
                            if (root.freshening)
                                return "Getting the latest from iCloud…";
                            var ms = root.detail(backend.currentNote).modifiedMs || 0;
                            if (ms <= 0)
                                return "";
                            var d = new Date(ms);
                            return d.toLocaleDateString(Qt.locale(), Locale.LongFormat)
                                + " at " + d.toLocaleTimeString(Qt.locale(), Locale.ShortFormat);
                        }
                    }

                    // In-body vaults keep the title as the note's first line, edited
                    // in the editor like Typora. Filename vaults title by file name,
                    // edited here; committing it renames the file.
                    TextInput {
                        id: titleField
                        Layout.fillWidth: true
                        Layout.leftMargin: 32
                        Layout.rightMargin: 32
                        Layout.topMargin: 12
                        visible: root.filenameTitles
                        property string current: root.displayTitle(backend.currentNote)
                        onCurrentChanged: text = current
                        text: current
                        color: root.colText
                        selectionColor: root.colAccent
                        selectedTextColor: root.colBg
                        font.pixelSize: root.pt(24)
                        font.bold: true
                        selectByMouse: true
                        clip: true
                        readOnly: root.noteLocked
                        onEditingFinished: root.commitTitle(text)
                        Keys.onReturnPressed: editor.forceActiveFocus()
                        Keys.onEnterPressed: editor.forceActiveFocus()
                        Keys.onEscapePressed: { text = current; editor.forceActiveFocus(); }
                    }
                    Label {
                        textFormat: Text.PlainText
                        Layout.fillWidth: true
                        Layout.leftMargin: 32
                        Layout.rightMargin: 32
                        Layout.topMargin: 10
                        visible: root.noteIsConflictCopy
                        wrapMode: Text.Wrap
                        horizontalAlignment: Text.AlignHCenter
                        color: root.colYellow
                        font.pixelSize: root.pt(11)
                        text: "This is a Syncthing conflict copy, so it is not synced. Merge what you want from it into the note and delete it."
                    }
                    Label {
                        textFormat: Text.PlainText
                        Layout.fillWidth: true
                        Layout.leftMargin: 32
                        Layout.rightMargin: 32
                        Layout.topMargin: 10
                        visible: root.noteLocked
                        wrapMode: Text.Wrap
                        horizontalAlignment: Text.AlignHCenter
                        color: root.colYellow
                        font.pixelSize: root.pt(11)
                        text: "Read-only here: this note " + backend.readOnlyReason
                            + ". Edit it in Apple Notes, and the changes still sync here."
                    }

                    Rectangle {
                        Layout.fillWidth: true
                        Layout.leftMargin: 32
                        Layout.rightMargin: 32
                        Layout.topMargin: 14
                        implicitHeight: 70
                        visible: backend.noteAttachments.length > 0
                        radius: 8
                        color: root.colRaised
                        RowLayout {
                            anchors.fill: parent
                            anchors.margins: 8
                            spacing: 10
                            Glyph { text: "\uf0c6"; color: root.colTextMuted }
                            ListView {
                                Layout.fillWidth: true
                                Layout.fillHeight: true
                                orientation: ListView.Horizontal
                                model: backend.noteAttachments
                                spacing: 10
                                clip: true
                                delegate: ColumnLayout {
                                    spacing: 2
                                    Image {
                                        Layout.preferredWidth: 46
                                        Layout.preferredHeight: 36
                                        Layout.alignment: Qt.AlignHCenter
                                        fillMode: Image.PreserveAspectFit
                                        source: modelData.url
                                        visible: modelData.image
                                    }
                                    Glyph { Layout.alignment: Qt.AlignHCenter; text: "\uf016"; visible: !modelData.image; font.pixelSize: 22; color: root.colTextDim }
                                    Label {
                                        textFormat: Text.PlainText
                                        Layout.maximumWidth: 90
                                        elide: Text.ElideMiddle
                                        text: modelData.name
                                        color: root.colTextMuted
                                        font.pixelSize: 10
                                    }
                                }
                            }
                            Label {
                                visible: !editor.readOnly
                                text: "Delete a link to remove its attachment"
                                color: root.colTextMuted
                                font.pixelSize: 10
                            }
                        }
                    }

                    ScrollView {
                        id: conflictView
                        Layout.fillWidth: true
                        Layout.fillHeight: true
                        Layout.topMargin: 14
                        visible: root.resolving
                        contentWidth: availableWidth
                        ColumnLayout {
                            width: conflictView.availableWidth
                            spacing: 20

                            Rectangle {
                                Layout.fillWidth: true
                                Layout.leftMargin: 32
                                Layout.rightMargin: 32
                                implicitHeight: conflictBannerRow.implicitHeight + 24
                                radius: 10
                                color: Qt.rgba(root.colYellow.r, root.colYellow.g, root.colYellow.b, 0.12)
                                RowLayout {
                                    id: conflictBannerRow
                                    anchors.fill: parent
                                    anchors.margins: 12
                                    spacing: 12
                                    Glyph { Layout.alignment: Qt.AlignTop; text: "\uf071"; color: root.colYellow; font.pixelSize: 18 }
                                    ColumnLayout {
                                        Layout.fillWidth: true
                                        spacing: 2
                                        Label {
                                            text: "This note changed in two places"
                                            font.bold: true
                                            color: root.colText
                                            font.pixelSize: root.pt(13)
                                        }
                                        Label {
                                            Layout.fillWidth: true
                                            wrapMode: Text.Wrap
                                            textFormat: Text.PlainText
                                            color: root.colTextDim
                                            font.pixelSize: root.pt(12)
                                            text: "It was edited on this computer and in iCloud before the two could sync. "
                                                + (root.conflicts.length === 1 ? "Pick the version to keep."
                                                   : "Pick the version to keep for each of the " + root.conflicts.length + " changes.")
                                        }
                                    }
                                }
                            }

                            Repeater {
                                model: root.conflicts
                                delegate: ColumnLayout {
                                    id: hunk
                                    required property var modelData
                                    required property int index
                                    readonly property string choice: root.conflictChoices[index] || ""
                                    Layout.fillWidth: true
                                    Layout.leftMargin: 32
                                    Layout.rightMargin: 32
                                    spacing: 8
                                    Label {
                                        textFormat: Text.PlainText
                                        visible: root.conflicts.length > 1
                                        text: "Change " + (hunk.index + 1) + " of " + root.conflicts.length
                                        color: root.colTextMuted
                                        font.pixelSize: root.pt(11)
                                        font.bold: true
                                    }
                                    Label {
                                        textFormat: Text.PlainText
                                        Layout.fillWidth: true
                                        visible: hunk.modelData.before.length > 0
                                        text: hunk.modelData.before.join("\n")
                                        elide: Text.ElideRight
                                        color: root.colTextMuted
                                        font.pixelSize: root.pt(12)
                                    }
                                    GridLayout {
                                        Layout.fillWidth: true
                                        columns: width >= 560 ? 2 : 1
                                        columnSpacing: 12
                                        rowSpacing: 12
                                        VersionCard {
                                            label: "This computer"
                                            glyph: "\uf109"
                                            lines: hunk.modelData.local
                                            selected: hunk.choice === "local"
                                            onClicked: root.chooseConflict(hunk.index, "local")
                                        }
                                        VersionCard {
                                            label: "iCloud"
                                            glyph: "\uf0c2"
                                            lines: hunk.modelData.remote
                                            selected: hunk.choice === "remote"
                                            onClicked: root.chooseConflict(hunk.index, "remote")
                                        }
                                    }
                                    AbstractButton {
                                        id: bothButton
                                        hoverEnabled: true
                                        padding: 4
                                        contentItem: RowLayout {
                                            spacing: 8
                                            Glyph {
                                                text: hunk.choice === "both" ? "\uf058" : "\uf10c"
                                                color: hunk.choice === "both" ? root.colAccent : root.colTextMuted
                                            }
                                            Label {
                                                text: "Keep both, this computer's version first"
                                                color: bothButton.hovered || hunk.choice === "both" ? root.colText : root.colTextDim
                                                font.pixelSize: root.pt(12)
                                            }
                                        }
                                        onClicked: root.chooseConflict(hunk.index, "both")
                                    }
                                    Label {
                                        textFormat: Text.PlainText
                                        Layout.fillWidth: true
                                        visible: hunk.modelData.after.length > 0
                                        text: hunk.modelData.after.join("\n")
                                        elide: Text.ElideRight
                                        color: root.colTextMuted
                                        font.pixelSize: root.pt(12)
                                    }
                                }
                            }

                            RowLayout {
                                Layout.fillWidth: true
                                Layout.leftMargin: 32
                                Layout.rightMargin: 32
                                Layout.bottomMargin: 32
                                spacing: 8
                                Button { text: "Edit as text"; flat: true; onClicked: root.conflictAsText = true }
                                Item { Layout.fillWidth: true }
                                Button {
                                    visible: root.conflicts.length > 1
                                    text: "All from this computer"
                                    onClicked: root.chooseAllConflicts("local")
                                }
                                Button {
                                    visible: root.conflicts.length > 1
                                    text: "All from iCloud"
                                    onClicked: root.chooseAllConflicts("remote")
                                }
                                PrimaryButton {
                                    text: "Keep selected"
                                    enabled: !backend.syncRunning
                                        && root.conflictChoices.every(function (c) { return c.length > 0; })
                                    onClicked: root.applyConflictChoices()
                                }
                            }
                        }
                    }

                    Rectangle {
                        Layout.fillWidth: true
                        Layout.leftMargin: 32
                        Layout.rightMargin: 32
                        Layout.topMargin: 14
                        visible: root.unreadableConflict
                        implicitHeight: unreadableColumn.implicitHeight + 24
                        radius: 10
                        color: Qt.rgba(root.colYellow.r, root.colYellow.g, root.colYellow.b, 0.12)
                        ColumnLayout {
                            id: unreadableColumn
                            anchors.fill: parent
                            anchors.margins: 12
                            spacing: 8
                            RowLayout {
                                spacing: 12
                                Glyph { Layout.alignment: Qt.AlignTop; text: "\uf071"; color: root.colYellow; font.pixelSize: 18 }
                                ColumnLayout {
                                    Layout.fillWidth: true
                                    spacing: 2
                                    Label {
                                        text: "This note has conflict markers Notes can't read."
                                        font.bold: true
                                        color: root.colText
                                        font.pixelSize: root.pt(13)
                                    }
                                    Label {
                                        Layout.fillWidth: true
                                        wrapMode: Text.Wrap
                                        textFormat: Text.PlainText
                                        color: root.colTextDim
                                        font.pixelSize: root.pt(12)
                                        text: "\"Remove the markers\" keeps every line from both versions and drops only the marker lines, for you to read over. "
                                            + (backend.noteHasSyncedCopy ? "\"Use the last synced version\" goes back to the text last synced with iCloud. " : "")
                                            + "Before either, the note as it is now is copied to .icloud-notes/conflict-backups in your notes folder, so nothing is lost."
                                    }
                                }
                            }
                            RowLayout {
                                Layout.fillWidth: true
                                spacing: 8
                                Button { text: "Edit as text"; flat: true; onClicked: root.conflictAsText = true }
                                Item { Layout.fillWidth: true }
                                Button {
                                    visible: backend.noteHasSyncedCopy
                                    enabled: !backend.syncRunning && !root.dirty
                                    text: "Use the last synced version"
                                    onClicked: root.recoverConflictedNote("synced")
                                }
                                PrimaryButton {
                                    enabled: !backend.syncRunning && !root.dirty
                                    text: "Remove the markers"
                                    onClicked: root.recoverConflictedNote("strip")
                                }
                            }
                        }
                    }

                    ScrollView {
                        Layout.fillWidth: true
                        Layout.fillHeight: true
                        Layout.topMargin: 8
                        visible: !root.resolving
                        TextArea {
                            id: editor
                            Keys.onPressed: function (event) {
                                // A copied file or picture is pasted as an attachment.
                                if (event.matches(StandardKey.Paste) && root.canAttach) {
                                    var files = backend.clipboardFiles();
                                    if (files.length > 0) {
                                        event.accepted = true;
                                        root.attachFiles(files);
                                        return;
                                    }
                                }
                                // The first edit after a while waits for the pull; the
                                // key that triggered it is dropped rather than typed
                                // into a copy about to change.
                                var edits = event.text.length > 0 || event.key === Qt.Key_Backspace
                                    || event.key === Qt.Key_Delete;
                                if (edits && !root.dirty && root.pullIfStale())
                                    event.accepted = true;
                            }
                            // Arrow keys move the caret here (TextArea default);
                            // Escape walks back out to the note list.
                            Keys.onEscapePressed: function (event) {
                                event.accepted = true;
                                root.kbFocus("notes");
                            }
                            wrapMode: TextArea.Wrap
                            selectByMouse: true
                            readOnly: root.noteLocked || root.freshening || root.unreadableConflict
                            background: null
                            color: root.colText
                            selectionColor: root.colAccent
                            selectedTextColor: root.colBg
                            font.pixelSize: root.pt(14 * root.editorZoom)
                            leftPadding: 32
                            rightPadding: 32
                            topPadding: 4
                            bottomPadding: 32
                            placeholderText: "Start writing…"
                            onTextChanged: { root.closeWarned = false; autosave.restart(); }
                            onCursorPositionChanged: root.trackCursor()
                            onActiveFocusChanged: {
                                root.trackCursor();
                                if (activeFocus) {
                                    root.kbPane = "editor";
                                    root.pullIfStale();
                                }
                            }
                            Component.onCompleted: backend.attachEditor(editor.textDocument)

                            // Files dropped on the note are attached where they land.
                            DropArea {
                                anchors.fill: parent
                                keys: ["text/uri-list"]
                                onEntered: function (drag) { drag.accepted = root.canAttach && drag.hasUrls; }
                                onDropped: function (drop) {
                                    var paths = root.localPaths(drop.urls);
                                    if (paths.length === 0)
                                        return;
                                    drop.accept(Qt.CopyAction);
                                    root.attachFiles(paths, editor.positionAt(drop.x, drop.y));
                                }
                            }
                        }
                    }
                }
            }
        }
    }

    FileDialog {
        id: attachDialog
        title: "Attach files"
        fileMode: FileDialog.OpenFiles
        currentFolder: StandardPaths.writableLocation(StandardPaths.HomeLocation)
        onAccepted: root.attachFiles(root.localPaths(selectedFiles))
    }

    // Notes has no Save button: edits land on disk once typing pauses. Edits
    // a guardrail refuses stay in the editor and are flagged in the footer.
    Timer {
        id: autosave
        interval: 1500
        onTriggered: {
            if (backend.currentNote.length === 0)
                return;
            // Under a running sync the backend writes nothing: it keeps the
            // text and writes it once the sync is done (or the window
            // closes). An edit undone meanwhile takes that back.
            if (!root.dirty)
                backend.dropQueuedSave();
            else if (backend.saveWarning(editor.text).length === 0)
                root.doSave();
        }
    }

    function dialogOpen() {
        return [previewDialog, logDialog, historyDialog, saveWarnDialog, newNoteDialog,
                newFolderDialog, renameFolderDialog, deleteFolderDialog, deleteDialog, deleteMissingDialog, onboardDialog, moveDialog]
            .some(function (d) { return d.visible; });
    }

    // Like Notes, changes reach iCloud on their own. The sync tool keeps this
    // safe: it merges, refuses a note it cannot push safely (the badges say
    // which; Sync status shows why), and deletions only move notes to Recently
    // Deleted. Pushes wait until edits settle so a burst of typing is one push.
    Timer {
        id: autoPush
        interval: 20 * 1000
        onTriggered: {
            if (!backend.cloned || backend.authExpired)
                return;
            if (root.autoSyncBlocked()) {
                restart();
                return;
            }
            backend.runPush();
        }
    }
    // Changes from other devices come in the moment you switch to the
    // window, at most once a minute so flipping between windows does not
    // sync on every flip, and on a timer (below) while it stays in front.
    property double lastFocusSync: 0
    onActiveChanged: {
        if (active)
            backend.refreshSignIn(); // the day count moves on; also keeps icloud-session validating
        if (!active || !backend.cloned || backend.authExpired)
            return;
        if (root.autoSyncBlocked() || autoPush.running || Date.now() - lastFocusSync < 60 * 1000)
            return;
        root.syncNow(); // both directions: a change from any program in any folder
    }

    // iCloud does not tell a web client that something changed, so a phone
    // edit made while Notes stays open is fetched by polling: every minute
    // while the window is active, every 15 minutes (like the background
    // timer) while it is not. A pull that finds nothing is three small
    // requests. Never faster than once a minute, and slower after each
    // failed pull, up to 15 minutes, so a bad network or an outage is not
    // retried at full rate.
    property int pollFailures: 0
    Timer {
        id: poll
        interval: root.active ? Math.min(60 * 1000 * Math.pow(2, root.pollFailures), 15 * 60 * 1000)
                              : 15 * 60 * 1000
        repeat: true
        running: backend.cloned && !backend.authExpired
        onTriggered: {
            if (root.autoSyncBlocked() || autoPush.running)
                return;
            root.syncNow(); // pending edits go up first, then the pull
        }
    }

    // Sync on startup, like Notes does on launch, once icloud-session has
    // said whether anyone is signed in (without it, sync anyway: the
    // sign-in is unknown, not missing). Without a vault, an account already
    // signed in on this machine is cloned quietly; only a device with no
    // sign-in sees the dialog.
    property bool started: false
    function start() {
        if (started || backend.signInPending)
            return;
        started = true;
        if (backend.cloned) {
            if (backend.syncToolAvailable && !backend.authExpired)
                root.syncNow(); // edits made while the app was closed go up first; the window's first activation is this sync
        } else if (backend.syncToolAvailable && backend.signedIn) {
            root.notice = "Downloading your notes as " + backend.appleId + "…";
            backend.runClone();
        } else {
            onboardDialog.open();
        }
    }
    // Arm the arrow-key navigation from a cold start: the first Up/Down
    // lands on the folder list unless something (an onboarding dialog, a
    // search) already holds the focus.
    function kbArmStartup() {
        if (!activeFocusItem || activeFocusItem === contentItem)
            root.kbFocus(backend.folders.length > 0 ? "folders" : "notes");
    }
    Component.onCompleted: {
        start();
        if (narrow)
            revealCurrent();
        Qt.callLater(root.kbArmStartup);
    }

    Shortcut { sequences: [StandardKey.Save]; onActivated: root.save() }
    Shortcut { sequence: "Ctrl+N"; enabled: !backend.syncRunning; onActivated: newNoteDialog.open() }
    Shortcut { sequence: "Ctrl+B"; onActivated: root.wrapSelection("**", "**") }
    Shortcut { sequence: "Ctrl+I"; onActivated: root.wrapSelection("*", "*") }
    Shortcut { sequence: "Ctrl+K"; onActivated: root.insertLink() }
    Shortcut { sequence: "Ctrl+Return"; enabled: editor.activeFocus; onActivated: root.toggleTask() }
    Shortcut { sequences: [StandardKey.ZoomIn, "Ctrl+="]; onActivated: root.zoomIn() }
    Shortcut { sequences: [StandardKey.ZoomOut]; onActivated: root.zoomOut() }
    Shortcut { sequence: "Ctrl+0"; onActivated: root.zoomReset() }

    Settings {
        id: settings
        property alias windowWidth: root.width
        property alias windowHeight: root.height
        property var panes
        property alias editorZoom: root.editorZoom
        // The sidebar's width in each layout, and the tree's open folders.
        property real folderListWidth: root.folderPaneWidth
        property real treeWidth: 260
        property alias expandedFolders: root.expandedFolders
    }
    Component.onDestruction: {
        keepSidebarWidth(narrow);
        settings.panes = panes.saveState();
    }
    Shortcut {
        sequences: [StandardKey.Find]
        onActivated: {
            if (root.narrow && !root.sidebarShown)
                root.sidebarShown = true;
            (root.narrow ? treeSearch : searchField).forceActiveFocus();
        }
    }

    // Menus of a right-clicked row, in either layout.
    Menu {
        id: folderMenu
        MenuItem { text: "Rename folder…"; onTriggered: renameFolderDialog.open() }
        MenuItem { text: "Delete folder…"; onTriggered: deleteFolderDialog.open() }
    }
    Menu {
        id: noteMenu
        MenuItem {
            text: "Rename note"
            enabled: !root.noteLocked && !backend.syncRunning
            onTriggered: {
                if (root.cramped)
                    root.sidebarShown = false; // the title is in the editor
                root.selectTitle();
            }
        }
        MenuItem {
            text: "Move to…"
            enabled: !root.noteLocked && !backend.syncRunning && root.moveTargets().length > 0
            onTriggered: moveDialog.open()
        }
        MenuSeparator {}
        MenuItem {
            text: "Delete note…"
            enabled: !backend.syncRunning
            onTriggered: deleteDialog.open()
        }
    }

    // What a dragged note row shows under the pointer; dropped on a folder
    // (a folder list row, or a folder of the tree), it moves the note there.
    Rectangle {
        id: dragProxy
        property string noteFolder
        property string noteFile
        property point origin
        visible: Drag.active
        z: 100
        width: Math.min(dragLabel.implicitWidth + 20, 260)
        height: 28
        radius: 6
        color: root.colRaised
        border.color: root.colAccent
        Drag.keys: [root.noteDragKey]
        Drag.hotSpot.x: 10
        Drag.hotSpot.y: 14
        Label {
            id: dragLabel
            textFormat: Text.PlainText
            anchors.fill: parent
            anchors.leftMargin: 10
            anchors.rightMargin: 10
            verticalAlignment: Text.AlignVCenter
            elide: Text.ElideRight
            color: root.colText
            font.pixelSize: root.pt(12)
        }
        function begin(handler) {
            noteFolder = handler.noteFolder;
            noteFile = handler.noteFile;
            dragLabel.text = handler.noteTitle;
            var p = handler.parent.mapToItem(parent, handler.centroid.pressPosition.x, handler.centroid.pressPosition.y);
            origin = Qt.point(p.x, p.y);
            follow(handler);
            Drag.active = true;
        }
        function follow(handler) {
            x = origin.x + handler.translation.x - Drag.hotSpot.x;
            y = origin.y + handler.translation.y - Drag.hotSpot.y;
        }
        function end() {
            if (!Drag.active)
                return;
            Drag.drop();
            Drag.active = false;
        }
    }

    Connections {
        target: backend
        // "~ query" shows Recall's ranking alone; a plain query keeps its
        // text matches and gains the rest of Recall's hits below them.
        function onSemanticSearchFinished(query, hits, available) {
            if (root.searchText !== query || !root.searching) return;
            if (query.trim().startsWith("~")) {
                if (available) root.searchResults = hits;
                root.notice = available ? "Matches by meaning · local Recall" : "Recall unavailable or warming up · showing text matches";
                return;
            }
            if (!available) return;
            var shown = {};
            root.searchResults.forEach(function (r) { shown[r.folder + "/" + r.file] = true; });
            var related = hits.filter(function (h) { return !shown[h.folder + "/" + h.file]; })
                              .map(function (h) { return Object.assign({ related: true }, h); });
            if (related.length > 0)
                root.searchResults = root.searchResults.concat(related);
        }
        function onSyncFinished(label, ok) {
            if (label === "Pull")
                root.pollFailures = ok ? 0 : Math.min(root.pollFailures + 1, 4);
        }
        function onNoteContentChanged() {
            if (!root.dirty) {
                root.loadEditor();
                return;
            }
            // The note changed under unsaved edits (a pull, another program).
            // Saving would overwrite that change, so the two are merged:
            // edits to different lines land in the note as it is, and only
            // lines both sides changed open as versions to pick from.
            // Never under a running sync: the refresh after it comes back here.
            if (backend.syncRunning)
                return;
            // A note moved elsewhere is followed there; one deleted keeps
            // the edits as a new note (editsKeptAsNote). The reloads that
            // merge makes come back here, and it answers those with false.
            root.keptNotice = "";
            if (backend.keepEditsAsConflict(root.savedText, editor.text)) {
                root.loadEditor();
                root.notice = root.keptNotice.length > 0 ? root.keptNotice
                    : backend.noteConflicts.length === 0 ? "Merged your edits with a change from another device." : "";
            }
        }
        // What a save under a sync asked for is on disk now, merged with
        // whatever the sync changed: show that, unless typing went on.
        function onQueuedSaveWritten(body) {
            if (editor.text === body) {
                root.loadEditor();
                root.notice = "";
                return;
            }
            // Typing went on: the edits now start from what was queued. A
            // sync that changed the note too is merged with them; otherwise
            // they stay unsaved, on top of the text just written.
            if (backend.keepEditsAsConflict(body, editor.text))
                root.loadEditor();
            else
                root.savedText = body;
        }
        function onEditsKeptAsNote(message) {
            root.keptNotice = message;
        }
        function onNotesChanged() { root.refreshSearch(); }
        function onFoldersChanged() { root.refreshSearch(); }
        function onCurrentNoteChanged() {
            root.conflictAsText = false;
            if (root.cramped && backend.currentNote.length === 0)
                root.sidebarShown = true; // nothing to edit: the tree
            if (root.narrow)
                root.revealCurrent();
        }
        function onCurrentFolderChanged() {
            if (root.narrow)
                root.revealCurrent();
        }
        // The poll counts from the end of the last sync, whatever started it
        // (a focus, a push, Pull), so it never follows one within the interval.
        // The editor is unlocked here and not on syncRunningChanged: that
        // turns false between runSync's push and its pull, too early.
        function onSyncChainFinished() {
            if (poll.running)
                poll.restart();
            root.freshening = false;
        }
        function onCurrentNoteChangedOnDisk() {
            // Unsaved edits are kept apart from the change (onNoteContentChanged),
            // after any running sync is done writing.
            if (!root.dirty || !backend.syncRunning)
                backend.refresh();
        }
        function onHistoryReady(ok) {
            if (!ok) {
                root.notice = backend.historyError;
                return;
            }
            if (backend.historyEntries.length > 0)
                root.historyEpoch = backend.historyEntries[0].id;
            historyDialog.open();
        }
        function onPushPreviewReady(ok) {
            if (ok)
                previewDialog.open();
            else
                root.notice = backend.statusError;
        }
        function onVaultChanged() {
            autoPush.restart();
        }
        function onCloneFinished(ok) {
            root.notice = "";
            // A sign-in that no longer works, or was cancelled, asks again.
            if (!ok && !backend.cloned)
                onboardDialog.open();
        }
        function onSignInChanged() {
            root.start();
        }
    }

    // ---- Dialogs
    PromptDialog {
        id: newNoteDialog
        title: "New note"
        placeholder: "Note title"
        onAccepted: {
            if (value.length === 0 || !root.flushEdits())
                return;
            backend.newNote(value);
            editor.forceActiveFocus();
        }
    }

    PromptDialog {
        id: newFolderDialog
        title: "New folder"
        placeholder: "Folder name"
        onAccepted: { if (value.length > 0) backend.newFolder(value); }
    }

    PromptDialog {
        id: renameFolderDialog
        title: "Rename folder"
        placeholder: "Folder name"
        initial: root.folderLabel(backend.currentFolder)
        hint: "The next Push renames the folder in iCloud too."
        onAccepted: {
            if (value.length === 0 || !root.flushEdits())
                return;
            var err = backend.renameCurrentFolder(value);
            if (err.length > 0)
                root.notice = err;
        }
    }

    AppDialog {
        id: deleteFolderDialog
        title: "Delete folder?"
        standardButtons: Dialog.Yes | Dialog.No
        ColumnLayout {
            Label {
                textFormat: Text.PlainText
                Layout.preferredWidth: 340
                wrapMode: Text.WordWrap
                text: "Move \"" + root.folderLabel(backend.currentFolder) + "\" and its "
                      + (backend.folderNoteCounts[backend.currentFolder] || 0) + " note(s) to the trash? "
                      + "The next Push moves the notes to Recently Deleted in iCloud and deletes the folder there."
            }
        }
        onAccepted: {
            var err = backend.deleteCurrentFolder();
            if (err.length > 0)
                root.notice = err;
            else
                root.loadEditor();
        }
    }

    AppDialog {
        id: saveWarnDialog
        title: "Save anyway?"
        standardButtons: Dialog.Yes | Dialog.No
        ColumnLayout {
            Label {
                textFormat: Text.PlainText
                id: saveWarnText
                Layout.preferredWidth: 340
                wrapMode: Text.WordWrap
            }
        }
        onAccepted: root.doSave()
    }

    AppDialog {
        id: deleteDialog
        title: "Delete note?"
        standardButtons: Dialog.Yes | Dialog.No
        ColumnLayout {
            Label {
                textFormat: Text.PlainText
                Layout.preferredWidth: 300
                wrapMode: Text.WordWrap
                text: "Move \"" + root.noteLabel(backend.currentNote) + "\" to the trash? The next Push moves it to Recently Deleted in iCloud."
            }
        }
        onAccepted: {
            var err = backend.deleteCurrentNote();
            if (err.length > 0)
                root.notice = err;
            else
                root.loadEditor(); // unsaved edits go with the note
        }
    }

    AppDialog {
        id: moveDialog
        title: "Move note"
        standardButtons: Dialog.Ok | Dialog.Cancel
        onOpened: { moveTarget.model = root.moveTargets(); moveTarget.currentIndex = 0; }
        ColumnLayout {
            spacing: 10
            Label {
                textFormat: Text.PlainText
                Layout.preferredWidth: 300
                wrapMode: Text.WordWrap
                text: "Move \"" + root.displayTitle(backend.currentNote) + "\" to:"
            }
            ComboBox {
                id: moveTarget
                Layout.preferredWidth: 300
            }
        }
        onAccepted: { if (moveTarget.currentText.length > 0) root.moveOpenNote(moveTarget.currentText); }
    }

    // The one place a sync is told to delete most of the vault (sync --yes).
    // "Keep them" is the default: it has the focus, Enter and Escape both
    // leave everything as it is, and deleting takes a click on its button,
    // which the keyboard never reaches.
    AppDialog {
        id: deleteMissingDialog
        title: "Delete these notes from iCloud?"
        onOpened: keepMissingButton.forceActiveFocus()
        ColumnLayout {
            spacing: 14
            Keys.onReturnPressed: deleteMissingDialog.reject()
            Keys.onEnterPressed: deleteMissingDialog.reject()
            Label {
                textFormat: Text.PlainText
                Layout.preferredWidth: 400
                wrapMode: Text.WordWrap
                text: backend.deletionsRefused + "\n\nDeleting moves every note whose file is gone to Recently Deleted in iCloud, on all your devices (recoverable there for about 30 days). Sync status lists them."
            }
            RowLayout {
                Layout.fillWidth: true
                spacing: 8
                Button {
                    text: "Delete from iCloud"
                    focusPolicy: Qt.NoFocus
                    enabled: !backend.syncRunning
                    onClicked: deleteMissingDialog.accept()
                }
                Item { Layout.fillWidth: true }
                PrimaryButton {
                    id: keepMissingButton
                    text: "Keep them"
                    onClicked: deleteMissingDialog.reject()
                }
            }
        }
        onAccepted: {
            if (!root.flushEdits())
                return;
            root.lastFocusSync = Date.now();
            backend.runSync(true);
        }
    }

    AppDialog {
        id: previewDialog
        title: "Sync status"
        width: Math.min(root.width - 80, 620)
        height: Math.min(root.height - 80, 460)
        footer: DialogButtonBox {
            PrimaryButton {
                text: "Sync now"
                enabled: !backend.syncRunning
                onClicked: {
                    previewDialog.close();
                    root.syncNow();
                }
            }
            Button { text: "Close"; DialogButtonBox.buttonRole: DialogButtonBox.RejectRole }
        }
        ColumnLayout {
            anchors.fill: parent
            Label {
                textFormat: Text.PlainText
                Layout.fillWidth: true
                wrapMode: Text.WordWrap
                color: root.colTextMuted
                text: backend.statusEntries.length === 0
                      ? "Nothing to sync: everything matches iCloud."
                      : backend.statusUnchanged > 0
                        ? backend.statusUnchanged + " note(s) already match iCloud."
                        : "Every tracked note has a pending change."
            }
            Label {
                textFormat: Text.PlainText
                Layout.fillWidth: true
                wrapMode: Text.WordWrap
                visible: backend.statusNotices.length > 0
                color: root.colYellow
                text: backend.statusNotices.join("\n")
            }
            ScrollView {
                Layout.fillWidth: true
                Layout.fillHeight: true
                ListView {
                    model: backend.statusEntries
                    clip: true
                    spacing: 6
                    delegate: ColumnLayout {
                        width: ListView.view.width
                        spacing: 0
                        RowLayout {
                            spacing: 8
                            Pill {
                                label: ({ createFolder: "new folder", renameFolder: "rename folder", deleteFolder: "delete folder" })[modelData.kind]
                                       || modelData.kind
                                tint: root.colAccent
                            }
                            Pill {
                                label: modelData.resolution
                                tint: modelData.resolution === "ready" ? root.colGreen
                                    : modelData.resolution === "refused" ? root.colRed : root.colYellow
                            }
                            Label { textFormat: Text.PlainText; Layout.fillWidth: true; elide: Text.ElideRight; text: modelData.file; color: root.colText }
                        }
                        Label {
                            textFormat: Text.PlainText
                            Layout.fillWidth: true
                            Layout.leftMargin: 2
                            wrapMode: Text.WordWrap
                            visible: text.length > 0
                            color: modelData.reason ? root.colRed : root.colTextMuted
                            font.pixelSize: 11
                            text: modelData.reason || modelData.remark || ""
                        }
                    }
                    ScrollBar.vertical: ScrollBar {}
                }
            }
        }
    }

    AppDialog {
        id: historyDialog
        title: "Note history"
        width: Math.min(root.width - 80, 640)
        height: Math.min(root.height - 80, 480)
        standardButtons: Dialog.Close
        ColumnLayout {
            anchors.fill: parent
            Label {
                Layout.fillWidth: true
                wrapMode: Text.WordWrap
                color: root.colTextMuted
                text: "Snapshots from past pulls and pushes, newest first. Read-only here; to discard a note's local edits, run `icloud-notes restore <note> --yes` in a terminal."
            }
            SplitView {
                Layout.fillWidth: true
                Layout.fillHeight: true
                orientation: Qt.Horizontal
                ListView {
                    SplitView.preferredWidth: 220
                    model: backend.historyEntries
                    clip: true
                    delegate: ItemDelegate {
                        width: ListView.view.width
                        text: (modelData.timestamp || modelData.id) + "\n" + (modelData.changed || []).join(", ")
                        highlighted: modelData.id === root.historyEpoch
                        onClicked: {
                            root.historyEpoch = modelData.id;
                            backend.runDiff(modelData.id);
                        }
                    }
                    ScrollBar.vertical: ScrollBar {}
                }
                ScrollView {
                    SplitView.fillWidth: true
                    TextArea {
                        readOnly: true
                        selectByMouse: true
                        font.family: "monospace"
                        font.pointSize: 10
                        text: backend.diffText.length > 0 ? backend.diffText : "Pick a snapshot to diff it against the current iCloud copy."
                    }
                }
            }
        }
    }

    AppDialog {
        id: onboardDialog
        title: "Link your Apple Notes"
        footer: DialogButtonBox {
            PrimaryButton {
                text: "Clone my notes"
                enabled: !backend.syncRunning
                onClicked: {
                    onboardDialog.close();
                    backend.runClone();
                }
            }
            Button { text: "Cancel"; DialogButtonBox.buttonRole: DialogButtonBox.RejectRole }
        }
        ColumnLayout {
            Label {
                Layout.preferredWidth: 380
                wrapMode: Text.WordWrap
                textFormat: Text.PlainText
                text: "This downloads all your Apple Notes into ~/Documents/Notes as Markdown, one file per note with the title as its first line, like in Notes. "
                      + (backend.signedIn ? "It uses the iCloud account signed in on this computer, " + backend.appleId + ". "
                                          : "Apple's own sign-in window opens first (password and 2FA stay on Apple's pages); the sign-in is shared with the other iCloud apps. ")
                      + "Apple Notes must not use Advanced Data Protection, because icloud-notes-sync cannot decrypt it."
            }
        }
    }

    AppDialog {
        id: logDialog
        title: "Sync log"
        width: Math.min(root.width - 80, 640)
        height: Math.min(root.height - 80, 480)
        footer: DialogButtonBox {
            Button { text: "Clear"; onClicked: backend.clearLog() }
            Button { text: "Close"; DialogButtonBox.buttonRole: DialogButtonBox.RejectRole }
        }
        ScrollView {
            anchors.fill: parent
            TextArea {
                readOnly: true
                selectByMouse: true
                font.family: "monospace"
                font.pointSize: 10
                text: backend.syncLog
            }
        }
    }
}
