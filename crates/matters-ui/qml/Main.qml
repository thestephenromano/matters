import QtQuick
import QtQuick.Controls
import QtQuick.Layouts
import QtQuick.Window
import matters

ApplicationWindow {
    id: window
    width: 960
    height: 680
    minimumWidth: 720
    minimumHeight: 500
    visible: true
    title: controller.currentTitle + " — Matters"

    // Theme color palette (respects light/dark system settings)
    readonly property bool isDark: Application.styleHints.colorScheme === Qt.ColorScheme.Dark
    readonly property color colBg: isDark ? "#1A1B1E" : "#FFFFFF"
    readonly property color colSidebarBg: isDark ? "#141517" : "#F7F8FA"
    readonly property color colSidebarHover: isDark ? "#25262B" : "#EBECEF"
    readonly property color colSidebarActive: isDark ? "#2C2E33" : "#E2E4E8"
    readonly property color colTextPrimary: isDark ? "#F8F9FA" : "#1A1B1E"
    readonly property color colTextSecondary: isDark ? "#909296" : "#868E96"
    readonly property color colAccent: "#228BE6" // Things 3 signature blue
    readonly property color colBorder: isDark ? "#2C2E33" : "#E9ECEF"
    readonly property color colCardBg: isDark ? "#202124" : "#FFFFFF"
    readonly property color colEvening: "#FAB005" // Warm gold for evening

    AppController {
        id: controller
    }

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
            color: window.colSidebarBg

            Rectangle {
                anchors.right: parent.right
                anchors.top: parent.top
                anchors.bottom: parent.bottom
                width: 1
                color: window.colBorder
            }

            ColumnLayout {
                anchors.fill: parent
                anchors.margins: 14
                spacing: 8

                // Quick Find Trigger Button
                Rectangle {
                    Layout.fillWidth: true
                    height: 34
                    radius: 6
                    color: searchArea.containsMouse ? window.colSidebarHover : "transparent"
                    border.color: window.colBorder
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
                            color: window.colTextSecondary
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
                            { id: "logbook", title: "Logbook", icon: "📓", count: controller.logbookCount, color: "#51CF66" }
                        ]

                        delegate: Rectangle {
                            id: navItem
                            Layout.fillWidth: true
                            height: 32
                            radius: 6
                            readonly property bool isSelected: controller.currentView === modelData.id
                            color: isSelected ? window.colSidebarActive : (navItemMouse.containsMouse ? window.colSidebarHover : "transparent")

                            RowLayout {
                                anchors.fill: parent
                                anchors.leftMargin: 8
                                anchors.rightMargin: 8
                                spacing: 10

                                Text {
                                    text: modelData.icon
                                    font.pixelSize: 14
                                }

                                Text {
                                    text: modelData.title
                                    color: navItem.isSelected ? window.colAccent : window.colTextPrimary
                                    font.pixelSize: 13
                                    font.weight: navItem.isSelected ? Font.DemiBold : Font.Normal
                                    Layout.fillWidth: true
                                }

                                Rectangle {
                                    visible: modelData.count > 0
                                    height: 18
                                    width: Math.max(18, countText.contentWidth + 10)
                                    radius: 9
                                    color: navItem.isSelected ? window.colAccent : (window.isDark ? "#2C2E33" : "#DEE2E6")

                                    Text {
                                        id: countText
                                        anchors.centerIn: parent
                                        text: modelData.count.toString()
                                        font.pixelSize: 11
                                        font.weight: Font.DemiBold
                                        color: navItem.isSelected ? "#FFFFFF" : window.colTextSecondary
                                    }
                                }
                            }

                            MouseArea {
                                id: navItemMouse
                                anchors.fill: parent
                                hoverEnabled: true
                                cursorShape: Qt.PointingHandCursor
                                onClicked: controller.select_view(modelData.id)
                            }
                        }
                    }
                }

                // Section Divider
                Rectangle {
                    Layout.fillWidth: true
                    height: 1
                    color: window.colBorder
                    Layout.topMargin: 6
                    Layout.bottomMargin: 6
                }

                // Projects Header
                RowLayout {
                    Layout.fillWidth: true

                    Text {
                        text: "PROJECTS"
                        color: window.colTextSecondary
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
                            width: projectsList.width
                            height: 30
                            radius: 6
                            readonly property bool isSelected: controller.currentView === "project" && controller.currentProjectId === modelData.id
                            color: isSelected ? window.colSidebarActive : (projMouse.containsMouse ? window.colSidebarHover : "transparent")

                            RowLayout {
                                anchors.fill: parent
                                anchors.leftMargin: 8
                                anchors.rightMargin: 8
                                spacing: 8

                                // Mini Progress Indicator Circle
                                Rectangle {
                                    width: 12
                                    height: 12
                                    radius: 6
                                    color: "transparent"
                                    border.color: projItem.isSelected ? window.colAccent : window.colTextSecondary
                                    border.width: 1.5
                                }

                                Text {
                                    text: modelData.title
                                    color: projItem.isSelected ? window.colAccent : window.colTextPrimary
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
                                onClicked: controller.select_project(modelData.id)
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
            color: window.colBg

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
                        color: window.colTextPrimary
                        Layout.fillWidth: true
                    }

                    // Circular Progress Display for Projects
                    RowLayout {
                        visible: controller.currentView === "project"
                        spacing: 8

                        Rectangle {
                            width: 24
                            height: 24
                            radius: 12
                            color: "transparent"
                            border.color: window.colAccent
                            border.width: 2

                            Rectangle {
                                anchors.centerIn: parent
                                width: parent.width * controller.currentProjectProgress
                                height: parent.height * controller.currentProjectProgress
                                radius: width / 2
                                color: window.colAccent
                                Behavior on width { NumberAnimation { duration: 200 } }
                                Behavior on height { NumberAnimation { duration: 200 } }
                            }
                        }

                        Text {
                            text: Math.round(controller.currentProjectProgress * 100) + "%"
                            color: window.colTextSecondary
                            font.pixelSize: 13
                            font.weight: Font.DemiBold
                        }
                    }
                }

                // Quick Task Input Bar
                Rectangle {
                    Layout.fillWidth: true
                    height: 42
                    radius: 8
                    color: window.isDark ? "#25262B" : "#F1F3F5"
                    border.color: newTaskInput.activeFocus ? window.colAccent : "transparent"
                    border.width: 1.5

                    RowLayout {
                        anchors.fill: parent
                        anchors.leftMargin: 12
                        anchors.rightMargin: 12
                        spacing: 10

                        Text {
                            text: "+"
                            font.pixelSize: 20
                            color: window.colAccent
                        }

                        TextInput {
                            id: newTaskInput
                            Layout.fillWidth: true
                            font.pixelSize: 14
                            color: window.colTextPrimary
                            clip: true

                            Text {
                                anchors.fill: parent
                                text: "New to-do... (Press Enter to add)"
                                color: window.colTextSecondary
                                font.pixelSize: 14
                                visible: !newTaskInput.text && !newTaskInput.activeFocus
                            }

                            Keys.onReturnPressed: {
                                if (text.trim().length > 0) {
                                    if (controller.currentView === "project") {
                                        controller.add_task_to_project(text, controller.currentProjectId);
                                    } else {
                                        controller.add_task(text, controller.currentView);
                                    }
                                    text = "";
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
                        model: {
                            try {
                                return JSON.parse(controller.tasksJson);
                            } catch (e) {
                                return [];
                            }
                        }

                        // Empty State
                        Text {
                            anchors.centerIn: parent
                            visible: taskListView.count === 0
                            text: "No tasks here. Enjoy your peace of mind."
                            color: window.colTextSecondary
                            font.pixelSize: 15
                        }

                        delegate: Rectangle {
                            id: taskRow
                            width: taskListView.width
                            property bool isExpanded: false
                            property bool isDone: modelData.status === "completed"
                            height: isExpanded ? expandedContent.height + 56 : 48
                            radius: 8
                            color: taskMouse.containsMouse || isExpanded ? (window.isDark ? "#25262B" : "#F8F9FA") : "transparent"
                            border.color: isExpanded ? window.colBorder : "transparent"
                            border.width: 1

                            Behavior on height {
                                NumberAnimation { duration: 180; easing.type: Easing.OutCubic }
                            }

                            ColumnLayout {
                                anchors.fill: parent
                                anchors.margins: 8
                                spacing: 8

                                // Main Row: Checkbox + Title + Date Pills
                                RowLayout {
                                    Layout.fillWidth: true
                                    spacing: 12

                                    // Things-style Circular Checkbox
                                    Rectangle {
                                        id: checkCircle
                                        width: 20
                                        height: 20
                                        radius: 10
                                        color: taskRow.isDone ? window.colAccent : "transparent"
                                        border.color: taskRow.isDone ? window.colAccent : (checkMouse.containsMouse ? window.colAccent : window.colTextSecondary)
                                        border.width: 1.5

                                        Text {
                                            anchors.centerIn: parent
                                            text: "✓"
                                            color: "#FFFFFF"
                                            font.pixelSize: 11
                                            font.weight: Font.Bold
                                            visible: taskRow.isDone
                                        }

                                        MouseArea {
                                            id: checkMouse
                                            anchors.fill: parent
                                            hoverEnabled: true
                                            cursorShape: Qt.PointingHandCursor
                                            onClicked: controller.toggle_task(modelData.id)
                                        }
                                    }

                                    // Title
                                    Text {
                                        text: modelData.title
                                        font.pixelSize: 14
                                        font.strikeout: taskRow.isDone
                                        color: taskRow.isDone ? window.colTextSecondary : window.colTextPrimary
                                        Layout.fillWidth: true
                                        elide: Text.ElideRight
                                    }

                                    // "This Evening" Badge
                                    Rectangle {
                                        visible: modelData.time_of_day === "evening"
                                        height: 20
                                        width: eveningLabel.contentWidth + 12
                                        radius: 4
                                        color: window.isDark ? "#382D16" : "#FFF3BF"

                                        Text {
                                            id: eveningLabel
                                            anchors.centerIn: parent
                                            text: "🌙 Evening"
                                            font.pixelSize: 11
                                            font.weight: Font.DemiBold
                                            color: window.colEvening
                                        }
                                    }

                                    // Schedule Date Badge
                                    Rectangle {
                                        visible: modelData.start_date !== null && modelData.start_date !== undefined
                                        height: 20
                                        width: dateLabel.contentWidth + 12
                                        radius: 4
                                        color: window.isDark ? "#2C2E33" : "#E9ECEF"

                                        Text {
                                            id: dateLabel
                                            anchors.centerIn: parent
                                            text: modelData.start_date ? modelData.start_date : ""
                                            font.pixelSize: 11
                                            color: window.colTextSecondary
                                        }
                                    }

                                    // Quick Delete Button (shows on hover)
                                    ToolButton {
                                        text: "✕"
                                        visible: taskMouse.containsMouse
                                        font.pixelSize: 11
                                        onClicked: controller.delete_task(modelData.id)
                                    }
                                }

                                // In-Place Expanded Detail: Notes & Quick Scheduler
                                ColumnLayout {
                                    id: expandedContent
                                    visible: taskRow.isExpanded
                                    Layout.fillWidth: true
                                    Layout.leftMargin: 32
                                    spacing: 10

                                    // Notes Editor
                                    TextArea {
                                        id: notesInput
                                        Layout.fillWidth: true
                                        Layout.preferredHeight: 60
                                        placeholderText: "Notes (Markdown supported)..."
                                        font.pixelSize: 13
                                        color: window.colTextPrimary
                                        text: modelData.notes || ""
                                        wrapMode: TextArea.Wrap
                                        background: Rectangle {
                                            color: window.isDark ? "#1A1B1E" : "#FFFFFF"
                                            border.color: window.colBorder
                                            radius: 6
                                        }
                                        onTextChanged: {
                                            if (activeFocus) {
                                                controller.update_task_notes(modelData.id, text);
                                            }
                                        }
                                    }

                                    // Quick Schedule Buttons
                                    RowLayout {
                                        spacing: 8

                                        Text {
                                            text: "When:"
                                            color: window.colTextSecondary
                                            font.pixelSize: 12
                                        }

                                        Button {
                                            text: "Today"
                                            font.pixelSize: 11
                                            onClicked: controller.schedule_task(modelData.id, "today")
                                        }

                                        Button {
                                            text: "This Evening"
                                            font.pixelSize: 11
                                            onClicked: controller.schedule_task(modelData.id, "evening")
                                        }

                                        Button {
                                            text: "Someday"
                                            font.pixelSize: 11
                                            onClicked: controller.schedule_task(modelData.id, "someday")
                                        }
                                    }
                                }
                            }

                            // Click row to toggle in-place expansion
                            MouseArea {
                                id: taskMouse
                                anchors.fill: parent
                                hoverEnabled: true
                                propagateComposedEvents: true
                                onClicked: (mouse) => {
                                    if (mouse.x > 36) { // ignore clicks on the checkbox
                                        taskRow.isExpanded = !taskRow.isExpanded;
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
    // QUICK FIND POPUP DIALOG (Ctrl+K)
    // =========================================================================
    Dialog {
        id: quickFindDialog
        x: (parent.width - width) / 2
        y: 80
        width: 520
        height: 380
        modal: true
        focus: true
        closePolicy: Popup.CloseOnEscape | Popup.CloseOnPressOutside

        background: Rectangle {
            color: window.isDark ? "#202124" : "#FFFFFF"
            radius: 10
            border.color: window.colBorder
            border.width: 1
        }

        contentItem: ColumnLayout {
            spacing: 12

            TextField {
                id: searchInput
                Layout.fillWidth: true
                placeholderText: "Quick Find across all tasks, projects, and areas..."
                font.pixelSize: 15
                focus: true
                onTextChanged: controller.search(text)
            }

            ListView {
                id: searchResultsList
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
                    width: searchResultsList.width
                    height: 36
                    radius: 6
                    color: resMouse.containsMouse ? window.colSidebarHover : "transparent"

                    RowLayout {
                        anchors.fill: parent
                        anchors.leftMargin: 10
                        anchors.rightMargin: 10
                        spacing: 8

                        Text {
                            text: "•"
                            color: window.colAccent
                        }

                        Text {
                            text: modelData.title
                            font.pixelSize: 13
                            color: window.colTextPrimary
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
                            quickFindDialog.close();
                        }
                    }
                }
            }
        }
    }

    // New Project Dialog
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
                controller.create_project(projTitleInput.text.trim(), "");
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
