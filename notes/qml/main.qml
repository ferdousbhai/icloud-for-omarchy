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
    property bool filenameTitles: backend.vaultTitleMode === "filename"
    property string notice: ""
    // The sync tool reads this note but will never push it, so it opens locked.
    readonly property bool noteLocked: backend.readOnlyReason.length > 0
    // A note both sides edited opens on its conflict blocks, one version
    // picked per block, instead of on the raw markers ("Edit as text").
    readonly property var conflicts: backend.noteConflicts
    property var conflictChoices: []
    property bool conflictAsText: false
    readonly property bool resolving: conflicts.length > 0 && !conflictAsText && !noteLocked
    // Markers with no versions to pick from (nested or out of order): the
    // note opens on a way out instead, read-only until "Edit as text".
    readonly property bool unreadableConflict: backend.noteConflictsUnreadable && !conflictAsText && !noteLocked
    onConflictsChanged: conflictChoices = conflicts.map(function () { return ""; })
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
                Label { text: card.label; font.bold: true; color: root.colText; font.pixelSize: root.pt(13) }
                Item { Layout.fillWidth: true }
                Label {
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
                        // Rendered as Markdown so it reads like the note; blank
                        // lines keep their height.
                        text: parent.modelData.text.trim().length > 0 ? parent.modelData.text : " "
                        textFormat: parent.modelData.text.trim().length > 0 ? Text.MarkdownText : Text.PlainText
                        wrapMode: Text.WrapAtWordBoundaryOrAnywhere
                        color: parent.modelData.changed ? root.colText : root.colTextDim
                        linkColor: root.colAccent
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
        if (!backend.saveCurrentNote(text)) {
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
    function openNote(folder, name) {
        if (!flushEdits())
            return;
        notice = "";
        backend.currentFolder = folder;
        backend.openNote(name);
        pullIfStale();
    }
    // Edit the latest copy: opening a note, focusing the editor or typing
    // into an untouched note pulls first when the last pull is over a
    // minute old. The editor waits (read-only) until the pull is in, so an
    // edit never starts on a copy iCloud has already moved past.
    property bool freshening: false
    property bool keepingEdits: false
    property string keptNotice: ""
    function pullIfStale() {
        if (!backend.cloned || backend.authExpired || backend.syncRunning
                || dirty || dialogOpen() || Date.now() - lastFocusSync < 60 * 1000)
            return false;
        lastFocusSync = Date.now();
        freshening = true;
        backend.runSync(); // pending edits go up first, then the pull
        return true;
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
                enabled: backend.currentNote.length > 0 && !root.noteLocked && !backend.syncRunning
                onClicked: root.selectTitle()
            }
            Item { Layout.fillWidth: true }
            IconButton { glyph: "\uf046"; tip: "Checklist (Ctrl+Enter)"; enabled: backend.currentNote.length > 0 && !root.noteLocked && !root.resolving; onClicked: root.toggleTask() }
            IconButton { glyph: "\uf032"; tip: "Bold (Ctrl+B)"; enabled: backend.currentNote.length > 0 && !root.noteLocked && !root.resolving; onClicked: root.wrapSelection("**", "**") }
            IconButton { glyph: "\uf033"; tip: "Italic (Ctrl+I)"; enabled: backend.currentNote.length > 0 && !root.noteLocked && !root.resolving; onClicked: root.wrapSelection("*", "*") }
            IconButton { glyph: "\uf0c1"; tip: "Link (Ctrl+K)"; enabled: backend.currentNote.length > 0 && !root.noteLocked && !root.resolving; onClicked: root.insertLink() }
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

                    Label {
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
                                        visible: root.conflicts.length > 1
                                        text: "Change " + (hunk.index + 1) + " of " + root.conflicts.length
                                        color: root.colTextMuted
                                        font.pixelSize: root.pt(11)
                                        font.bold: true
                                    }
                                    Label {
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
                                        color: root.colTextDim
                                        font.pixelSize: root.pt(12)
                                        text: "\"Remove the markers\" keeps every line from both versions and drops only the marker lines, for you to read over. "
                                            + (backend.noteHasSyncedCopy ? "\"Use the last synced version\" goes back to the text last synced with iCloud. " : "")
                                            + "Before either, the note as it is now is copied to .icloud-md/conflict-backups in your notes folder, so nothing is lost."
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
                                // The first edit after a while waits for the pull; the
                                // key that triggered it is dropped rather than typed
                                // into a copy about to change.
                                var edits = event.text.length > 0 || event.key === Qt.Key_Backspace
                                    || event.key === Qt.Key_Delete;
                                if (edits && !root.dirty && root.pullIfStale())
                                    event.accepted = true;
                            }
                            wrapMode: TextArea.Wrap
                            selectByMouse: true
                            readOnly: root.noteLocked || root.freshening || root.unreadableConflict
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
                            onCursorPositionChanged: root.trackCursor()
                            onActiveFocusChanged: {
                                root.trackCursor();
                                if (activeFocus)
                                    root.pullIfStale();
                            }
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

    // Like Notes, changes reach iCloud on their own. The sync tool keeps this
    // safe: it merges, refuses a note it cannot push safely (the badges say
    // which; Push… shows why), and deletions only move notes to Recently
    // Deleted. Pushes wait until edits settle so a burst of typing is one push.
    Timer {
        id: autoPush
        interval: 20 * 1000
        onTriggered: {
            if (!backend.cloned || backend.authExpired)
                return;
            if (backend.syncRunning || root.dirty || root.dialogOpen()) {
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
        if (backend.syncRunning || root.dialogOpen() || root.dirty || autoPush.running
                || Date.now() - lastFocusSync < 60 * 1000)
            return;
        lastFocusSync = Date.now();
        backend.runSync(); // both directions: a change from any program in any folder
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
            if (backend.syncRunning || root.dirty || root.dialogOpen() || autoPush.running)
                return;
            root.lastFocusSync = Date.now();
            backend.runSync(); // pending edits go up first, then the pull
        }
    }
    Connections {
        target: backend
        // Counted from the end of the last sync, whatever started it (a
        // focus, a push, Pull), so a poll never follows one within the interval.
        function onSyncChainFinished() {
            if (poll.running)
                poll.restart();
        }
        function onSyncFinished(label, ok) {
            if (label === "Pull")
                root.pollFailures = ok ? 0 : Math.min(root.pollFailures + 1, 4);
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
            if (backend.syncToolAvailable && !backend.authExpired) {
                root.lastFocusSync = Date.now(); // the window's first activation is this sync
                backend.runSync(); // edits made while the app was closed go up first
            }
        } else if (backend.syncToolAvailable && backend.signedIn) {
            root.notice = "Downloading your notes as " + backend.appleId + "…";
            backend.runClone();
        } else {
            onboardDialog.open();
        }
    }
    Component.onCompleted: start()

    Shortcut { sequence: StandardKey.Save; onActivated: root.save() }
    Shortcut { sequence: "Ctrl+N"; onActivated: newNoteDialog.open() }
    Shortcut { sequence: "Ctrl+B"; onActivated: root.wrapSelection("**", "**") }
    Shortcut { sequence: "Ctrl+I"; onActivated: root.wrapSelection("*", "*") }
    Shortcut { sequence: "Ctrl+K"; onActivated: root.insertLink() }
    Shortcut { sequence: "Ctrl+Return"; enabled: editor.activeFocus; onActivated: root.toggleTask() }

    Settings {
        id: settings
        property alias windowWidth: root.width
        property alias windowHeight: root.height
        property var panes
    }
    Component.onDestruction: settings.panes = panes.saveState()
    Shortcut { sequence: StandardKey.Find; onActivated: searchField.forceActiveFocus() }

    Connections {
        target: backend
        function onNoteContentChanged() {
            if (root.keepingEdits)
                return;
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
            // the edits as a new note (editsKeptAsNote).
            root.keepingEdits = true;
            root.keptNotice = "";
            var kept = backend.keepEditsAsConflict(root.savedText, editor.text);
            root.keepingEdits = false;
            if (kept) {
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
            }
        }
        function onEditsKeptAsNote(message) {
            root.keptNotice = message;
        }
        function onCurrentNoteChanged() { root.conflictAsText = false; }
        // Not on syncRunningChanged: that turns false between runSync's
        // push and its pull, which would unlock the editor too early.
        function onSyncChainFinished() { root.freshening = false; }
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
                                label: ({ createFolder: "new folder", renameFolder: "rename folder", deleteFolder: "delete folder" })[modelData.kind]
                                       || modelData.kind
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
                text: "This downloads all your Apple Notes into ~/Documents/icloud-notes as Markdown, one file per note with the title as its first line, like in Notes. "
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
