import QtQuick
import QtQuick.Controls
import QtQuick.Layouts
import QtCore

ApplicationWindow {
    id: root
    visible: true
    width: 1100
    height: 700
    title: backend.currentNote.length > 0 ? noteLabel(backend.currentNote) + " - Notes" : "Notes"

    // ---- Theme: the active Omarchy palette, or the system palette off Omarchy.
    SystemPalette { id: sys }
    readonly property var theme: backend.theme
    function tone(key, fallback) { return theme[key] ? theme[key] : fallback; }
    readonly property color colBg: tone("background", sys.window)
    readonly property color colPanel: tone("dark_background", Qt.darker(sys.window, 1.08))
    readonly property color colSidebar: tone("darker_background", Qt.darker(sys.window, 1.16))
    readonly property color colRaised: tone("lighter_background", sys.base)
    readonly property color colText: tone("foreground", sys.text)
    readonly property color colTextDim: tone("light_foreground", sys.text)
    readonly property color colTextMuted: tone("dark_foreground", sys.mid)
    readonly property color colLine: tone("muted", sys.mid)
    readonly property color colSelection: tone("selection", sys.highlight)
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
    color: colBg

    // ---- State
    // What the editor was last loaded from or saved as; edits diverge from it.
    property string savedText: ""
    property bool dirty: editor.text !== savedText
    property string notice: ""
    property bool searching: searchField.text.trim().length >= 2
    property var searchResults: []
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
        implicitHeight: 28
        hoverEnabled: true
        opacity: enabled ? 1 : 0.3
        background: Rectangle {
            radius: 6
            color: iconButton.down || iconButton.checked ? root.colSelection
                 : iconButton.hovered ? root.colRaised : "transparent"
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
            id: pillText
            anchors.centerIn: parent
            text: parent.label
            color: parent.tint
            font.pixelSize: 10
            font.bold: true
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
                       "tables": ["tables", root.colTextMuted] };
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
        editor.text = backend.noteBody;
        savedText = editor.text;
    }
    function doSave() {
        backend.saveCurrentNote(editor.text);
        savedText = editor.text;
        notice = "";
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
        doSave();
        return true;
    }
    function openNote(folder, name) {
        if (!flushEdits())
            return;
        notice = "";
        backend.currentFolder = folder;
        backend.openNote(name);
    }
    function openFolder(folder) {
        if (folder !== backend.currentFolder && flushEdits())
            backend.currentFolder = folder;
    }
    function commitTitle(title) {
        title = title.trim();
        if (title.length === 0 || title === titleField.current || !flushEdits()) {
            titleField.text = titleField.current;
            return;
        }
        var err = backend.renameCurrentNote(title);
        if (err.length > 0) {
            notice = err;
            titleField.text = titleField.current;
        }
    }

    function toggleTask() {
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
        if (backend.currentNote.length === 0 || sel.length === 0)
            return;
        editor.remove(s, editor.selectionEnd);
        editor.insert(s, before + sel + after);
        editor.select(s + before.length, s + before.length + sel.length);
        editor.forceActiveFocus();
    }
    function insertLink() {
        if (backend.currentNote.length === 0)
            return;
        var s = editor.selectionStart, sel = editor.selectedText || "text", proto = "https://";
        editor.remove(s, editor.selectionEnd);
        editor.insert(s, "[" + sel + "](" + proto + ")");
        editor.select(s + sel.length + 3, s + sel.length + 3 + proto.length);
        editor.forceActiveFocus();
    }

    // ---- Toolbar
    header: Rectangle {
        height: 44
        color: root.colPanel
        RowLayout {
            anchors.fill: parent
            anchors.leftMargin: 10
            anchors.rightMargin: 10
            spacing: 2

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
                enabled: backend.currentNote.length > 0 && !backend.syncRunning
                onClicked: { titleField.forceActiveFocus(); titleField.selectAll(); }
            }
            Item { Layout.fillWidth: true }
            IconButton { glyph: "\uf046"; tip: "Checklist (Ctrl+Enter)"; enabled: backend.currentNote.length > 0; onClicked: root.toggleTask() }
            IconButton { glyph: "\uf032"; tip: "Bold (Ctrl+B)"; enabled: backend.currentNote.length > 0; onClicked: root.wrapSelection("**", "**") }
            IconButton { glyph: "\uf033"; tip: "Italic (Ctrl+I)"; enabled: backend.currentNote.length > 0; onClicked: root.wrapSelection("*", "*") }
            IconButton { glyph: "\uf0c1"; tip: "Link (Ctrl+K)"; enabled: backend.currentNote.length > 0; onClicked: root.insertLink() }
            Separator { Layout.leftMargin: 6; Layout.rightMargin: 6 }
            IconButton {
                glyph: "\uf0ed"; tip: "Pull from iCloud"
                enabled: backend.cloned && !backend.syncRunning
                onClicked: backend.runPull()
            }
            IconButton {
                glyph: "\uf0ee"; tip: "Push to iCloud…"
                enabled: backend.cloned && !backend.syncRunning
                // Preview first: push only runs after explicit confirmation.
                onClicked: { if (root.flushEdits()) backend.refreshPushPreview(); }
            }
            IconButton {
                id: autoButton
                glyph: "\uf021"; tip: "Sync automatically: pull when you switch to Notes, push once edits settle"
                checkable: true
                checked: true // like Notes, sync on its own; the setting remembers a change
                enabled: backend.cloned
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
                    text: "Status preview"
                    enabled: backend.cloned && !backend.syncRunning
                    onTriggered: backend.refreshPushPreview()
                }
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
                text: backend.syncMessage
                color: root.colTextMuted
                font.pixelSize: 11
                elide: Text.ElideRight
                Layout.fillWidth: true
            }
            Label {
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
            visible: !backend.icloudMdAvailable || !backend.cloned
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
                Glyph { text: backend.icloudMdAvailable ? "\uf0c2" : "\uf071"; color: backend.icloudMdAvailable ? root.colAccent : root.colYellow }
                Label {
                    Layout.fillWidth: true
                    wrapMode: Text.WordWrap
                    color: root.colTextDim
                    text: backend.icloudMdAvailable
                          ? "This folder is not linked to iCloud yet. Clone to download your Apple Notes."
                          : "icloud-md was not found on PATH. Install it (npm install -g icloud-md, needs Node 20+) and restart to enable sync."
                }
                PrimaryButton {
                    visible: backend.icloudMdAvailable
                    text: "Clone…"
                    enabled: !backend.syncRunning
                    onClicked: onboardDialog.open()
                }
            }
        }

        Rectangle {
            visible: backend.authExpired
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
                    Layout.fillWidth: true
                    wrapMode: Text.WordWrap
                    color: root.colTextDim
                    text: "Your iCloud sign-in expired, so syncing is paused. Your edits are safe on this computer and go up once you sign in again. Apple's window opens once, and a returning browser usually skips 2FA."
                }
                PrimaryButton {
                    text: "Sign in"
                    enabled: !backend.syncRunning
                    onClicked: backend.runReauthenticate()
                }
            }
        }

        SplitView {
            id: panes
            Layout.fillWidth: true
            Layout.fillHeight: true
            orientation: Qt.Horizontal
            handle: Rectangle { implicitWidth: 1; color: root.colLine; opacity: 0.6 }
            Component.onCompleted: { if (settings.panes) restoreState(settings.panes); }

            // Folders
            Rectangle {
                SplitView.preferredWidth: 210
                SplitView.minimumWidth: 150
                color: root.colSidebar
                ColumnLayout {
                    anchors.fill: parent
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
                        delegate: Item {
                            id: folderRow
                            width: folderView.width
                            height: 30
                            property bool selected: modelData === backend.currentFolder
                            property int depth: modelData.length === 0 ? 0 : modelData.split("/").length - 1
                            HoverHandler { id: folderHover }
                            Rectangle {
                                anchors.fill: parent
                                anchors.leftMargin: 8
                                anchors.rightMargin: 8
                                radius: 6
                                color: folderRow.selected ? root.colSelection
                                     : folderHover.hovered ? Qt.rgba(root.colSelection.r, root.colSelection.g, root.colSelection.b, 0.5)
                                     : "transparent"
                            }
                            RowLayout {
                                anchors.fill: parent
                                anchors.leftMargin: 18 + folderRow.depth * 14
                                anchors.rightMargin: 18
                                spacing: 8
                                Glyph { text: modelData.length === 0 ? "\uf07c" : "\uf07b"; font.pixelSize: 13; color: root.colAccent }
                                Label {
                                    Layout.fillWidth: true
                                    elide: Text.ElideRight
                                    text: root.folderLabel(modelData)
                                    color: root.colText
                                    font.pixelSize: root.pt(13)
                                }
                                Label {
                                    text: backend.folderNoteCounts[modelData] || ""
                                    color: root.colTextMuted
                                    font.pixelSize: 11
                                }
                            }
                            TapHandler { acceptedButtons: Qt.LeftButton; onTapped: root.openFolder(modelData) }
                            TapHandler {
                                acceptedButtons: Qt.RightButton
                                enabled: modelData.length > 0
                                onTapped: { root.openFolder(modelData); folderMenu.popup(); }
                            }
                        }
                        Menu {
                            id: folderMenu
                            MenuItem { text: "Rename folder…"; onTriggered: renameFolderDialog.open() }
                            MenuItem { text: "Delete folder…"; onTriggered: deleteFolderDialog.open() }
                        }
                    }
                }
            }

            // Notes
            Rectangle {
                SplitView.preferredWidth: 290
                SplitView.minimumWidth: 200
                color: root.colPanel
                ColumnLayout {
                    anchors.fill: parent
                    spacing: 0
                    Label {
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
                    Rectangle {
                        Layout.fillWidth: true
                        Layout.margins: 12
                        Layout.topMargin: 8
                        implicitHeight: 30
                        radius: 7
                        color: root.colRaised
                        RowLayout {
                            anchors.fill: parent
                            anchors.leftMargin: 10
                            anchors.rightMargin: 6
                            spacing: 6
                            Glyph { text: "\uf002"; font.pixelSize: 12; color: root.colTextMuted }
                            TextField {
                                id: searchField
                                Layout.fillWidth: true
                                placeholderText: "Search"
                                background: null
                                padding: 0
                                color: root.colText
                                font.pixelSize: root.pt(12)
                                onTextChanged: {
                                    if (root.searching)
                                        root.searchResults = backend.searchVault(text);
                                }
                                Keys.onEscapePressed: { text = ""; editor.forceActiveFocus(); }
                            }
                        }
                    }
                    Label {
                        Layout.leftMargin: 20
                        Layout.bottomMargin: 4
                        text: {
                            if (root.searching)
                                return root.searchResults.length + " result" + (root.searchResults.length === 1 ? "" : "s");
                            var n = backend.notes.length;
                            return n + " note" + (n === 1 ? "" : "s");
                        }
                        color: root.colTextMuted
                        font.pixelSize: 11
                    }
                    ListView {
                        id: noteView
                        Layout.fillWidth: true
                        Layout.fillHeight: true
                        model: root.searching ? root.searchResults : backend.notes
                        clip: true
                        delegate: Item {
                            id: noteRow
                            width: noteView.width
                            height: noteColumn.implicitHeight + 18
                            property bool isResult: typeof modelData !== "string"
                            property string folder: isResult ? modelData.folder : backend.currentFolder
                            property string fileName: isResult ? modelData.file : modelData
                            property bool selected: fileName === backend.currentNote && folder === backend.currentFolder
                            property var pills: isResult ? [] : root.badges(fileName)
                            HoverHandler { id: noteHover }
                            Rectangle {
                                anchors.fill: parent
                                anchors.leftMargin: 8
                                anchors.rightMargin: 8
                                radius: 8
                                color: noteRow.selected ? root.colSelection
                                     : noteHover.hovered ? Qt.rgba(root.colSelection.r, root.colSelection.g, root.colSelection.b, 0.5)
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
                                        visible: text.length > 0
                                        text: noteRow.isResult ? (modelData.folder || "All Notes")
                                                               : ((root.detail(noteRow.fileName).modifiedMs || 0) > 0
                                                                  ? root.shortDate(root.detail(noteRow.fileName).modifiedMs) : "")
                                        color: root.colTextDim
                                        font.pixelSize: root.pt(11)
                                    }
                                    Label {
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
                            TapHandler { onTapped: { if (!noteRow.selected) root.openNote(noteRow.folder, noteRow.fileName); } }
                        }
                    }
                }
            }

            // Editor
            Rectangle {
                SplitView.fillWidth: true
                SplitView.minimumWidth: 320
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

                    // The title is edited in place, like the first line in Notes;
                    // committing it retitles the note (heading line or file name).
                    TextInput {
                        id: titleField
                        Layout.fillWidth: true
                        Layout.leftMargin: 32
                        Layout.rightMargin: 32
                        Layout.topMargin: 24
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
                        onEditingFinished: root.commitTitle(text)
                        Keys.onReturnPressed: editor.forceActiveFocus()
                        Keys.onEnterPressed: editor.forceActiveFocus()
                        Keys.onEscapePressed: { text = current; editor.forceActiveFocus(); }
                    }
                    Label {
                        Layout.fillWidth: true
                        Layout.topMargin: 4
                        horizontalAlignment: Text.AlignHCenter
                        color: root.colTextMuted
                        font.pixelSize: root.pt(11)
                        text: {
                            var ms = root.detail(backend.currentNote).modifiedMs || 0;
                            if (ms <= 0)
                                return "";
                            var d = new Date(ms);
                            return d.toLocaleDateString(Qt.locale(), Locale.LongFormat)
                                + " at " + d.toLocaleTimeString(Qt.locale(), Locale.ShortFormat);
                        }
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
                                        Layout.maximumWidth: 90
                                        elide: Text.ElideMiddle
                                        text: modelData.name
                                        color: root.colTextMuted
                                        font.pixelSize: 10
                                    }
                                }
                            }
                            Label { text: "read-only in iCloud"; color: root.colTextMuted; font.pixelSize: 10 }
                        }
                    }

                    ScrollView {
                        Layout.fillWidth: true
                        Layout.fillHeight: true
                        Layout.topMargin: 8
                        TextArea {
                            id: editor
                            wrapMode: TextArea.Wrap
                            selectByMouse: true
                            background: null
                            color: root.colText
                            selectionColor: root.colAccent
                            selectedTextColor: root.colBg
                            font.pixelSize: root.pt(14)
                            leftPadding: 32
                            rightPadding: 32
                            topPadding: 4
                            bottomPadding: 32
                            placeholderText: "Start writing…"
                            onTextChanged: autosave.restart()
                            Component.onCompleted: backend.attachEditor(editor.textDocument)
                        }
                    }
                }
            }
        }
    }

    // Notes has no Save button: edits land on disk once typing pauses. Edits
    // a guardrail refuses stay in the editor and are flagged in the footer.
    Timer {
        id: autosave
        interval: 1500
        onTriggered: {
            if (backend.syncRunning) { // never write under a running pull; try again after
                restart();
                return;
            }
            if (root.dirty && backend.currentNote.length > 0 && backend.saveWarning(editor.text).length === 0)
                root.doSave();
        }
    }

    function dialogOpen() {
        return [previewDialog, logDialog, historyDialog, saveWarnDialog, newNoteDialog,
                newFolderDialog, renameFolderDialog, deleteFolderDialog, deleteDialog, onboardDialog]
            .some(function (d) { return d.visible; });
    }

    // Like Notes, changes reach iCloud on their own. icloud-md keeps this
    // safe: it merges, refuses a note it cannot push safely (the badges say
    // which; Push… shows why), and deletions only move notes to Recently
    // Deleted. Pushes wait until edits settle so a burst of typing is one push.
    Timer {
        id: autoPush
        interval: 20 * 1000
        onTriggered: {
            if (!autoButton.checked || !backend.cloned || backend.authExpired)
                return;
            if (backend.syncRunning || root.dirty || root.dialogOpen()) {
                restart();
                return;
            }
            backend.runPush();
        }
    }
    // Changes from other devices come in when the window is looked at, not
    // on a timer: nothing runs while Notes sits unused, and a phone edit is
    // there the moment you switch back. At most once a minute, so flipping
    // between windows does not sync on every flip.
    property double lastFocusSync: 0
    onActiveChanged: {
        if (!active || !autoButton.checked || !backend.cloned || backend.authExpired)
            return;
        if (backend.syncRunning || root.dialogOpen() || root.dirty || autoPush.running
                || Date.now() - lastFocusSync < 60 * 1000)
            return;
        lastFocusSync = Date.now();
        backend.runSync(); // both directions: a change from any program in any folder
    }

    Component.onCompleted: {
        // Sync on startup, like Notes does on launch. Without
        // a vault, an account already signed in on this machine is cloned
        // quietly; only a device that has never signed in sees the dialog.
        if (backend.cloned) {
            if (backend.icloudMdAvailable && !backend.authExpired) {
                root.lastFocusSync = Date.now(); // the window's first activation is this sync
                backend.runSync(); // edits made while the app was closed go up first
            }
        } else if (backend.icloudMdAvailable && backend.savedAccount.length > 0) {
            root.notice = "Downloading your notes as " + backend.savedAccount + "…";
            backend.runClone(backend.savedAccount);
        } else {
            onboardDialog.open();
        }
    }

    Shortcut { sequence: StandardKey.Save; onActivated: root.save() }
    Shortcut { sequence: "Ctrl+N"; onActivated: newNoteDialog.open() }
    Shortcut { sequence: "Ctrl+B"; onActivated: root.wrapSelection("**", "**") }
    Shortcut { sequence: "Ctrl+I"; onActivated: root.wrapSelection("*", "*") }
    Shortcut { sequence: "Ctrl+K"; onActivated: root.insertLink() }
    Shortcut { sequence: "Ctrl+Return"; enabled: editor.activeFocus; onActivated: root.toggleTask() }

    Settings {
        id: settings
        property alias autoPull: autoButton.checked
        property alias windowWidth: root.width
        property alias windowHeight: root.height
        property var panes
    }
    Component.onDestruction: settings.panes = panes.saveState()
    Shortcut { sequence: StandardKey.Find; onActivated: searchField.forceActiveFocus() }

    Connections {
        target: backend
        function onNoteContentChanged() {
            if (!root.dirty)
                root.loadEditor();
        }
        function onCurrentNoteChangedOnDisk() {
            // If dirty, the editor keeps the user's text; Ctrl+S/Refresh reconciles.
            if (!root.dirty)
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
            if (autoButton.checked)
                autoPush.restart();
        }
        function onCloneFinished(ok) {
            root.notice = "";
            // A saved sign-in that no longer works falls back to signing in.
            if (!ok && !backend.cloned)
                onboardDialog.open();
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
        hint: "Notes has no folder renames: the next Push creates the new folder and moves these notes into it. The old folder stays in Notes, empty, until you delete it there."
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
                Layout.preferredWidth: 340
                wrapMode: Text.WordWrap
                text: "Move \"" + root.folderLabel(backend.currentFolder) + "\" and its "
                      + (backend.folderNoteCounts[backend.currentFolder] || 0) + " note(s) to the trash? "
                      + "The next Push moves the notes to Recently Deleted in iCloud; the empty folder stays in Notes until you delete it there."
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
        id: previewDialog
        title: "Push preview"
        width: Math.min(root.width - 80, 620)
        height: Math.min(root.height - 80, 460)
        footer: DialogButtonBox {
            PrimaryButton {
                text: "Push now"
                enabled: !backend.syncRunning
                onClicked: {
                    previewDialog.close();
                    backend.runPush();
                }
            }
            Button { text: "Cancel"; DialogButtonBox.buttonRole: DialogButtonBox.RejectRole }
        }
        ColumnLayout {
            anchors.fill: parent
            Label {
                Layout.fillWidth: true
                wrapMode: Text.WordWrap
                color: root.colTextMuted
                text: backend.statusUnchanged > 0
                      ? backend.statusUnchanged + " note(s) already match iCloud."
                      : "Every tracked note has a pending change."
            }
            Label {
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
                                label: modelData.kind === "createFolder" ? "new folder" : modelData.kind
                                tint: root.colAccent
                            }
                            Pill {
                                label: modelData.resolution
                                tint: modelData.resolution === "ready" ? root.colGreen
                                    : modelData.resolution === "refused" ? root.colRed : root.colYellow
                            }
                            Label { Layout.fillWidth: true; elide: Text.ElideRight; text: modelData.file; color: root.colText }
                        }
                        Label {
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
                text: "Snapshots from past pulls and pushes, newest first. Restoring is read-only here; use `icloud-md revert` deliberately."
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
                text: "This downloads all your Apple Notes into ~/Documents/icloud-notes as Markdown, one file per note with the title as its first line, like in Notes. "
                      + "A real Apple sign-in window opens once (password and 2FA are handled by Apple's own pages); after that this device stays signed in. "
                      + "Apple Notes must not use Advanced Data Protection, because icloud-md cannot decrypt it."
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
