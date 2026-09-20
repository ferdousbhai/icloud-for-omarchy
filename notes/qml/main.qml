import QtQuick
import QtQuick.Controls
import QtQuick.Layouts

ApplicationWindow {
    id: root
    visible: true
    width: 1024
    height: 680
    title: backend.currentNote.length > 0 ? noteLabel(backend.currentNote) + " — Notes" : "Notes"

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

    component AppDialog: Dialog {
        anchors.centerIn: parent
        modal: true
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
                opacity: 0.7
                visible: text.length > 0
            }
        }
        onOpened: { field.text = initial; field.forceActiveFocus(); }
    }
    component Divider: Rectangle {
        anchors.left: parent.left
        anchors.right: parent.right
        anchors.bottom: parent.bottom
        height: 1
        color: "gray"
        opacity: 0.25
    }

    function noteLabel(n) { return n.replace(/\.md$/i, ""); }
    function folderLabel(f) { return f.length === 0 ? "All Notes" : f; }
    function vaultRel(name) {
        return backend.currentFolder.length === 0 ? name : backend.currentFolder + "/" + name;
    }
    function detail(name) { return backend.noteDetails[name] || {}; }
    function displayTitle(name) { return detail(name).title || noteLabel(name); }
    function badges(name) {
        var labels = { "conflict": "conflict", "new": "new", "missing-id": "no id",
                       "foreign-id": "foreign id", "tables": "tables" };
        var out = (backend.noteStates[name] || []).map(function (flag) { return labels[flag] || flag; });
        var st = statusByFile[vaultRel(name)];
        if (st === "refused")
            out.push("push refused");
        else if (st === "conflict")
            out.push("remote changed");
        return out;
    }
    function displaySub(name) {
        var d = detail(name), parts = [], b = badges(name);
        if ((d.snippet || "").length > 0)
            parts.push(d.snippet);
        if ((d.modifiedMs || 0) > 0)
            parts.push(new Date(d.modifiedMs).toLocaleDateString(Qt.locale(), Locale.ShortFormat));
        if (b.length > 0)
            parts.push("(" + b.join(", ") + ")");
        return parts.join(" · ");
    }

    function loadEditor() {
        savedText = backend.noteBody;
        editor.text = backend.noteBody;
    }
    function doSave() {
        backend.saveCurrentNote(editor.text);
        savedText = editor.text;
        notice = "";
    }
    // Explicit save: risky edits go through the "Save anyway?" dialog.
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
            notice = "Kept your edits in the editor — resolve the warning (press Save) before moving on.";
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

    function toggleTask() {
        var line = editor.text.slice(0, editor.cursorPosition).split("\n").length - 1;
        var updated = backend.toggleCheckbox(editor.text, line);
        if (updated !== editor.text)
            editor.text = updated;
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

    header: ToolBar {
        RowLayout {
            anchors.fill: parent
            anchors.leftMargin: 8
            anchors.rightMargin: 8
            spacing: 6

            Button { text: "New note"; enabled: !backend.syncRunning; onClicked: newNoteDialog.open() }
            Button {
                text: "Save"
                enabled: root.dirty && backend.currentNote.length > 0 && !backend.syncRunning
                highlighted: root.dirty
                onClicked: root.save()
            }
            Button {
                text: "Delete"
                enabled: backend.currentNote.length > 0 && !backend.syncRunning
                onClicked: deleteDialog.open()
            }
            Button {
                text: "Rename"
                enabled: backend.currentNote.length > 0 && !backend.syncRunning
                onClicked: renameDialog.open()
            }
            Button { text: "New folder"; enabled: !backend.syncRunning; onClicked: newFolderDialog.open() }
            ToolSeparator {}
            Button {
                text: "Pull"
                enabled: backend.cloned && !backend.syncRunning
                onClicked: backend.runPull()
            }
            Button {
                text: "Push…"
                enabled: backend.cloned && !backend.syncRunning
                // Preview first: push only runs after explicit confirmation.
                onClicked: { if (root.flushEdits()) backend.refreshPushPreview(); }
            }
            Button {
                id: autoButton
                text: "Auto"
                checkable: true
                enabled: backend.cloned
            }
            Item { Layout.fillWidth: true }
            Button { text: "•••"; onClicked: moreMenu.open() }
            Menu {
                id: moreMenu
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

    footer: ToolBar {
        RowLayout {
            anchors.fill: parent
            anchors.leftMargin: 8
            anchors.rightMargin: 8
            BusyIndicator { running: backend.syncRunning; implicitWidth: 20; implicitHeight: 20 }
            Label {
                text: backend.syncMessage
                elide: Text.ElideRight
                Layout.fillWidth: true
            }
            Label {
                text: root.dirty ? "● unsaved" : "saved"
                opacity: 0.7
            }
            Label {
                visible: root.notice.length > 0
                text: root.notice
                elide: Text.ElideRight
                color: palette.highlight
                Layout.maximumWidth: 420
            }
        }
    }

    ColumnLayout {
        anchors.fill: parent
        spacing: 0

        Label {
            visible: !backend.icloudMdAvailable
            text: "icloud-md was not found on PATH. Install it (npm install -g icloud-md, needs Node 20+) and restart to enable sync."
            wrapMode: Text.WordWrap
            color: palette.highlight
            Layout.fillWidth: true
            Layout.margins: 8
        }
        RowLayout {
            visible: backend.icloudMdAvailable && !backend.cloned
            Layout.fillWidth: true
            Layout.margins: 8
            spacing: 10
            Label {
                Layout.fillWidth: true
                text: "This folder is not linked to iCloud yet. Clone to download your Apple Notes (Advanced Data Protection must be off)."
                wrapMode: Text.WordWrap
                color: palette.highlight
            }
            Button {
                text: "Clone…"
                highlighted: true
                enabled: !backend.syncRunning
                onClicked: onboardDialog.open()
            }
        }

        SplitView {
            Layout.fillWidth: true
            Layout.fillHeight: true
            orientation: Qt.Horizontal

            ColumnLayout {
                SplitView.preferredWidth: 200
                SplitView.minimumWidth: 140
                spacing: 0
                Label {
                    Layout.fillWidth: true
                    Layout.leftMargin: 10
                    Layout.topMargin: 8
                    Layout.bottomMargin: 4
                    text: "FOLDERS"
                    font.pointSize: 9
                    font.bold: true
                    opacity: 0.55
                }
                ListView {
                    id: folderView
                    Layout.fillWidth: true
                    Layout.fillHeight: true
                    model: backend.folders
                    clip: true
                    delegate: ItemDelegate {
                        width: folderView.width
                        highlighted: modelData === backend.currentFolder
                        onClicked: root.openFolder(modelData)
                        Divider {}
                        contentItem: RowLayout {
                            spacing: 8
                            Label { text: "📁" }
                            Label {
                                Layout.fillWidth: true
                                elide: Text.ElideRight
                                text: root.folderLabel(modelData)
                            }
                            Label {
                                opacity: 0.6
                                font.pointSize: 9
                                text: backend.folderNoteCounts[modelData] || ""
                            }
                        }
                    }
                    ScrollBar.vertical: ScrollBar {}
                }
            }

            ColumnLayout {
                SplitView.preferredWidth: 240
                SplitView.minimumWidth: 160
                spacing: 4

                Label {
                    Layout.fillWidth: true
                    Layout.leftMargin: 10
                    elide: Text.ElideRight
                    font.bold: true
                    font.pointSize: Math.round(13 * backend.uiScale)
                    text: {
                        var n = backend.notes.length;
                        return root.folderLabel(backend.currentFolder) + " — " + n + (n === 1 ? " note" : " notes");
                    }
                }
                TextField {
                    id: searchField
                    Layout.fillWidth: true
                    placeholderText: "Search all notes"
                    onTextChanged: {
                        if (root.searching)
                            root.searchResults = backend.searchVault(text);
                    }
                }

                ListView {
                    id: noteView
                    Layout.fillWidth: true
                    Layout.fillHeight: true
                    model: root.searching ? root.searchResults : backend.notes
                    clip: true
                    delegate: ItemDelegate {
                        width: noteView.width
                        property bool isResult: typeof modelData !== "string"
                        property string folder: isResult ? modelData.folder : backend.currentFolder
                        property string fileName: isResult ? modelData.file : modelData
                        highlighted: fileName === backend.currentNote && folder === backend.currentFolder
                        onClicked: { if (!highlighted) root.openNote(folder, fileName); }
                        Divider {}
                        contentItem: ColumnLayout {
                            spacing: 0
                            Label {
                                Layout.fillWidth: true
                                elide: Text.ElideRight
                                font.bold: true
                                text: isResult ? modelData.title : root.displayTitle(fileName)
                            }
                            Label {
                                Layout.fillWidth: true
                                elide: Text.ElideRight
                                opacity: 0.6
                                font.pointSize: 9
                                visible: text.length > 0
                                text: isResult
                                      ? [modelData.snippet, modelData.folder].filter(Boolean).join(" · ")
                                      : root.displaySub(fileName)
                            }
                        }
                    }
                    ScrollBar.vertical: ScrollBar {}
                }
            }

            ColumnLayout {
                SplitView.fillWidth: true
                SplitView.minimumWidth: 300
                spacing: 0

                RowLayout {
                    Layout.fillWidth: true
                    Layout.leftMargin: 8
                    Layout.rightMargin: 8
                    Layout.topMargin: 4
                    Layout.bottomMargin: 2
                    spacing: 4
                    visible: backend.currentNote.length > 0
                    Button { Layout.preferredWidth: 56; text: "B"; font.bold: true; onClicked: root.wrapSelection("**", "**") }
                    Button { Layout.preferredWidth: 56; text: "I"; font.italic: true; onClicked: root.wrapSelection("*", "*") }
                    Button { Layout.preferredWidth: 72; text: "Link"; onClicked: root.insertLink() }
                    Button { Layout.preferredWidth: 56; text: "☑"; onClicked: root.toggleTask() }
                    Item { Layout.fillWidth: true }
                }

                Label {
                    Layout.fillWidth: true
                    Layout.leftMargin: 20
                    Layout.rightMargin: 20
                    Layout.topMargin: 6
                    visible: backend.currentNote.length > 0
                    elide: Text.ElideRight
                    font.bold: true
                    font.pointSize: Math.round(20 * backend.uiScale)
                    text: root.displayTitle(backend.currentNote)
                }

                Label {
                    Layout.fillWidth: true
                    Layout.leftMargin: 20
                    Layout.rightMargin: 20
                    visible: backend.currentNote.length > 0
                    horizontalAlignment: Text.AlignHCenter
                    opacity: 0.6
                    font.pointSize: Math.round(11 * backend.uiScale)
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
                    implicitHeight: 64
                    visible: backend.noteAttachments.length > 0
                    color: "transparent"
                    border.color: palette.mid
                    RowLayout {
                        anchors.fill: parent
                        anchors.margins: 4
                        spacing: 8
                        Label {
                            text: "Attachments (preview only — read-only in iCloud):"
                            opacity: 0.7
                            font.pointSize: 9
                        }
                        ListView {
                            Layout.fillWidth: true
                            Layout.fillHeight: true
                            orientation: ListView.Horizontal
                            model: backend.noteAttachments
                            clip: true
                            delegate: ColumnLayout {
                                spacing: 0
                                Image {
                                    Layout.preferredWidth: 44
                                    Layout.preferredHeight: 36
                                    fillMode: Image.PreserveAspectFit
                                    source: modelData.url
                                    visible: modelData.image
                                }
                                Label {
                                    Layout.maximumWidth: 100
                                    elide: Text.ElideMiddle
                                    font.pointSize: 8
                                    opacity: modelData.image ? 0.7 : 1
                                    text: modelData.name
                                }
                            }
                        }
                    }
                }

                ScrollView {
                    Layout.fillWidth: true
                    Layout.fillHeight: true
                    TextArea {
                        id: editor
                        wrapMode: TextArea.Wrap
                        selectByMouse: true
                        font.pointSize: Math.round(13 * backend.uiScale)
                        leftPadding: 20
                        rightPadding: 20
                        topPadding: 8
                        placeholderText: "Select a note, or create a new one."
                        // Keep the caret visible while typing long notes.
                        onCursorRectangleChanged: editor.ensureVisible(cursorRectangle)
                    }
                }
            }
        }
    }

    Timer {
        interval: 5 * 60 * 1000
        running: autoButton.checked && backend.cloned
        repeat: true
        onTriggered: {
            // Auto-fetch only: publishing stays an explicit, previewed act.
            var open = [previewDialog, logDialog, historyDialog, saveWarnDialog, renameDialog,
                        newNoteDialog, newFolderDialog, deleteDialog, onboardDialog];
            if (backend.syncRunning || open.some(function (d) { return d.visible; }))
                return;
            if (root.dirty) {
                root.notice = "Auto-pull skipped: unsaved changes.";
                return;
            }
            backend.runPull();
        }
    }

    Component.onCompleted: {
        // Fetch remote changes on startup, like Notes does on launch.
        // First run shows the Clone dialog up front so it cannot be missed.
        if (!backend.cloned)
            onboardDialog.open();
        else if (backend.icloudMdAvailable)
            backend.runPull();
    }

    Shortcut { sequence: StandardKey.Save; onActivated: root.save() }
    Shortcut { sequence: "Ctrl+N"; onActivated: newNoteDialog.open() }
    Shortcut { sequence: "Ctrl+B"; onActivated: root.wrapSelection("**", "**") }
    Shortcut { sequence: "Ctrl+I"; onActivated: root.wrapSelection("*", "*") }
    Shortcut { sequence: "Ctrl+K"; onActivated: root.insertLink() }
    Shortcut { sequence: "Ctrl+Return"; onActivated: root.toggleTask() }

    Connections {
        target: backend
        function onNoteContentChanged() {
            if (!root.dirty)
                root.loadEditor();
        }
        function onCurrentNoteChangedOnDisk() {
            // If dirty, the editor keeps the user's text; Save/Refresh reconciles.
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
    }

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
        id: renameDialog
        title: "Rename note"
        placeholder: "New title"
        initial: root.noteLabel(backend.currentNote)
        hint: backend.vaultTitleMode === "filename"
              ? "This vault titles notes by file name: renaming the file retitles the note in iCloud."
              : "This vault keeps the title in the note's first line: only that line changes."
        onAccepted: {
            if (!root.flushEdits())
                return;
            var err = backend.renameCurrentNote(value);
            if (err.length > 0)
                root.notice = err;
        }
    }

    AppDialog {
        id: saveWarnDialog
        title: "Save anyway?"
        standardButtons: Dialog.Yes | Dialog.No
        Label {
            id: saveWarnText
            width: 340
            wrapMode: Text.WordWrap
        }
        onAccepted: root.doSave()
    }

    AppDialog {
        id: deleteDialog
        title: "Delete note?"
        standardButtons: Dialog.Yes | Dialog.No
        Label {
            width: 280
            wrapMode: Text.WordWrap
            text: "Move \"" + root.noteLabel(backend.currentNote) + "\" to the trash? The next Push moves it to Recently Deleted in iCloud."
        }
        onAccepted: {
            backend.deleteCurrentNote();
            root.loadEditor(); // unsaved edits go with the note
        }
    }

    AppDialog {
        id: previewDialog
        title: "Push preview"
        width: Math.min(root.width - 80, 620)
        height: Math.min(root.height - 80, 460)
        footer: DialogButtonBox {
            Button {
                text: "Push now"
                highlighted: true
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
                opacity: 0.7
                text: backend.statusUnchanged > 0
                      ? backend.statusUnchanged + " note(s) already match iCloud."
                      : "Every tracked note has a pending change."
            }
            Label {
                Layout.fillWidth: true
                wrapMode: Text.WordWrap
                visible: backend.statusNotices.length > 0
                color: palette.highlight
                text: backend.statusNotices.join("\n")
            }
            ScrollView {
                Layout.fillWidth: true
                Layout.fillHeight: true
                ListView {
                    model: backend.statusEntries
                    clip: true
                    delegate: ColumnLayout {
                        width: ListView.view.width
                        spacing: 0
                        Label {
                            Layout.fillWidth: true
                            elide: Text.ElideRight
                            font.bold: modelData.resolution !== "ready"
                            text: (modelData.kind === "createFolder" ? "new folder" : modelData.kind)
                                  + " · " + modelData.resolution + " · " + modelData.file
                        }
                        Label {
                            Layout.fillWidth: true
                            wrapMode: Text.WordWrap
                            visible: text.length > 0
                            color: modelData.reason ? "red" : palette.text
                            opacity: modelData.reason ? 1 : 0.7
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
                opacity: 0.7
                text: "Snapshots from past pulls and pushes, newest first. Restoring is read-only here — use `icloud-md revert` deliberately."
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
            Button {
                text: "Clone my notes"
                highlighted: true
                enabled: !backend.syncRunning
                onClicked: {
                    onboardDialog.close();
                    backend.runClone(filenameShape.checked ? "filename" : "in-body");
                }
            }
            Button { text: "Cancel"; DialogButtonBox.buttonRole: DialogButtonBox.RejectRole }
        }
        ColumnLayout {
            Label {
                Layout.preferredWidth: 380
                wrapMode: Text.WordWrap
                text: "This downloads all your Apple Notes into ~/Documents/icloud-notes as Markdown. "
                      + "A real Apple sign-in window opens (password and 2FA are handled by Apple's own pages). "
                      + "Apple Notes must not use Advanced Data Protection — icloud-md cannot decrypt it."
            }
            Label {
                Layout.preferredWidth: 380
                wrapMode: Text.WordWrap
                font.bold: true
                text: "How should note titles be stored? (chosen once, cannot change later)"
            }
            RadioButton { checked: true; text: "Title as first line (plain Markdown shape)" }
            RadioButton { id: filenameShape; text: "Title as file name (Obsidian shape, body only)" }
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
