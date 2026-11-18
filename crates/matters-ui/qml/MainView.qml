pragma ComponentBehavior: Bound

import QtQuick
import QtQuick.Controls
import QtQuick.Layouts
import matters

// The whole application UI as a plain Item, so it can be hosted by Main.qml's
// window or embedded directly in a test.
Item {
    id: root

    readonly property string title: controller.currentTitle
    // Exposed for hosts and tests.
    readonly property alias appController: controller
    readonly property alias taskList: taskListView

    readonly property var trashedProjects: {
        try {
            return JSON.parse(controller.trashedProjectsJson);
        } catch (e) {
            return [];
        }
    }

    // What the Undo notice would put back: kind is "task", "project" or ""
    // (no notice).
    property string undoKind: ""
    property string undoId: ""
    property string undoTitle: ""

    function showUndo(kind, id, title) {
        undoKind = kind;
        undoId = id;
        undoTitle = title;
        undoTimer.restart();
    }

    function hideUndo() {
        undoKind = "";
        undoTimer.stop();
    }

    function undo() {
        if (undoKind === "task") {
            controller.restoreTask(undoId);
        } else if (undoKind === "project") {
            controller.restoreProject(undoId);
            controller.selectProject(undoId);
        }
        hideUndo();
    }

    // Theme color palette (respects light/dark system settings)
    readonly property bool isDark: Application.styleHints.colorScheme === Qt.ColorScheme.Dark
    readonly property color colBg: isDark ? "#1A1B1E" : "#FFFFFF"
    readonly property color colSidebarBg: isDark ? "#141517" : "#F7F8FA"
    readonly property color colSidebarHover: isDark ? "#25262B" : "#EBECEF"
    readonly property color colSidebarActive: isDark ? "#2C2E33" : "#E2E4E8"
    readonly property color colTextPrimary: isDark ? "#F8F9FA" : "#1A1B1E"
    readonly property color colTextSecondary: isDark ? "#909296" : "#868E96"
    readonly property color colAccent: "#228BE6"
    readonly property color colBorder: isDark ? "#2C2E33" : "#E9ECEF"
    readonly property color colCardBg: isDark ? "#202124" : "#FFFFFF"
    readonly property color colEvening: "#FAB005" // Warm gold for evening

    AppController {
        id: controller
        // Every state change bumps the revision; reload once per event-loop
        // turn, never from inside a model notification.
        onRevisionChanged: Qt.callLater(root.reloadTasks)
    }

    TaskListModel {
        id: taskModel
    }

    // Today, Upcoming and the counts depend on the date. Check it every minute;
    // a repeating check also catches resume-from-sleep and clock changes.
    Timer {
        interval: 60 * 1000
        repeat: true
        running: true
        onTriggered: controller.checkDate()
    }

    // Task to scroll to once the list has loaded (set by Quick Find).
    property string pendingRevealId: ""

    function reloadTasks() {
        taskModel.load(controller.currentView, controller.currentProjectId, controller.visit);
        if (pendingRevealId !== "") {
            const row = taskModel.rowOf(pendingRevealId);
            if (row >= 0)
                taskListView.positionViewAtIndex(row, ListView.Contain);
            pendingRevealId = "";
        }
    }

    // Scroll the task's row fully into view, if the list shows it. Call it
    // directly or deferred, never from a row's handler during a list layout.
    function scrollToTask(id) {
        const row = taskModel.rowOf(id);
        if (row >= 0)
            taskListView.positionViewAtIndex(row, ListView.Contain);
    }

    Component.onCompleted: reloadTasks()

    // Root Container
    RowLayout {
        anchors.fill: parent
        spacing: 0

        // =====================================================================
        // LEFT SIDEBAR
        // =====================================================================
        Rectangle {
            Layout.fillHeight: true
            Layout.preferredWidth: 240
            Layout.minimumWidth: 200
            Layout.maximumWidth: 320
            color: root.colSidebarBg

            Rectangle {
                anchors.right: parent.right
                anchors.top: parent.top
                anchors.bottom: parent.bottom
                width: 1
                color: root.colBorder
            }

            ColumnLayout {
                anchors.fill: parent
                anchors.margins: 14
                spacing: 8

                // Quick Find Trigger Button
                Rectangle {
                    Layout.fillWidth: true
                    Layout.preferredHeight: 34
                    radius: 6
                    color: searchArea.containsMouse ? root.colSidebarHover : "transparent"
                    border.color: root.colBorder
                    border.width: 1

                    RowLayout {
                        anchors.fill: parent
                        anchors.leftMargin: 10
                        anchors.rightMargin: 10
                        spacing: 8

                        Text {
                            text: "🔍"
                            font.pixelSize: 12
                        }

                        Text {
                            text: "Quick Find (Ctrl+K)"
                            color: root.colTextSecondary
                            font.pixelSize: 12
                            Layout.fillWidth: true
                        }
                    }

                    MouseArea {
                        id: searchArea
                        anchors.fill: parent
                        hoverEnabled: true
                        cursorShape: Qt.PointingHandCursor
                        onClicked: quickFindDialog.open()
                    }
                }

                // Core GTD Navigation
                ColumnLayout {
                    Layout.fillWidth: true
                    spacing: 2

                    Repeater {
                        model: [
                            { id: "inbox", title: "Inbox", icon: "📥", count: controller.inboxCount, color: "#339AF0" },
                            { id: "today", title: "Today", icon: "⭐", count: controller.todayCount, color: "#FCC419" },
                            { id: "upcoming", title: "Upcoming", icon: "📅", count: controller.upcomingCount, color: "#FF6B6B" },
                            { id: "anytime", title: "Anytime", icon: "📦", count: controller.anytimeCount, color: "#20C997" },
                            { id: "someday", title: "Someday", icon: "🗃️", count: controller.somedayCount, color: "#FAB005" },
                            { id: "logbook", title: "Logbook", icon: "📓", count: controller.logbookCount, color: "#51CF66" },
                            { id: "trash", title: "Trash", icon: "🗑️", count: 0, color: "#868E96" }
                        ]

                        delegate: Rectangle {
                            id: navItem
                            required property var modelData
                            Layout.fillWidth: true
                            height: 32
                            radius: 6
                            readonly property bool isSelected: controller.currentView === navItem.modelData.id
                            color: isSelected ? root.colSidebarActive : (navItemMouse.containsMouse ? root.colSidebarHover : "transparent")

                            RowLayout {
                                anchors.fill: parent
                                anchors.leftMargin: 8
                                anchors.rightMargin: 8
                                spacing: 10

                                Text {
                                    text: navItem.modelData.icon
                                    font.pixelSize: 14
                                }

                                Text {
                                    text: navItem.modelData.title
                                    color: navItem.isSelected ? root.colAccent : root.colTextPrimary
                                    font.pixelSize: 13
                                    font.weight: navItem.isSelected ? Font.DemiBold : Font.Normal
                                    Layout.fillWidth: true
                                }

                                Rectangle {
                                    visible: navItem.modelData.count > 0
                                    Layout.preferredHeight: 18
                                    Layout.preferredWidth: Math.max(18, countText.contentWidth + 10)
                                    radius: 9
                                    color: navItem.isSelected ? root.colAccent : (root.isDark ? "#2C2E33" : "#DEE2E6")

                                    Text {
                                        id: countText
                                        anchors.centerIn: parent
                                        text: navItem.modelData.count.toString()
                                        font.pixelSize: 11
                                        font.weight: Font.DemiBold
                                        color: navItem.isSelected ? "#FFFFFF" : root.colTextSecondary
                                    }
                                }
                            }

                            MouseArea {
                                id: navItemMouse
                                anchors.fill: parent
                                hoverEnabled: true
                                cursorShape: Qt.PointingHandCursor
                                onClicked: controller.selectView(navItem.modelData.id)
                            }
                        }
                    }
                }

                // Section Divider
                Rectangle {
                    Layout.fillWidth: true
                    Layout.preferredHeight: 1
                    color: root.colBorder
                    Layout.topMargin: 6
                    Layout.bottomMargin: 6
                }

                // Projects Header
                RowLayout {
                    Layout.fillWidth: true

                    Text {
                        text: "PROJECTS"
                        color: root.colTextSecondary
                        font.pixelSize: 11
                        font.weight: Font.Bold
                        Layout.fillWidth: true
                    }

                    ToolButton {
                        text: "+"
                        font.pixelSize: 14
                        onClicked: newProjectDialog.open()
                    }
                }

                // Projects List
                ScrollView {
                    Layout.fillWidth: true
                    Layout.fillHeight: true
                    clip: true

                    ListView {
                        id: projectsList
                        spacing: 2
                        model: {
                            try {
                                return JSON.parse(controller.projectsJson);
                            } catch (e) {
                                return [];
                            }
                        }

                        delegate: Rectangle {
                            id: projItem
                            required property var modelData
                            width: projectsList.width
                            height: 30
                            radius: 6
                            readonly property bool isSelected: controller.currentView === "project" && controller.currentProjectId === projItem.modelData.id
                            color: isSelected ? root.colSidebarActive : (projMouse.containsMouse ? root.colSidebarHover : "transparent")

                            RowLayout {
                                anchors.fill: parent
                                anchors.leftMargin: 8
                                anchors.rightMargin: 8
                                spacing: 8

                                // Mini Progress Indicator Circle
                                Rectangle {
                                    Layout.preferredWidth: 12
                                    Layout.preferredHeight: 12
                                    radius: 6
                                    color: "transparent"
                                    border.color: projItem.isSelected ? root.colAccent : root.colTextSecondary
                                    border.width: 1.5
                                }

                                Text {
                                    text: projItem.modelData.title
                                    color: projItem.isSelected ? root.colAccent : root.colTextPrimary
                                    font.pixelSize: 13
                                    font.weight: projItem.isSelected ? Font.DemiBold : Font.Normal
                                    elide: Text.ElideRight
                                    Layout.fillWidth: true
                                }
                            }

                            MouseArea {
                                id: projMouse
                                anchors.fill: parent
                                hoverEnabled: true
                                cursorShape: Qt.PointingHandCursor
                                onClicked: controller.selectProject(projItem.modelData.id)
                            }
                        }
                    }
                }
            }
        }

        // =====================================================================
        // MAIN CONTENT AREA
        // =====================================================================
        Rectangle {
            Layout.fillWidth: true
            Layout.fillHeight: true
            color: root.colBg

            ColumnLayout {
                anchors.fill: parent
                anchors.margins: 36
                spacing: 20

                // Title & Action Bar
                RowLayout {
                    Layout.fillWidth: true
                    spacing: 12

                    Text {
                        text: controller.currentTitle
                        font.pixelSize: 28
                        font.weight: Font.Bold
                        color: root.colTextPrimary
                        Layout.fillWidth: true
                    }

                    Button {
                        id: emptyTrashButton
                        objectName: "emptyTrashButton"
                        text: "Empty Trash"
                        visible: controller.currentView === "trash"
                            && (taskListView.count > 0 || root.trashedProjects.length > 0)
                        onClicked: emptyTrashDialog.open()
                        // Once the Trash is empty this button hides; it must not
                        // keep the keyboard focus the dialog hands back to it.
                        // Depending on the style, the dialog finishes closing
                        // before or after the list empties, so both orders are
                        // handled: here, and in the dialog's onClosed.
                        onVisibleChanged: if (!visible && activeFocus) taskListView.forceActiveFocus()
                    }

                    ToolButton {
                        objectName: "trashProjectButton"
                        text: "Move to Trash"
                        visible: controller.currentView === "project"
                        ToolTip.visible: hovered
                        ToolTip.text: "Move this project and its tasks to the Trash"
                        onClicked: {
                            const id = controller.currentProjectId;
                            root.showUndo("project", id, controller.currentTitle);
                            controller.trashProject(id);
                        }
                    }

                    // Circular Progress Display for Projects
                    RowLayout {
                        visible: controller.currentView === "project"
                        spacing: 8

                        Rectangle {
                            Layout.preferredWidth: 24
                            Layout.preferredHeight: 24
                            radius: 12
                            color: "transparent"
                            border.color: root.colAccent
                            border.width: 2

                            Rectangle {
                                anchors.centerIn: parent
                                width: parent.width * controller.currentProjectProgress
                                height: parent.height * controller.currentProjectProgress
                                radius: width / 2
                                color: root.colAccent
                                Behavior on width { NumberAnimation { duration: 200 } }
                                Behavior on height { NumberAnimation { duration: 200 } }
                            }
                        }

                        Text {
                            text: Math.round(controller.currentProjectProgress * 100) + "%"
                            color: root.colTextSecondary
                            font.pixelSize: 13
                            font.weight: Font.DemiBold
                        }
                    }
                }

                // Error banner: failed commands are reported, never dropped.
                Rectangle {
                    id: errorBanner
                    readonly property string message: controller.errorMessage || taskModel.errorMessage
                    visible: message !== ""
                    Layout.fillWidth: true
                    implicitHeight: errorRow.implicitHeight + 16
                    radius: 8
                    color: root.isDark ? "#3B1F1F" : "#FFE3E3"

                    RowLayout {
                        id: errorRow
                        anchors.fill: parent
                        anchors.margins: 8
                        spacing: 8

                        Text {
                            text: errorBanner.message
                            color: root.isDark ? "#FFA8A8" : "#C92A2A"
                            font.pixelSize: 13
                            wrapMode: Text.Wrap
                            Layout.fillWidth: true
                        }

                        ToolButton {
                            text: "✕"
                            visible: controller.errorMessage !== ""
                            onClicked: controller.clearError()
                        }
                    }
                }

                // Quick Task Input Bar
                Rectangle {
                    Layout.fillWidth: true
                    Layout.preferredHeight: 42
                    radius: 8
                    color: root.isDark ? "#25262B" : "#F1F3F5"
                    border.color: newTaskInput.activeFocus ? root.colAccent : "transparent"
                    border.width: 1.5

                    RowLayout {
                        anchors.fill: parent
                        anchors.leftMargin: 12
                        anchors.rightMargin: 12
                        spacing: 10

                        Text {
                            text: "+"
                            font.pixelSize: 20
                            color: root.colAccent
                        }

                        TextInput {
                            id: newTaskInput
                            objectName: "newTaskInput"
                            Layout.fillWidth: true
                            font.pixelSize: 14
                            color: root.colTextPrimary
                            clip: true

                            Text {
                                anchors.fill: parent
                                text: "New to-do... (Press Enter to add)"
                                color: root.colTextSecondary
                                font.pixelSize: 14
                                visible: !newTaskInput.text && !newTaskInput.activeFocus
                            }

                            Keys.onReturnPressed: {
                                if (text.trim().length > 0) {
                                    controller.addTask(text);
                                    text = "";
                                }
                            }
                        }
                    }
                }

                // Trashed projects, each standing for its tasks.
                ColumnLayout {
                    visible: controller.currentView === "trash" && root.trashedProjects.length > 0
                    Layout.fillWidth: true
                    spacing: 4

                    Repeater {
                        model: root.trashedProjects

                        delegate: Rectangle {
                            id: trashedProject
                            objectName: "trashedProject"
                            required property var modelData
                            Layout.fillWidth: true
                            Layout.preferredHeight: 40
                            radius: 8
                            color: "transparent"
                            border.color: root.colBorder
                            border.width: 1

                            RowLayout {
                                anchors.fill: parent
                                anchors.leftMargin: 12
                                anchors.rightMargin: 8
                                spacing: 8

                                Text {
                                    text: "📁 " + trashedProject.modelData.title
                                    color: root.colTextPrimary
                                    font.pixelSize: 14
                                    elide: Text.ElideRight
                                    Layout.fillWidth: true
                                }

                                ToolButton {
                                    objectName: "restoreProjectButton"
                                    text: "Put Back"
                                    font.pixelSize: 11
                                    onClicked: controller.restoreProject(trashedProject.modelData.id)
                                }
                            }
                        }
                    }
                }

                // Task List View
                ScrollView {
                    Layout.fillWidth: true
                    Layout.fillHeight: true
                    clip: true

                    ListView {
                        id: taskListView
                        spacing: 6
                        model: taskModel

                        // Expansion is keyed by task id and kept here, outside the
                        // delegates, so it survives list updates and scrolling.
                        property string expandedTaskId: ""

                        // Empty State
                        Text {
                            anchors.centerIn: parent
                            visible: taskListView.count === 0
                            text: "No tasks here. Enjoy your peace of mind."
                            color: root.colTextSecondary
                            font.pixelSize: 15
                        }

                        // A FocusScope: when the ListView focuses a row (it does so
                        // for its current item as rows come and go), focus goes to
                        // whatever inside the row last had it, such as the notes
                        // editor, instead of the row itself (issue #1).
                        delegate: FocusScope {
                            id: taskRow
                            objectName: "taskRow"
                            required property string taskId
                            required property string title
                            required property string notes
                            required property bool done
                            required property string startDate
                            required property bool evening
                            required property int index

                            readonly property bool isExpanded: taskListView.expandedTaskId === taskId

                            // The content keeps its natural height, pinned to the top;
                            // the row grows to reveal it and clips the rest, so the
                            // header stays put while the height animates.
                            readonly property real targetHeight: isExpanded ? rowContent.implicitHeight + 16 : 48
                            width: taskListView.width
                            height: targetHeight
                            clip: true

                            // Once expanded, scroll the whole row into view, so its
                            // notes and When buttons don't open below the fold. This
                            // can fire while the ListView is laying out (a row created
                            // already expanded), and repositioning the view from inside
                            // its own layout crashes Qt, so the scroll is deferred.
                            onHeightChanged: {
                                if (isExpanded && height === targetHeight)
                                    Qt.callLater(root.scrollToTask, taskId);
                            }

                            Rectangle {
                                anchors.fill: parent
                                radius: 8
                                color: rowHover.hovered || taskRow.isExpanded ? (root.isDark ? "#25262B" : "#F8F9FA") : "transparent"
                                border.color: taskRow.isExpanded ? root.colBorder : "transparent"
                                border.width: 1
                            }

                            Behavior on height {
                                NumberAnimation { duration: 180; easing.type: Easing.OutCubic }
                            }

                            // Passive hover tracking: unlike a MouseArea it never
                            // blocks clicks from reaching the controls in the row.
                            HoverHandler { id: rowHover }

                            // Click on empty row space toggles expansion. Declared
                            // first, so it sits beneath the row's controls and only
                            // receives clicks that none of them accept.
                            MouseArea {
                                anchors.fill: parent
                                onClicked: taskListView.expandedTaskId = taskRow.isExpanded ? "" : taskRow.taskId
                            }

                            ColumnLayout {
                                id: rowContent
                                anchors.top: parent.top
                                anchors.left: parent.left
                                anchors.right: parent.right
                                anchors.margins: 8
                                spacing: 8

                                // Main Row: Checkbox + Title + Date Pills. Fixed
                                // height: the collapsed row shows exactly this.
                                RowLayout {
                                    Layout.fillWidth: true
                                    Layout.preferredHeight: 32
                                    Layout.maximumHeight: 32
                                    spacing: 12

                                    // Circular checkbox
                                    Rectangle {
                                        id: checkCircle
                                        objectName: "checkCircle"
                                        Layout.preferredWidth: 20
                                        Layout.preferredHeight: 20
                                        radius: 10
                                        color: taskRow.done ? root.colAccent : "transparent"
                                        border.color: taskRow.done ? root.colAccent : (checkMouse.containsMouse ? root.colAccent : root.colTextSecondary)
                                        border.width: 1.5

                                        Text {
                                            anchors.centerIn: parent
                                            text: "✓"
                                            color: "#FFFFFF"
                                            font.pixelSize: 11
                                            font.weight: Font.Bold
                                            visible: taskRow.done
                                        }

                                        MouseArea {
                                            id: checkMouse
                                            anchors.fill: parent
                                            hoverEnabled: true
                                            cursorShape: Qt.PointingHandCursor
                                            onClicked: controller.toggleTask(taskRow.taskId)
                                        }
                                    }

                                    // Title
                                    Text {
                                        text: taskRow.title
                                        font.pixelSize: 14
                                        font.strikeout: taskRow.done
                                        color: taskRow.done ? root.colTextSecondary : root.colTextPrimary
                                        Layout.fillWidth: true
                                        elide: Text.ElideRight
                                    }

                                    // "This Evening" Badge
                                    Rectangle {
                                        visible: taskRow.evening
                                        Layout.preferredHeight: 20
                                        Layout.preferredWidth: eveningLabel.contentWidth + 12
                                        radius: 4
                                        color: root.isDark ? "#382D16" : "#FFF3BF"

                                        Text {
                                            id: eveningLabel
                                            anchors.centerIn: parent
                                            text: "🌙 Evening"
                                            font.pixelSize: 11
                                            font.weight: Font.DemiBold
                                            color: root.colEvening
                                        }
                                    }

                                    // Schedule Date Badge
                                    Rectangle {
                                        visible: taskRow.startDate !== ""
                                        Layout.preferredHeight: 20
                                        Layout.preferredWidth: dateLabel.contentWidth + 12
                                        radius: 4
                                        color: root.isDark ? "#2C2E33" : "#E9ECEF"

                                        Text {
                                            id: dateLabel
                                            anchors.centerIn: parent
                                            text: taskRow.startDate
                                            font.pixelSize: 11
                                            color: root.colTextSecondary
                                        }
                                    }

                                    // Move to Trash (shows on hover). Nothing is
                                    // destroyed until the Trash is emptied.
                                    ToolButton {
                                        objectName: "deleteButton"
                                        text: "✕"
                                        visible: rowHover.hovered && controller.currentView !== "trash"
                                        font.pixelSize: 11
                                        ToolTip.visible: hovered
                                        ToolTip.text: "Move to Trash"
                                        onClicked: {
                                            root.showUndo("task", taskRow.taskId, taskRow.title);
                                            controller.trashTask(taskRow.taskId);
                                        }
                                    }

                                    // In the Trash, each task can be put back.
                                    ToolButton {
                                        objectName: "restoreButton"
                                        text: "Put Back"
                                        visible: controller.currentView === "trash"
                                        font.pixelSize: 11
                                        onClicked: controller.restoreTask(taskRow.taskId)
                                    }
                                }

                                // In-Place Expanded Detail: Notes & Quick Scheduler
                                ColumnLayout {
                                    id: expandedContent
                                    visible: taskRow.isExpanded
                                    Layout.fillWidth: true
                                    Layout.leftMargin: 32
                                    spacing: 10

                                    // Notes Editor. Saves after a pause in typing and
                                    // when editing ends, not on every keystroke.
                                    TextArea {
                                        id: notesInput
                                        objectName: "notesInput"
                                        Layout.fillWidth: true
                                        Layout.preferredHeight: 60
                                        placeholderText: "Notes (Markdown supported)..."
                                        font.pixelSize: 13
                                        color: root.colTextPrimary
                                        wrapMode: TextArea.Wrap
                                        background: Rectangle {
                                            color: root.isDark ? "#1A1B1E" : "#FFFFFF"
                                            border.color: root.colBorder
                                            radius: 6
                                        }

                                        function save() {
                                            saveTimer.stop();
                                            if (text !== taskRow.notes)
                                                controller.updateTaskNotes(taskRow.taskId, text);
                                        }

                                        // Not a binding: while the user is typing, the
                                        // stored notes must not overwrite the editor.
                                        Component.onCompleted: text = taskRow.notes
                                        Connections {
                                            target: taskRow
                                            function onNotesChanged() {
                                                if (!notesInput.activeFocus)
                                                    notesInput.text = taskRow.notes;
                                            }
                                        }

                                        onTextChanged: if (activeFocus) saveTimer.restart()
                                        onEditingFinished: save()
                                        Component.onDestruction: save()

                                        Timer {
                                            id: saveTimer
                                            interval: 600
                                            onTriggered: notesInput.save()
                                        }
                                    }

                                    // Quick Schedule Buttons
                                    RowLayout {
                                        spacing: 8

                                        Text {
                                            text: "When:"
                                            color: root.colTextSecondary
                                            font.pixelSize: 12
                                        }

                                        Repeater {
                                            model: [
                                                { label: "Today", when: "today" },
                                                { label: "This Evening", when: "evening" },
                                                { label: "Someday", when: "someday" }
                                            ]
                                            delegate: Button {
                                                required property var modelData
                                                objectName: "schedule-" + modelData.when
                                                text: modelData.label
                                                font.pixelSize: 11
                                                onClicked: controller.scheduleTask(taskRow.taskId, modelData.when)
                                            }
                                        }
                                    }
                                }
                            }
                        }
                    }
                }
            }
        }
    }

    // =========================================================================
    // UNDO NOTICE: shown for a few seconds after moving something to the Trash
    // =========================================================================
    Timer {
        id: undoTimer
        interval: 5000
        onTriggered: root.hideUndo()
    }

    Rectangle {
        objectName: "undoToast"
        visible: root.undoKind !== ""
        z: 10
        anchors.horizontalCenter: parent.horizontalCenter
        anchors.bottom: parent.bottom
        anchors.bottomMargin: 24
        width: undoRow.implicitWidth + 24
        height: 40
        radius: 8
        color: root.isDark ? "#E9ECEF" : "#212529"

        RowLayout {
            id: undoRow
            anchors.centerIn: parent
            spacing: 12

            Text {
                text: (root.undoKind === "project" ? "Project" : "Task") + " “" + root.undoTitle + "” moved to Trash"
                color: root.isDark ? "#212529" : "#F8F9FA"
                font.pixelSize: 13
                elide: Text.ElideMiddle
                Layout.maximumWidth: 360
            }

            Button {
                objectName: "undoButton"
                text: "Undo"
                flat: true
                onClicked: root.undo()
            }
        }
    }

    // =========================================================================
    // QUICK FIND POPUP DIALOG (Ctrl+K)
    // =========================================================================
    Dialog {
        id: quickFindDialog
        objectName: "quickFindDialog"
        x: (parent.width - width) / 2
        y: 80
        width: 520
        height: 380
        modal: true
        focus: true
        closePolicy: Popup.CloseOnEscape | Popup.CloseOnPressOutside

        background: Rectangle {
            color: root.isDark ? "#202124" : "#FFFFFF"
            radius: 10
            border.color: root.colBorder
            border.width: 1
        }

        contentItem: ColumnLayout {
            spacing: 12

            TextField {
                id: searchInput
                objectName: "searchInput"
                Layout.fillWidth: true
                placeholderText: "Quick Find across all tasks, projects, and areas..."
                font.pixelSize: 15
                focus: true
                onTextChanged: controller.search(text)
            }

            ListView {
                id: searchResultsList
                objectName: "searchResults"
                Layout.fillWidth: true
                Layout.fillHeight: true
                clip: true
                model: {
                    try {
                        return JSON.parse(controller.searchResultsJson);
                    } catch (e) {
                        return [];
                    }
                }

                delegate: Rectangle {
                    id: resultItem
                    required property var modelData
                    width: searchResultsList.width
                    height: 36
                    radius: 6
                    color: resMouse.containsMouse ? root.colSidebarHover : "transparent"

                    RowLayout {
                        anchors.fill: parent
                        anchors.leftMargin: 10
                        anchors.rightMargin: 10
                        spacing: 8

                        Text {
                            text: "•"
                            color: root.colAccent
                        }

                        Text {
                            text: resultItem.modelData.title
                            font.pixelSize: 13
                            color: root.colTextPrimary
                            Layout.fillWidth: true
                            elide: Text.ElideRight
                        }
                    }

                    MouseArea {
                        id: resMouse
                        anchors.fill: parent
                        hoverEnabled: true
                        cursorShape: Qt.PointingHandCursor
                        onClicked: {
                            const id = resultItem.modelData.id;
                            root.pendingRevealId = id;
                            taskListView.expandedTaskId = id;
                            controller.revealTask(id);
                            quickFindDialog.close();
                        }
                    }
                }
            }
        }
    }

    // New Project Dialog
    Dialog {
        id: emptyTrashDialog
        objectName: "emptyTrashDialog"
        title: "Empty Trash?"
        modal: true
        x: (parent.width - width) / 2
        y: (parent.height - height) / 2
        width: 380

        Label {
            width: parent.width
            wrapMode: Text.WordWrap
            text: "Everything in the Trash will be permanently deleted, including the tasks in trashed projects. This can't be undone."
        }

        footer: DialogButtonBox {
            Button {
                objectName: "confirmEmptyTrash"
                text: "Delete Permanently"
                DialogButtonBox.buttonRole: DialogButtonBox.AcceptRole
            }
            Button {
                objectName: "cancelEmptyTrash"
                text: "Cancel"
                DialogButtonBox.buttonRole: DialogButtonBox.RejectRole
            }
        }

        onAccepted: {
            root.hideUndo(); // nothing left to put back
            controller.emptyTrash();
        }
        // Focus returns to the button that opened the dialog, which hides once
        // the Trash is empty; give it to the task list instead.
        onClosed: if (!emptyTrashButton.visible) taskListView.forceActiveFocus()
    }

    Dialog {
        id: newProjectDialog
        title: "Create New Project"
        standardButtons: Dialog.Ok | Dialog.Cancel
        x: (parent.width - width) / 2
        y: (parent.height - height) / 2
        width: 360

        ColumnLayout {
            spacing: 12
            TextField {
                id: projTitleInput
                placeholderText: "Project Title..."
                Layout.fillWidth: true
                focus: true
            }
        }

        onAccepted: {
            if (projTitleInput.text.trim().length > 0) {
                controller.createProject(projTitleInput.text.trim());
                projTitleInput.text = "";
            }
        }
    }

    // Global Keyboard Shortcuts
    Shortcut {
        sequence: "Ctrl+K"
        onActivated: quickFindDialog.open()
    }
    Shortcut {
        sequence: "/"
        onActivated: quickFindDialog.open()
    }
}
