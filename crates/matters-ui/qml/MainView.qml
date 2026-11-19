pragma ComponentBehavior: Bound

import QtQuick
import QtQuick.Controls
import QtQuick.Layouts
import Qt.labs.qmlmodels
import matters

// The whole application UI as a plain Item, so it can be hosted by Main.qml's
// window or embedded directly in a test.
Item {
    id: root

    readonly property string title: controller.currentTitle
    // Exposed for hosts and tests.
    readonly property alias appController: controller
    readonly property alias taskList: taskListView

    // No animations when the desktop asks for reduced motion (#77). Writable
    // so tests can try both.
    property bool reduceMotion: controller.reduceMotion

    readonly property var projects: {
        try {
            return JSON.parse(controller.projectsJson);
        } catch (e) {
            return [];
        }
    }

    readonly property var areas: {
        try {
            return JSON.parse(controller.areasJson);
        } catch (e) {
            return [];
        }
    }

    readonly property var trashedAreas: {
        try {
            return JSON.parse(controller.trashedAreasJson);
        } catch (e) {
            return [];
        }
    }

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
        } else if (undoKind === "area") {
            controller.restoreArea(undoId);
            controller.selectArea(undoId);
        } else if (undoKind === "heading") {
            controller.restoreHeading();
        }
        hideUndo();
    }

    // Theme color palette (respects light/dark system settings)
    // "system" follows the desktop's light or dark scheme; "light" and "dark"
    // choose one (for a future Appearance preference, #72, and for tests: a
    // desktop without a theme ignores an app's request to switch scheme).
    property string appearance: "system"

    // The sidebar can be hidden (F9 or the header's toggle) to give the list
    // the whole window.
    property bool sidebarShown: true
    readonly property bool isDark: appearance === "dark"
        || (appearance === "system" && Application.styleHints.colorScheme === Qt.ColorScheme.Dark)
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

    // Controls (buttons, fields, menus, tooltips) take their colours from
    // this palette, built from the colours above, rather than from the
    // desktop's, which can disagree with Matters' light or dark scheme.
    palette {
        window: colCardBg
        windowText: colTextPrimary
        base: isDark ? "#25262B" : "#FFFFFF"
        alternateBase: colSidebarBg
        text: colTextPrimary
        placeholderText: colTextSecondary
        button: isDark ? "#2C2E33" : "#F1F3F5"
        buttonText: colTextPrimary
        brightText: "#FFFFFF"
        highlight: colAccent
        highlightedText: "#FFFFFF"
        accent: colAccent
        link: colAccent
        light: isDark ? "#373A40" : "#FFFFFF"
        midlight: isDark ? "#303237" : "#E9ECEF"
        mid: isDark ? "#3A3C42" : "#CED4DA"
        dark: isDark ? "#101113" : "#ADB5BD"
        shadow: "#000000"
        toolTipBase: isDark ? "#373A40" : "#25262B"
        toolTipText: "#F8F9FA"
    }
    // Popups (menus, dialogs, popovers, tooltips) take their palette from
    // the window rather than from the item that declares them.
    Binding {
        target: root.Window.window
        property: "palette"
        value: root.palette
        when: root.Window.window !== null
    }

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
        // A new visit (navigation) starts with nothing selected.
        if (controller.visit !== loadedVisit) {
            loadedVisit = controller.visit;
            taskListView.selectedTaskId = "";
            newBelowId = "";
        }
        // A project or area view loads the tasks of that project or area.
        const container = controller.currentView === "area" ? controller.currentAreaId : controller.currentProjectId;
        taskModel.load(controller.currentView, container, controller.visit);
        if (pendingRevealId !== "") {
            if (taskModel.rowOf(pendingRevealId) >= 0) {
                taskListView.selectedTaskId = pendingRevealId;
                scrollToTask(pendingRevealId);
            }
            pendingRevealId = "";
        }
    }

    // Scroll the task's row fully into view, if the list shows it. Call it
    // directly or deferred, never from a row's handler during a list layout.
    function scrollToTask(id) {
        const row = taskModel.rowOf(id);
        if (row < 0)
            return;
        // Finish any pending layout (new rows, section headings) first, so the
        // position is final. Safe here: never called from a row's handler
        // during a layout.
        taskListView.forceLayout();
        taskListView.positionViewAtIndex(row, ListView.Contain);
    }

    property int loadedVisit: -1

    // Capture a task into the current view. When the view lists it, the new
    // task is selected and scrolled into view, so it never appears off-screen.
    function captureTask(title) {
        const id = controller.addTask(title);
        if (id !== "")
            pendingRevealId = id;
        return id;
    }

    // =========================================================================
    // KEYBOARD COMMANDS on the selected task (SPEC §4)
    // =========================================================================

    function selectRow(row) {
        const id = taskModel.idAt(row);
        if (id === "")
            return;
        taskListView.selectedTaskId = id;
        taskListView.positionViewAtIndex(row, ListView.Contain);
    }

    // Move the selection by `step` rows; with nothing selected, Down selects
    // the first task and Up the last.
    function moveSelection(step) {
        const count = taskListView.count;
        if (count === 0)
            return;
        const current = taskModel.rowOf(taskListView.selectedTaskId);
        let row = current < 0 ? (step > 0 ? 0 : count - 1)
                              : Math.max(0, Math.min(count - 1, current + step));
        // An empty heading's row holds no task: go on past it.
        const direction = current < 0 ? (step > 0 ? 1 : -1) : Math.sign(step);
        while (row >= 0 && row < count && taskModel.idAt(row) === "")
            row += direction;
        if (row >= 0 && row < count)
            selectRow(row);
    }

    function moveSelected(step) {
        const id = taskListView.selectedTaskId;
        if (taskModel.rowOf(id) < 0 || !controller.reorderable)
            return;
        controller.moveTask(id, step);
        // The list reloads on the next turn of the event loop; keep the
        // moved task in view once it has.
        Qt.callLater(root.scrollToTask, id);
    }

    // ---- Drag and drop (#53) -------------------------------------------
    // Rows are dragged by their grip. While dragging, a line shows where the
    // item will land; dropping asks the core to place it there.

    // Where an item dragged from row `from` of `list` would land with the
    // pointer at `scenePos`: { afterId, section, y }, or null when it would
    // stay where it is. Rows belong to sections (`sectionAt(i)`; Today's are
    // daytime and This Evening, other lists have one). The section is the one
    // of the row under the pointer, or of the row below when the pointer is
    // in a gap or on a section heading; `afterId` is the row it lands after
    // within that section ("" when first). `y` is the gap's position in the
    // list's content, for the drop line.
    function dropSlot(list, from, idAt, sectionAt, scenePos) {
        const count = list.count;
        const p = list.contentItem.mapFromItem(null, scenePos.x, scenePos.y);
        // r: the row under the pointer, else the first row below it (the drop
        // then goes before it), else the last row.
        let r = -1;
        let before = true;
        for (let i = 0; i < count; ++i) {
            const item = list.itemAtIndex(i);
            if (!item)
                continue;
            if (p.y < item.y) {
                r = i;
                before = true;
                break;
            }
            if (p.y < item.y + item.height) {
                r = i;
                before = p.y < item.y + item.height / 2;
                break;
            }
            r = i;
            before = false;
        }
        if (r < 0)
            return null;
        const section = sectionAt(r);
        const k = before ? r : r + 1; // insert before row k
        if (section === sectionAt(from) && (k === from || k === from + 1))
            return null;
        // The row it lands after: the nearest row above k in the same section.
        let above = k - 1;
        if (above === from)
            above--;
        const afterId = above >= 0 && sectionAt(above) === section ? idAt(above) : "";

        const spacing = list.spacing / 2;
        const anchor = list.itemAtIndex(r);
        const y = before ? anchor.y - spacing : anchor.y + anchor.height + spacing;
        return { afterId: afterId, section: section, y: y };
    }

    // ---- New task below the selection (Enter, SPEC §4) ------------------
    // A field opens under the selected task; nothing is created until a title
    // is typed. Enter saves the task right below and moves the field under it,
    // so a list can be typed in one go; Escape or an empty Enter closes it.

    property string newBelowId: ""

    function startNewBelow() {
        const id = taskListView.selectedTaskId;
        if (taskModel.rowOf(id) < 0)
            return;
        newBelowId = id;
        Qt.callLater(root.scrollToTask, id);
    }

    function closeNewBelow() {
        newBelowId = "";
        taskListView.forceActiveFocus();
    }

    function commitNewBelow(belowId, input) {
        const title = input.text;
        input.text = "";
        const id = title.trim() === "" ? "" : controller.addTaskBelow(title, belowId);
        if (id === "") {
            closeNewBelow();
            return;
        }
        taskListView.selectedTaskId = id;
        newBelowId = id; // the field moves under the new task once it is listed
        Qt.callLater(root.scrollToTask, id);
    }

    // The task whose title should take keyboard focus once it is open (F2).
    property string titleEditId: ""

    function editSelectedTitle() {
        const id = taskListView.selectedTaskId;
        if (taskModel.rowOf(id) < 0)
            return;
        taskListView.expandedTaskId = id;
        titleEditId = id;
    }

    property string dragTaskId: ""
    property var taskDrop: null
    property point taskDragPos

    function beginTaskDrag(id) {
        dragTaskId = id;
        taskDrop = null;
        taskListView.selectedTaskId = id;
    }

    function updateTaskDrag(scenePos) {
        taskDragPos = scenePos;
        const from = taskModel.rowOf(dragTaskId);
        if (from < 0) {
            taskDrop = null;
            return;
        }
        // Today is split into daytime and This Evening, and a project into
        // its headings; dropping in another section moves the task there
        // (decided on #53 and #22).
        const view = controller.currentView;
        const sectionAt = view === "today" ? (row => taskModel.eveningAt(row))
            : view === "project" ? (row => taskModel.headingAt(row))
            : (row => false);
        taskDrop = dropSlot(taskListView, from, row => taskModel.idAt(row), sectionAt, scenePos);
    }

    function endTaskDrag() {
        const id = dragTaskId;
        const drop = taskDrop;
        dragTaskId = "";
        taskDrop = null;
        if (drop && id !== "") {
            if (controller.currentView === "today")
                controller.placeTodayTask(id, drop.section, drop.afterId);
            else if (controller.currentView === "project")
                controller.placeProjectTask(id, drop.section, drop.afterId);
            else
                controller.placeTask(id, drop.afterId);
            Qt.callLater(root.scrollToTask, id);
        }
    }

    property string dragProjectId: ""
    property var projectDrop: null

    function updateProjectDrag(scenePos) {
        const projects = projectsList.model;
        const from = projects.findIndex(p => p.id === dragProjectId);
        projectDrop = from < 0 ? null
            : dropSlot(projectsList, from, row => projects[row].id, row => false, scenePos);
    }

    function endProjectDrag() {
        const id = dragProjectId;
        const drop = projectDrop;
        dragProjectId = "";
        projectDrop = null;
        if (drop && id !== "")
            controller.placeProject(id, drop.afterId);
    }

    // Scroll the task list while a dragged task is held near its top or
    // bottom edge.
    Timer {
        interval: 30
        repeat: true
        running: root.dragTaskId !== ""
        onTriggered: {
            const p = taskListView.mapFromItem(null, root.taskDragPos.x, root.taskDragPos.y);
            const edge = 40;
            const maxY = Math.max(0, taskListView.contentHeight - taskListView.height);
            let y = taskListView.contentY;
            if (p.y < edge)
                y = Math.max(0, y - 12);
            else if (p.y > taskListView.height - edge)
                y = Math.min(maxY, y + 12);
            if (y !== taskListView.contentY) {
                taskListView.contentY = y;
                root.updateTaskDrag(root.taskDragPos);
            }
        }
    }

    function toggleSelectedExpansion() {
        const id = taskListView.selectedTaskId;
        if (taskModel.rowOf(id) < 0)
            return;
        taskListView.expandedTaskId = taskListView.expandedTaskId === id ? "" : id;
    }

    // Move the selected task to the Trash and select its neighbour, so
    // repeated presses work through the list.
    function trashSelected() {
        const id = taskListView.selectedTaskId;
        const row = taskModel.rowOf(id);
        if (row < 0)
            return;
        showUndo("task", id, taskModel.titleAt(row));
        taskListView.selectedTaskId = taskModel.idAt(row + 1) || taskModel.idAt(row - 1);
        controller.trashTask(id);
    }

    // ---- When popover (Ctrl+S, the Date… button; #59) ------------------

    // Numeric dates typed by the user follow the locale: 12/24 or 24/12.
    readonly property bool monthFirst: {
        const format = Qt.locale().dateFormat(Locale.ShortFormat);
        return format.indexOf("M") < format.indexOf("d");
    }

    // The popover sets a task's When or, in "deadline" mode, its deadline
    // (Ctrl+D, Deadline…; #20).
    function openDatePopup(id, mode) {
        const row = taskModel.rowOf(id);
        if (row < 0)
            return;
        taskListView.selectedTaskId = id;
        scrollToTask(id);
        whenPopup.taskId = id;
        whenPopup.mode = mode;
        whenPopup.open();
    }

    function openWhenFor(id) {
        openDatePopup(id, "when");
    }

    function openDeadlineFor(id) {
        openDatePopup(id, "deadline");
    }

    function openWhenMenu() {
        openWhenFor(taskListView.selectedTaskId);
    }

    // A date picked on the calendar; past days can't be chosen.
    function applyWhenDate(date) {
        const today = new Date();
        today.setHours(0, 0, 0, 0);
        if (date < today)
            return;
        const iso = Qt.formatDate(date, "yyyy-MM-dd");
        if (whenPopup.mode === "deadline")
            controller.setTaskDeadline(whenPopup.taskId, iso);
        else
            controller.scheduleTask(whenPopup.taskId, iso);
        whenPopup.close();
    }

    // The heading for a section key (`QueryEngine::section_keys`, #25).
    function describeSection(key) {
        if (key === "")
            return "";
        if (key === "evening")
            return "This Evening";
        const colon = key.indexOf(":");
        const kind = key.slice(0, colon);
        const rest = key.slice(colon + 1);
        if (kind === "project" || kind === "area" || kind === "heading") {
            // <id>:<title>; the title may itself contain colons.
            const title = rest.slice(rest.indexOf(":") + 1);
            return title;
        }
        if (kind === "done" && rest === "")
            return "Earlier";
        const parts = rest.split("-");
        const date = new Date(Number(parts[0]), Number(parts[1]) - 1, Number(parts[2]));
        const today = new Date();
        today.setHours(0, 0, 0, 0);
        const days = Math.round((date - today) / 86400000);
        if (kind === "date" && days === 1)
            return "Tomorrow";
        if (kind === "done" && days === 0)
            return "Today";
        if (kind === "done" && days === -1)
            return "Yesterday";
        return Qt.formatDate(date, "dddd, MMMM d");
    }

    // Name something: rename the current project or area (`kind` "project"
    // or "area"), rename heading `id` ("heading"), or add a heading to the
    // current project ("newHeading").
    function openRename(kind, id, title) {
        renameDialog.kind = kind;
        renameDialog.targetId = id || "";
        renameInput.text = title !== undefined ? title : controller.currentTitle;
        renameDialog.open();
    }

    // A start date as shown on a row, or "" where the list already says it:
    // Today and Upcoming (grouped by day) imply the date, and the Logbook is
    // about when tasks were finished. Elsewhere: Today, Tomorrow, a weekday
    // within the coming week, or the date (with the year when it differs).
    function describeStart(iso) {
        const view = controller.currentView;
        if (iso === "" || view === "today" || view === "upcoming" || view === "logbook")
            return "";
        return describeDay(iso);
    }

    // A task's When for its open card: Someday, This Evening, a day, or
    // just "When" when it has none.
    function describeWhenOf(iso, evening, someday) {
        if (someday)
            return "Someday";
        if (evening)
            return "This Evening";
        return iso === "" ? "When" : describeDay(iso);
    }

    // A day for people: Today, Tomorrow, a weekday within the coming week, or
    // the date (with the year when it differs).
    function describeDay(iso) {
        const parts = iso.split("-");
        const date = new Date(Number(parts[0]), Number(parts[1]) - 1, Number(parts[2]));
        const today = new Date();
        today.setHours(0, 0, 0, 0);
        const days = Math.round((date - today) / 86400000);
        if (days === 0)
            return "Today";
        if (days === 1)
            return "Tomorrow";
        if (days > 1 && days < 7)
            return Qt.formatDate(date, "ddd");
        return Qt.formatDate(date, date.getFullYear() === today.getFullYear() ? "MMM d" : "MMM d, yyyy");
    }

    // A deadline as shown on a row: "Due today", "Overdue: Nov 10" or the date.
    function describeDeadline(iso, status) {
        if (iso === "")
            return "";
        if (status === "today")
            return "Due today";
        const parts = iso.split("-");
        const date = Qt.formatDate(new Date(Number(parts[0]), Number(parts[1]) - 1, Number(parts[2])), "MMM d");
        return status === "overdue" ? "Overdue: " + date : date;
    }

    // How a typed When reads back, for the preview under the field.
    function describeWhen(preview) {
        if (preview === "today")
            return "Today";
        if (preview === "evening")
            return "This Evening";
        if (preview === "someday")
            return "Someday";
        const parts = preview.split("-");
        const date = new Date(Number(parts[0]), Number(parts[1]) - 1, Number(parts[2]));
        return Qt.formatDate(date, "dddd, MMMM d, yyyy");
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
            id: sidebar
            objectName: "sidebar"
            Layout.fillHeight: true
            Layout.preferredWidth: root.sidebarShown ? 240 : 0
            Layout.minimumWidth: root.sidebarShown ? 200 : 0
            Layout.maximumWidth: 320
            visible: root.sidebarShown || width > 0
            clip: true
            color: root.colSidebarBg

            Behavior on Layout.preferredWidth {
                enabled: !root.reduceMotion
                NumberAnimation { duration: 180; easing.type: Easing.OutCubic }
            }

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

                        Icon {
                            name: "search"
                            size: 14
                            color: root.colTextSecondary
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
                            { id: "inbox", title: "Inbox", icon: "inbox", count: controller.inboxCount, color: "#339AF0" },
                            { id: "today", title: "Today", icon: "today", count: controller.todayCount, color: "#FCC419" },
                            { id: "upcoming", title: "Upcoming", icon: "upcoming", count: controller.upcomingCount, color: "#FF6B6B" },
                            { id: "anytime", title: "Anytime", icon: "anytime", count: controller.anytimeCount, color: "#20C997" },
                            { id: "someday", title: "Someday", icon: "someday", count: controller.somedayCount, color: "#FAB005" },
                            { id: "logbook", title: "Logbook", icon: "logbook", count: controller.logbookCount, color: "#51CF66" },
                            { id: "trash", title: "Trash", icon: "trash", count: 0, color: "#868E96" }
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

                                Icon {
                                    name: navItem.modelData.icon
                                    color: navItem.modelData.color
                                    size: 16
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
                        objectName: "newContainerButton"
                        icon.width: 16
                        contentItem: Icon {
                            name: "plus"
                            size: 16
                            color: root.colTextSecondary
                        }
                        ToolTip.visible: hovered
                        ToolTip.text: "New Project or Area"
                        onClicked: newContainerMenu.popup()

                        Menu {
                            id: newContainerMenu
                            MenuItem {
                                objectName: "newProjectItem"
                                text: "New Project"
                                onTriggered: newProjectDialog.open()
                            }
                            MenuItem {
                                objectName: "newAreaItem"
                                text: "New Area"
                                onTriggered: newAreaDialog.open()
                            }
                        }
                    }
                }

                // Projects List
                ScrollView {
                    Layout.fillWidth: true
                    Layout.fillHeight: true
                    clip: true

                    ListView {
                        id: projectsList
                        objectName: "projectsList"
                        spacing: 2

                        // Areas, each with its projects (#15), below the projects
                        // that have no area; they scroll with the list.
                        footer: Column {
                            width: projectsList.width
                            spacing: 2
                            topPadding: 8

                            Repeater {
                                model: root.areas

                                delegate: Column {
                                    id: areaBlock
                                    required property var modelData
                                    width: projectsList.width
                                    spacing: 2

                                    Rectangle {
                                        id: areaRow
                                        objectName: "areaItem"
                                        readonly property string areaTitle: areaBlock.modelData.title
                                        readonly property bool isSelected: controller.currentView === "area"
                                            && controller.currentAreaId === areaBlock.modelData.id
                                        width: parent.width
                                        height: 30
                                        radius: 6
                                        color: isSelected ? root.colSidebarActive : (areaMouse.containsMouse ? root.colSidebarHover : "transparent")

                                        RowLayout {
                                            anchors.fill: parent
                                            anchors.leftMargin: 8
                                            anchors.rightMargin: 8
                                            spacing: 8

                                            Icon {
                                                name: "area"
                                                size: 15
                                                color: areaRow.isSelected ? root.colAccent : root.colTextSecondary
                                            }
                                            Text {
                                                text: areaBlock.modelData.title
                                                color: areaRow.isSelected ? root.colAccent : root.colTextPrimary
                                                font.pixelSize: 13
                                                font.weight: Font.DemiBold
                                                elide: Text.ElideRight
                                                Layout.fillWidth: true
                                            }
                                        }

                                        MouseArea {
                                            id: areaMouse
                                            anchors.fill: parent
                                            hoverEnabled: true
                                            cursorShape: Qt.PointingHandCursor
                                            onClicked: controller.selectArea(areaBlock.modelData.id)
                                        }
                                    }

                                    Repeater {
                                        model: root.projects.filter(p => p.area_id === areaBlock.modelData.id)

                                        delegate: Rectangle {
                                            id: areaProject
                                            objectName: "areaProjectItem"
                                            required property var modelData
                                            readonly property string projectTitle: modelData.title
                                            readonly property bool isSelected: controller.currentView === "project"
                                                && controller.currentProjectId === modelData.id
                                            width: areaBlock.width
                                            height: 28
                                            radius: 6
                                            color: isSelected ? root.colSidebarActive : (areaProjectMouse.containsMouse ? root.colSidebarHover : "transparent")

                                            // The same circle as projects without an area,
                                            // indented under the area.
                                            RowLayout {
                                                anchors.fill: parent
                                                anchors.leftMargin: 30
                                                anchors.rightMargin: 8
                                                spacing: 8

                                                Rectangle {
                                                    Layout.preferredWidth: 12
                                                    Layout.preferredHeight: 12
                                                    radius: 6
                                                    color: "transparent"
                                                    border.color: areaProject.isSelected ? root.colAccent : root.colTextSecondary
                                                    border.width: 1.5
                                                }

                                                Text {
                                                    text: areaProject.modelData.title
                                                    color: areaProject.isSelected ? root.colAccent : root.colTextPrimary
                                                    font.pixelSize: 13
                                                    elide: Text.ElideRight
                                                    Layout.fillWidth: true
                                                }
                                            }

                                            MouseArea {
                                                id: areaProjectMouse
                                                anchors.fill: parent
                                                hoverEnabled: true
                                                cursorShape: Qt.PointingHandCursor
                                                onClicked: controller.selectProject(areaProject.modelData.id)
                                            }
                                        }
                                    }
                                }
                            }
                        }

                        // Where a dragged project will land.
                        Rectangle {
                            objectName: "projectDropIndicator"
                            visible: root.dragProjectId !== "" && root.projectDrop !== null
                            z: 10
                            x: 4
                            width: projectsList.width - 8
                            height: 2
                            radius: 1
                            y: root.projectDrop ? root.projectDrop.y - 1 : 0
                            color: root.colAccent
                        }
                        // Projects with no area; area projects are listed under
                        // their area (footer).
                        model: root.projects.filter(p => !p.area_id)

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
                                anchors.leftMargin: 22
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

                            // Drag grip, above the row's click area so it sees the
                            // press first; reorders projects in the sidebar. On the
                            // left: the sidebar's scroll bar covers the right edge.
                            Item {
                                objectName: "projectGrip"
                                anchors.left: parent.left
                                anchors.verticalCenter: parent.verticalCenter
                                width: 20
                                height: parent.height
                                opacity: projMouse.containsMouse || projDrag.active ? 1 : 0

                                Icon {
                                    anchors.centerIn: parent
                                    name: "grip"
                                    size: 14
                                    color: root.colTextSecondary
                                }
                                HoverHandler { cursorShape: Qt.OpenHandCursor }
                                DragHandler {
                                    id: projDrag
                                    target: null
                                    onActiveChanged: {
                                        if (active) {
                                            root.dragProjectId = projItem.modelData.id;
                                            root.projectDrop = null;
                                        } else {
                                            root.endProjectDrag();
                                        }
                                    }
                                    onCentroidChanged: if (active) root.updateProjectDrag(centroid.scenePosition)
                                }
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

                    ToolButton {
                        objectName: "sidebarToggle"
                        focusPolicy: Qt.NoFocus
                        contentItem: Icon {
                            name: "sidebar"
                            size: 18
                            color: root.colTextSecondary
                        }
                        ToolTip.visible: hovered
                        ToolTip.text: (root.sidebarShown ? "Hide" : "Show") + " the sidebar (F9)"
                        onClicked: root.sidebarShown = !root.sidebarShown
                    }

                    // A project's progress, as a pie that fills as its tasks are
                    // done (#28).
                    Canvas {
                        id: progressPie
                        objectName: "progressPie"
                        readonly property real progress: controller.currentProjectProgress
                        visible: controller.currentView === "project"
                        Layout.preferredWidth: 24
                        Layout.preferredHeight: 24
                        Layout.alignment: Qt.AlignVCenter
                        onProgressChanged: requestPaint()
                        onVisibleChanged: requestPaint()

                        onPaint: {
                            const ctx = getContext("2d");
                            ctx.reset();
                            const cx = width / 2;
                            const cy = height / 2;
                            const r = Math.min(width, height) / 2 - 1.5;
                            ctx.lineWidth = 2;
                            ctx.strokeStyle = root.colAccent;
                            ctx.beginPath();
                            ctx.arc(cx, cy, r, 0, 2 * Math.PI);
                            ctx.stroke();
                            if (progress > 0) {
                                ctx.fillStyle = root.colAccent;
                                ctx.beginPath();
                                ctx.moveTo(cx, cy);
                                ctx.arc(cx, cy, r - 3, -Math.PI / 2, -Math.PI / 2 + 2 * Math.PI * progress);
                                ctx.closePath();
                                ctx.fill();
                            }
                        }

                        HoverHandler { id: pieHover }
                        ToolTip.visible: pieHover.hovered
                        ToolTip.text: Math.round(progress * 100) + "% done"
                    }

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
                            && (taskListView.count > 0 || root.trashedProjects.length > 0 || root.trashedAreas.length > 0)
                        onClicked: emptyTrashDialog.open()
                        // Once the Trash is empty this button hides; it must not
                        // keep the keyboard focus the dialog hands back to it.
                        // Depending on the style, the dialog finishes closing
                        // before or after the list empties, so both orders are
                        // handled: here, and in the dialog's onClosed.
                        onVisibleChanged: if (!visible && activeFocus) taskListView.forceActiveFocus()
                    }

                    // Project and area actions, in a menu beside the title.
                    ToolButton {
                        id: containerMenuButton
                        objectName: "containerMenuButton"
                        contentItem: Icon {
                            name: "more"
                            size: 20
                            color: root.colTextSecondary
                        }
                        visible: controller.currentView === "project" || controller.currentView === "area"
                        ToolTip.visible: hovered
                        ToolTip.text: controller.currentView === "area" ? "Area actions" : "Project actions"
                        onClicked: (controller.currentView === "area" ? areaMenu : projectMenu).popup(containerMenuButton, 0, height)

                        Menu {
                            id: projectMenu
                            objectName: "projectMenu"

                            // Which area the project is filed in.
                            Menu {
                                id: projectAreaMenu
                                objectName: "projectAreaMenu"
                                title: "Area"

                                Instantiator {
                                    model: [{ id: "", title: "No Area" }].concat(root.areas)
                                    delegate: MenuItem {
                                        required property var modelData
                                        objectName: "areaChoice-" + (modelData.id === "" ? "none" : modelData.id)
                                        text: modelData.title
                                        checkable: true
                                        checked: {
                                            const project = root.projects.find(p => p.id === controller.currentProjectId);
                                            return (project ? (project.area_id || "") : "") === modelData.id;
                                        }
                                        onTriggered: controller.setProjectArea(controller.currentProjectId, modelData.id)
                                    }
                                    onObjectAdded: (index, object) => projectAreaMenu.insertItem(index, object)
                                    onObjectRemoved: (index, object) => projectAreaMenu.removeItem(object)
                                }
                            }

                            MenuItem {
                                objectName: "renameProjectItem"
                                text: "Rename…"
                                onTriggered: root.openRename("project")
                            }

                            MenuItem {
                                objectName: "addHeadingItem"
                                text: "Add Heading…"
                                onTriggered: root.openRename("newHeading", "", "")
                            }

                            MenuSeparator {}

                            MenuItem {
                                objectName: "trashProjectItem"
                                text: "Move to Trash"
                                onTriggered: {
                                    const id = controller.currentProjectId;
                                    root.showUndo("project", id, controller.currentTitle);
                                    controller.trashProject(id);
                                }
                            }
                        }

                        Menu {
                            id: areaMenu
                            objectName: "areaMenu"

                            MenuItem {
                                objectName: "renameAreaItem"
                                text: "Rename…"
                                onTriggered: root.openRename("area")
                            }

                            MenuSeparator {}

                            MenuItem {
                                objectName: "trashAreaItem"
                                text: "Move to Trash"
                                onTriggered: {
                                    const id = controller.currentAreaId;
                                    root.showUndo("area", id, controller.currentTitle);
                                    controller.trashArea(id);
                                }
                            }
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
                            contentItem: Icon {
                                name: "close"
                                size: 14
                                color: root.isDark ? "#FFA8A8" : "#C92A2A"
                            }
                            ToolTip.visible: hovered
                            ToolTip.text: "Dismiss"
                            visible: controller.errorMessage !== ""
                            onClicked: controller.clearError()
                        }
                    }
                }

                // Quick Task Input Bar (not in the Logbook or the Trash)
                Rectangle {
                    objectName: "quickAddBar"
                    visible: controller.acceptsNewTasks
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

                        Icon {
                            name: "plus"
                            size: 16
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
                                text: "Add a task"
                                color: root.colTextSecondary
                                font.pixelSize: 14
                                visible: !newTaskInput.text && !newTaskInput.activeFocus
                            }

                            Keys.onReturnPressed: {
                                if (text.trim().length > 0) {
                                    root.captureTask(text);
                                    text = "";
                                }
                            }
                            Keys.onEscapePressed: {
                                text = "";
                                taskListView.forceActiveFocus();
                            }
                        }
                    }
                }

                // Trashed areas, each standing for its projects and tasks.
                ColumnLayout {
                    visible: controller.currentView === "trash" && root.trashedAreas.length > 0
                    Layout.fillWidth: true
                    spacing: 4

                    Repeater {
                        model: root.trashedAreas

                        delegate: Rectangle {
                            id: trashedArea
                            objectName: "trashedArea"
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

                                Icon {
                                    name: "area"
                                    size: 16
                                    color: root.colTextSecondary
                                }

                                Text {
                                    text: trashedArea.modelData.title
                                    color: root.colTextPrimary
                                    font.pixelSize: 14
                                    elide: Text.ElideRight
                                    Layout.fillWidth: true
                                }

                                ToolButton {
                                    objectName: "restoreAreaButton"
                                    text: "Put Back"
                                    font.pixelSize: 11
                                    onClicked: controller.restoreArea(trashedArea.modelData.id)
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

                                Icon {
                                    name: "project"
                                    size: 16
                                    color: root.colTextSecondary
                                }

                                Text {
                                    text: trashedProject.modelData.title
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
                    // Rows always fit the width. Some styles (Material) draw a
                    // horizontal bar over the last row's controls otherwise.
                    ScrollBar.horizontal.policy: ScrollBar.AlwaysOff

                    ListView {
                        id: taskListView
                        spacing: 6
                        model: taskModel

                        // Expansion and selection are keyed by task id and kept
                        // here, outside the delegates, so they survive list updates
                        // and scrolling.
                        property string expandedTaskId: ""
                        property string selectedTaskId: ""

                        // Arrow keys are handled below, by task id; ListView's own
                        // current-index navigation is not used.
                        keyNavigationEnabled: false

                        // Motion (#77): new tasks fade in, removed ones fade out,
                        // and the rows around them slide into place; a moved task
                        // glides to its new position. Displacement also finishes
                        // any interrupted fade, so no row is left half drawn.
                        add: Transition {
                            enabled: !root.reduceMotion
                            NumberAnimation { property: "opacity"; from: 0; to: 1; duration: 180; easing.type: Easing.OutCubic }
                        }
                        remove: Transition {
                            enabled: !root.reduceMotion
                            NumberAnimation { property: "opacity"; to: 0; duration: 140; easing.type: Easing.InCubic }
                        }
                        displaced: Transition {
                            enabled: !root.reduceMotion
                            NumberAnimation { properties: "x,y"; duration: 180; easing.type: Easing.OutCubic }
                            NumberAnimation { property: "opacity"; to: 1; duration: 180 }
                        }
                        move: Transition {
                            enabled: !root.reduceMotion
                            NumberAnimation { properties: "x,y"; duration: 200; easing.type: Easing.OutCubic }
                        }

                        // Keep the selected task in view when the list's visible
                        // height changes (the quick-add bar or error banner shows
                        // or hides, the window is resized): a task just scrolled
                        // into view must not end up below the fold.
                        onHeightChanged: {
                            if (selectedTaskId !== "")
                                Qt.callLater(root.scrollToTask, selectedTaskId);
                        }

                        // Sections (#25): This Evening in Today, days in Upcoming,
                        // areas and projects in Anytime, finishing days in the
                        // Logbook. The core gives each row a key
                        // (`QueryEngine::section_keys`); an empty key has no
                        // heading. The section property is the same in every view
                        // (changing it re-lays out the whole list after a view
                        // switch, which undid scrolling a task into view).
                        section.property: "section"
                        section.criteria: ViewSection.FullString
                        section.delegate: Item {
                            id: sectionHeading
                            required property string section
                            readonly property string kind: section.split(":")[0]
                            readonly property bool shown: section !== ""
                            objectName: kind === "evening" ? "eveningHeading" : (shown ? "sectionHeading" : "")
                            readonly property string title: root.describeSection(section)
                            // A project heading's id (#22), from heading:<id>:<title>.
                            readonly property string headingId: kind === "heading" ? section.split(":")[1] : ""
                            width: taskListView.width
                            height: !shown ? 0 : kind === "date" ? 48 : 40
                            visible: shown

                            // Upcoming's days lead with the day of the month.
                            Text {
                                id: headingDay
                                objectName: "headingDay"
                                visible: sectionHeading.kind === "date"
                                text: visible ? String(Number(sectionHeading.section.split(":")[1].split("-")[2])) : ""
                                anchors.left: parent.left
                                anchors.leftMargin: 8
                                anchors.bottom: parent.bottom
                                anchors.bottomMargin: 6
                                font.pixelSize: 24
                                font.weight: Font.Bold
                                color: root.colTextPrimary
                            }

                            // This Evening, a project or an area gets its icon.
                            Icon {
                                id: headingIcon
                                visible: name !== ""
                                name: sectionHeading.kind === "evening" ? "evening"
                                    : sectionHeading.kind === "project" ? "project"
                                    : sectionHeading.kind === "area" ? "area" : ""
                                color: sectionHeading.kind === "evening" ? root.colEvening : root.colTextSecondary
                                size: 16
                                anchors.left: parent.left
                                anchors.leftMargin: 8
                                anchors.verticalCenter: headingText.verticalCenter
                            }

                            Text {
                                id: headingText
                                anchors.left: headingDay.visible ? headingDay.right
                                    : headingIcon.visible ? headingIcon.right : parent.left
                                anchors.leftMargin: 8
                                anchors.bottom: parent.bottom
                                anchors.bottomMargin: 8
                                text: sectionHeading.title
                                font.pixelSize: 15
                                font.weight: Font.DemiBold
                                color: sectionHeading.kind === "heading" ? root.colAccent : root.colTextPrimary
                            }

                            // A project heading's actions.
                            ToolButton {
                                id: headingMenuButton
                                objectName: "headingMenuButton"
                                visible: sectionHeading.kind === "heading"
                                anchors.right: parent.right
                                anchors.verticalCenter: headingText.verticalCenter
                                focusPolicy: Qt.NoFocus
                                Accessible.name: "Heading actions"
                                contentItem: Icon {
                                    name: "more"
                                    size: 16
                                    color: root.colTextSecondary
                                }
                                ToolTip.visible: hovered
                                ToolTip.text: "Heading actions"
                                onClicked: headingMenu.popup(headingMenuButton, 0, height)

                                Menu {
                                    id: headingMenu
                                    objectName: "headingMenu"

                                    MenuItem {
                                        objectName: "renameHeadingItem"
                                        text: "Rename…"
                                        onTriggered: root.openRename("heading", sectionHeading.headingId, sectionHeading.title)
                                    }
                                    MenuItem {
                                        objectName: "moveHeadingUpItem"
                                        text: "Move Up"
                                        onTriggered: controller.moveHeading(sectionHeading.headingId, -1)
                                    }
                                    MenuItem {
                                        objectName: "moveHeadingDownItem"
                                        text: "Move Down"
                                        onTriggered: controller.moveHeading(sectionHeading.headingId, 1)
                                    }
                                    MenuSeparator {}
                                    // Its tasks stay in the project (decided on #22).
                                    MenuItem {
                                        objectName: "deleteHeadingItem"
                                        text: "Delete Heading"
                                        onTriggered: {
                                            root.showUndo("heading", sectionHeading.headingId, sectionHeading.title);
                                            controller.deleteHeading(sectionHeading.headingId);
                                        }
                                    }
                                }
                            }
                            Rectangle {
                                anchors.bottom: parent.bottom
                                width: parent.width
                                height: 1
                                color: root.colBorder
                            }
                        }
                        activeFocusOnTab: true

                        // Keys typed into a row's editors (notes) are handled there
                        // and never reach this handler, except Escape.
                        Keys.onPressed: (event) => {
                            // Ctrl+Up/Down reorders the selected task (lists
                            // arranged by hand only; elsewhere it does nothing).
                            if (event.modifiers === Qt.ControlModifier
                                    && (event.key === Qt.Key_Up || event.key === Qt.Key_Down)) {
                                root.moveSelected(event.key === Qt.Key_Up ? -1 : 1);
                                event.accepted = true;
                                return;
                            }
                            if (event.modifiers & (Qt.ControlModifier | Qt.AltModifier))
                                return;
                            switch (event.key) {
                            case Qt.Key_Down:
                                root.moveSelection(1);
                                break;
                            case Qt.Key_Up:
                                root.moveSelection(-1);
                                break;
                            case Qt.Key_Space:
                                root.toggleSelectedExpansion();
                                break;
                            case Qt.Key_F2:
                                root.editSelectedTitle();
                                break;
                            case Qt.Key_Return:
                            case Qt.Key_Enter:
                                // Below the selection where the list is arranged by
                                // hand; elsewhere into the quick-add field.
                                if (controller.reorderable && controller.acceptsNewTasks
                                        && taskModel.rowOf(taskListView.selectedTaskId) >= 0)
                                    root.startNewBelow();
                                else if (controller.acceptsNewTasks)
                                    newTaskInput.forceActiveFocus();
                                break;
                            case Qt.Key_Delete:
                            case Qt.Key_Backspace:
                                root.trashSelected();
                                break;
                            case Qt.Key_Escape:
                                if (taskListView.expandedTaskId !== "")
                                    taskListView.expandedTaskId = "";
                                else
                                    taskListView.selectedTaskId = "";
                                taskListView.forceActiveFocus();
                                break;
                            default:
                                return;
                            }
                            event.accepted = true;
                        }

                        // Where a dragged task will land.
                        Rectangle {
                            objectName: "dropIndicator"
                            visible: root.dragTaskId !== "" && root.taskDrop !== null
                            z: 10
                            x: 4
                            width: taskListView.width - 8
                            height: 2
                            radius: 1
                            y: root.taskDrop ? root.taskDrop.y - 1 : 0
                            color: root.colAccent
                        }

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
                        // A task, or a heading with no tasks to list (#22): its
                        // section heading shows above the row, which is where a
                        // task moved or dropped into it lands.
                        delegate: DelegateChooser {
                            role: "kind"

                            DelegateChoice {
                                roleValue: "heading"
                                delegate: Item {
                                    objectName: "emptyHeading"
                                    required property string headingId
                                    required property string title
                                    required property int index
                                    readonly property real targetHeight: height
                                    width: taskListView.width
                                    height: 12
                                }
                            }

                            DelegateChoice {
                                roleValue: "task"
                                delegate: FocusScope {
                                    id: taskRow
                                    objectName: "taskRow"
                                    required property string taskId
                                    required property string title
                                    required property string notes
                                    required property bool done
                                    required property string startDate
                                    required property bool evening
                                    required property string deadline
                                    required property string deadlineStatus
                                    required property bool someday
                                    required property int checklistDone
                                    required property int checklistTotal
                                    required property int index

                                    readonly property bool isExpanded: taskListView.expandedTaskId === taskId
                                    readonly property bool isSelected: taskListView.selectedTaskId === taskId

                                    // F2 asked to edit this task's title: focus it once open.
                                    // The new-task field is open under this row (Enter).
                                    readonly property bool newBelowHere: root.newBelowId === taskId
                                    onNewBelowHereChanged: if (newBelowHere) Qt.callLater(taskRow.focusNewBelow)
                                    Component.onCompleted: if (newBelowHere) Qt.callLater(taskRow.focusNewBelow)
                                    function focusNewBelow() {
                                        if (newBelowHere)
                                            newBelowInput.forceActiveFocus();
                                    }

                                    readonly property bool titleEditRequested: root.titleEditId === taskId
                                    onTitleEditRequestedChanged: if (titleEditRequested) Qt.callLater(taskRow.beginTitleEdit)
                                    function beginTitleEdit() {
                                        root.titleEditId = "";
                                        titleInput.forceActiveFocus();
                                        titleInput.selectAll();
                                    }

                                    // The content keeps its natural height, pinned to the top;
                                    // the row grows to reveal it and clips the rest, so the
                                    // header stays put while the height animates.
                                    readonly property real targetHeight: isExpanded || newBelowHere ? rowContent.implicitHeight + 16 : 48
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
                                        // An open task is a card, like a page; a closed one is
                                        // tinted when selected or hovered.
                                        radius: taskRow.isExpanded ? 10 : 8
                                        color: taskRow.isExpanded ? root.colCardBg
                                            : taskRow.isSelected ? (root.isDark ? "#1C3B5A" : "#D0E4F7")
                                            : rowHover.hovered ? (root.isDark ? "#25262B" : "#F8F9FA") : "transparent"
                                        border.color: !taskRow.isExpanded ? "transparent"
                                            : taskRow.isSelected ? (root.isDark ? "#2F5F8F" : "#A5C8EC") : root.colBorder
                                        border.width: 1
                                    }

                                    Behavior on height {
                                        enabled: !root.reduceMotion
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
                                        onClicked: {
                                            taskListView.selectedTaskId = taskRow.taskId;
                                            taskListView.expandedTaskId = taskRow.isExpanded ? "" : taskRow.taskId;
                                            taskListView.forceActiveFocus();
                                        }
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

                                            // Drag grip: reorders the task (lists arranged by
                                            // hand only). Dragging takes over from the row's
                                            // click once the pointer moves. It keeps its space
                                            // in every list, so rows line up the same everywhere.
                                            Item {
                                                objectName: "dragGrip"
                                                enabled: controller.reorderable
                                                Layout.preferredWidth: 12
                                                Layout.preferredHeight: 24
                                                opacity: enabled && (rowHover.hovered || gripDrag.active) ? 1 : 0

                                                Icon {
                                                    anchors.centerIn: parent
                                                    name: "grip"
                                                    size: 16
                                                    color: root.colTextSecondary
                                                }
                                                HoverHandler { cursorShape: Qt.OpenHandCursor }
                                                DragHandler {
                                                    id: gripDrag
                                                    target: null
                                                    onActiveChanged: active ? root.beginTaskDrag(taskRow.taskId) : root.endTaskDrag()
                                                    onCentroidChanged: if (active) root.updateTaskDrag(centroid.scenePosition)
                                                }
                                            }

                                            // Circular checkbox
                                            Rectangle {
                                                id: checkCircle
                                                objectName: "checkCircle"
                                                Layout.preferredWidth: 20
                                                Layout.preferredHeight: 20
                                                radius: 10

                                                // Completing a task: the circle fills with a
                                                // short pop.
                                                Behavior on color {
                                                    enabled: !root.reduceMotion
                                                    ColorAnimation { duration: 150 }
                                                }
                                                SequentialAnimation {
                                                    id: tickPop
                                                    NumberAnimation { target: checkCircle; property: "scale"; to: 1.2; duration: 90; easing.type: Easing.OutCubic }
                                                    NumberAnimation { target: checkCircle; property: "scale"; to: 1; duration: 140; easing.type: Easing.OutBack }
                                                }
                                                Connections {
                                                    target: taskRow
                                                    function onDoneChanged() {
                                                        if (taskRow.done && !root.reduceMotion)
                                                            tickPop.restart();
                                                    }
                                                }
                                                color: taskRow.done ? root.colAccent : "transparent"
                                                border.color: taskRow.done ? root.colAccent : (checkMouse.containsMouse ? root.colAccent : root.colTextSecondary)
                                                border.width: 1.5

                                                Icon {
                                                    anchors.centerIn: parent
                                                    name: "check"
                                                    size: 14
                                                    strokeWidth: 2.6
                                                    color: "#FFFFFF"
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

                                            // Title: plain text while the task is closed, an
                                            // editable field while it is open (#27).
                                            Text {
                                                visible: !taskRow.isExpanded
                                                text: taskRow.title
                                                font.pixelSize: 14
                                                font.strikeout: taskRow.done
                                                color: taskRow.done ? root.colTextSecondary : root.colTextPrimary
                                                Layout.fillWidth: true
                                                elide: Text.ElideRight
                                            }

                                            TextInput {
                                                id: titleInput
                                                objectName: "titleInput"
                                                visible: taskRow.isExpanded
                                                text: taskRow.title
                                                font.pixelSize: 14
                                                font.strikeout: taskRow.done
                                                color: taskRow.done ? root.colTextSecondary : root.colTextPrimary
                                                selectByMouse: true
                                                clip: true
                                                // Only as wide as the text, so a click on the
                                                // empty space beside it still closes the row.
                                                Layout.preferredWidth: Math.max(contentWidth + 8, 60)
                                                Layout.minimumWidth: 40

                                                // Saved when editing ends (Enter, focus leaves).
                                                // A blank title is refused and the old one shown.
                                                onEditingFinished: {
                                                    if (text !== taskRow.title && !controller.renameTask(taskRow.taskId, text))
                                                        text = taskRow.title;
                                                }
                                                onActiveFocusChanged: if (activeFocus) taskListView.selectedTaskId = taskRow.taskId
                                                // Enter goes on to the notes.
                                                Keys.onReturnPressed: notesInput.forceActiveFocus()
                                                Keys.onEnterPressed: notesInput.forceActiveFocus()
                                                // Escape abandons the edit; the row stays open.
                                                Keys.onEscapePressed: {
                                                    text = taskRow.title;
                                                    taskListView.forceActiveFocus();
                                                }

                                                // Show changes made elsewhere, but never while
                                                // the user is typing.
                                                Connections {
                                                    target: taskRow
                                                    function onTitleChanged() {
                                                        if (!titleInput.activeFocus)
                                                            titleInput.text = taskRow.title;
                                                    }
                                                }
                                            }

                                            // Fills the space beside the title field.
                                            Item {
                                                visible: taskRow.isExpanded
                                                Layout.fillWidth: true
                                            }

                                            // "This Evening" badge; Today has a This Evening
                                            // heading instead.
                                            Rectangle {
                                                objectName: "eveningBadge"
                                                visible: taskRow.evening && controller.currentView !== "today"
                                                Layout.preferredHeight: 20
                                                Layout.preferredWidth: eveningLabel.contentWidth + 28
                                                radius: 4
                                                color: root.isDark ? "#382D16" : "#FFF3BF"

                                                Icon {
                                                    id: eveningIcon
                                                    name: "evening"
                                                    size: 12
                                                    color: root.colEvening
                                                    anchors.left: parent.left
                                                    anchors.leftMargin: 6
                                                    anchors.verticalCenter: parent.verticalCenter
                                                }

                                                Text {
                                                    id: eveningLabel
                                                    anchors.left: eveningIcon.right
                                                    anchors.leftMargin: 4
                                                    anchors.verticalCenter: parent.verticalCenter
                                                    text: "Evening"
                                                    font.pixelSize: 11
                                                    font.weight: Font.DemiBold
                                                    color: root.colEvening
                                                }
                                            }

                                            // Start date badge, where the list doesn't already
                                            // say it (root.describeStart).
                                            Rectangle {
                                                objectName: "startBadge"
                                                visible: dateLabel.text !== ""
                                                Layout.preferredHeight: 20
                                                Layout.preferredWidth: dateLabel.contentWidth + 12
                                                radius: 4
                                                color: root.isDark ? "#2C2E33" : "#E9ECEF"

                                                Text {
                                                    id: dateLabel
                                                    objectName: "startLabel"
                                                    anchors.centerIn: parent
                                                    text: root.describeStart(taskRow.startDate)
                                                    font.pixelSize: 11
                                                    color: root.colTextSecondary
                                                }
                                            }

                                            // Deadline badge: red when due today or overdue.
                                            Rectangle {
                                                objectName: "deadlineBadge"
                                                readonly property bool urgent: taskRow.deadlineStatus === "today"
                                                    || taskRow.deadlineStatus === "overdue"
                                                visible: taskRow.deadline !== ""
                                                Layout.preferredHeight: 20
                                                Layout.preferredWidth: deadlineLabel.contentWidth + 28
                                                radius: 4
                                                color: urgent ? (root.isDark ? "#3B1F1F" : "#FFE3E3") : (root.isDark ? "#2C2E33" : "#E9ECEF")

                                                Icon {
                                                    id: deadlineIcon
                                                    name: "deadline"
                                                    size: 12
                                                    color: deadlineLabel.color
                                                    anchors.left: parent.left
                                                    anchors.leftMargin: 6
                                                    anchors.verticalCenter: parent.verticalCenter
                                                }

                                                Text {
                                                    id: deadlineLabel
                                                    objectName: "deadlineLabel"
                                                    anchors.left: deadlineIcon.right
                                                    anchors.leftMargin: 4
                                                    anchors.verticalCenter: parent.verticalCenter
                                                    text: root.describeDeadline(taskRow.deadline, taskRow.deadlineStatus)
                                                    font.pixelSize: 11
                                                    font.weight: parent.urgent ? Font.DemiBold : Font.Normal
                                                    color: parent.urgent ? (root.isDark ? "#FFA8A8" : "#C92A2A") : root.colTextSecondary
                                                }
                                            }

                                            // Checklist progress, while the row is closed.
                                            Rectangle {
                                                objectName: "checklistBadge"
                                                visible: taskRow.checklistTotal > 0 && !taskRow.isExpanded
                                                Layout.preferredHeight: 20
                                                Layout.preferredWidth: checklistLabel.contentWidth + 28
                                                radius: 4
                                                color: root.isDark ? "#2C2E33" : "#E9ECEF"
                                                Accessible.role: Accessible.StaticText
                                                Accessible.name: "Checklist: " + taskRow.checklistDone + " of " + taskRow.checklistTotal + " done"

                                                Icon {
                                                    id: checklistIcon
                                                    name: "checklist"
                                                    size: 12
                                                    color: root.colTextSecondary
                                                    anchors.left: parent.left
                                                    anchors.leftMargin: 6
                                                    anchors.verticalCenter: parent.verticalCenter
                                                }

                                                Text {
                                                    id: checklistLabel
                                                    objectName: "checklistProgress"
                                                    anchors.left: checklistIcon.right
                                                    anchors.leftMargin: 4
                                                    anchors.verticalCenter: parent.verticalCenter
                                                    text: taskRow.checklistDone + "/" + taskRow.checklistTotal
                                                    font.pixelSize: 11
                                                    color: root.colTextSecondary
                                                }
                                            }

                                            // Move to Trash (shows on hover). Nothing is
                                            // destroyed until the Trash is emptied.
                                            ToolButton {
                                                objectName: "deleteButton"
                                                focusPolicy: Qt.NoFocus
                                                contentItem: Icon {
                                                    name: "close"
                                                    size: 14
                                                    color: root.colTextSecondary
                                                }
                                                visible: rowHover.hovered && controller.currentView !== "trash"
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
                                                focusPolicy: Qt.NoFocus
                                                text: "Put Back"
                                                visible: controller.currentView === "trash"
                                                font.pixelSize: 11
                                                onClicked: controller.restoreTask(taskRow.taskId)
                                            }
                                        }

                                        // New task below this one (Enter on the selection).
                                        Rectangle {
                                            visible: taskRow.newBelowHere
                                            Layout.fillWidth: true
                                            Layout.preferredHeight: 34
                                            Layout.leftMargin: 56 // in line with the title
                                            radius: 6
                                            color: root.isDark ? "#1A1B1E" : "#FFFFFF"
                                            border.color: root.colAccent

                                            TextInput {
                                                id: newBelowInput
                                                objectName: "newBelowInput"
                                                anchors.fill: parent
                                                anchors.leftMargin: 8
                                                anchors.rightMargin: 8
                                                verticalAlignment: TextInput.AlignVCenter
                                                font.pixelSize: 14
                                                color: root.colTextPrimary
                                                clip: true

                                                Text {
                                                    anchors.fill: parent
                                                    verticalAlignment: Text.AlignVCenter
                                                    text: "New to-do"
                                                    color: root.colTextSecondary
                                                    font.pixelSize: 14
                                                    visible: !newBelowInput.text
                                                }

                                                Keys.onReturnPressed: root.commitNewBelow(taskRow.taskId, newBelowInput)
                                                Keys.onEnterPressed: root.commitNewBelow(taskRow.taskId, newBelowInput)
                                                Keys.onEscapePressed: {
                                                    text = "";
                                                    root.closeNewBelow();
                                                }
                                                // Clicking elsewhere keeps what was typed.
                                                onActiveFocusChanged: {
                                                    if (activeFocus || root.newBelowId !== taskRow.taskId)
                                                        return;
                                                    const title = text;
                                                    text = "";
                                                    root.newBelowId = "";
                                                    if (title.trim() !== "")
                                                        controller.addTaskBelow(title, taskRow.taskId);
                                                }
                                            }
                                        }

                                        // In-Place Expanded Detail: Notes & Quick Scheduler
                                        ColumnLayout {
                                            id: expandedContent
                                            visible: taskRow.isExpanded
                                            Layout.fillWidth: true
                                            // In line with the title: past the grip (12),
                                            // the checkbox (20) and their spacing (2 × 12).
                                            Layout.leftMargin: 56
                                            spacing: 10

                                            // Notes Editor. Saves after a pause in typing and
                                            // when editing ends, not on every keystroke.
                                            TextArea {
                                                id: notesInput
                                                objectName: "notesInput"
                                                // Editing a task's notes selects that task.
                                                onActiveFocusChanged: if (activeFocus) taskListView.selectedTaskId = taskRow.taskId
                                                Layout.fillWidth: true
                                                // Grows with what is written.
                                                Layout.preferredHeight: Math.max(44, contentHeight + topPadding + bottomPadding)
                                                placeholderText: "Notes"
                                                font.pixelSize: 13
                                                color: root.colTextPrimary
                                                wrapMode: TextArea.Wrap
                                                leftPadding: 0
                                                background: null

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

                                            // The checklist (#21), made only while the row is
                                            // open, so closed rows hold no model.
                                            Loader {
                                                id: checklistLoader
                                                objectName: "checklistLoader"
                                                active: taskRow.isExpanded
                                                readonly property ChecklistEditor editor: item as ChecklistEditor
                                                visible: editor !== null && editor.shown
                                                Layout.fillWidth: true
                                                Layout.preferredHeight: visible ? editor.implicitHeight : 0
                                                sourceComponent: ChecklistEditor {
                                                    taskId: taskRow.taskId
                                                    controller: controller
                                                    textPrimary: root.colTextPrimary
                                                    textSecondary: root.colTextSecondary
                                                    accent: root.colAccent
                                                    border: root.colBorder
                                                    hover: root.colSidebarHover
                                                    // Escape: back to the list; the row stays open.
                                                    onLeave: taskListView.forceActiveFocus()
                                                }
                                            }

                                            // When and Deadline, tucked into the corner: pills
                                            // that show the current value, with a chevron to
                                            // say a click changes it (in the popover).
                                            // The label is the buttons' own property, not
                                            // `text`: some styles (KDE's) paint `text` in
                                            // the background, beside our contentItem.
                                            RowLayout {
                                                Layout.fillWidth: true
                                                spacing: 4

                                                Item { Layout.fillWidth: true }

                                                // Adds an item at the end of the checklist.
                                                ToolButton {
                                                    id: checklistButton
                                                    objectName: "checklistButton"
                                                    focusPolicy: Qt.NoFocus
                                                    Accessible.name: "Add a checklist item"
                                                    leftPadding: 8
                                                    rightPadding: 8
                                                    topPadding: 4
                                                    bottomPadding: 4
                                                    background: Rectangle {
                                                        implicitHeight: 26
                                                        implicitWidth: 26
                                                        radius: height / 2
                                                        color: checklistButton.down ? root.colSidebarActive
                                                            : checklistButton.hovered ? root.colSidebarHover : "transparent"
                                                        border.width: 1
                                                        border.color: root.colBorder
                                                    }
                                                    contentItem: Icon {
                                                        name: "checklist"
                                                        size: 14
                                                        color: root.colTextSecondary
                                                    }
                                                    ToolTip.visible: hovered
                                                    ToolTip.text: "Add a checklist item"
                                                    onClicked: if (checklistLoader.editor) checklistLoader.editor.startAdding("")
                                                }

                                                ToolButton {
                                                    id: whenControl
                                                    objectName: "schedule-date"
                                                    focusPolicy: Qt.NoFocus
                                                    font.pixelSize: 12
                                                    property string label: root.describeWhenOf(taskRow.startDate, taskRow.evening, taskRow.someday)
                                                    readonly property bool hasValue: taskRow.startDate !== "" || taskRow.evening || taskRow.someday
                                                    Accessible.name: label
                                                    Accessible.role: Accessible.ButtonMenu
                                                    leftPadding: 10
                                                    rightPadding: 8
                                                    topPadding: 4
                                                    bottomPadding: 4
                                                    background: Rectangle {
                                                        implicitHeight: 26
                                                        radius: height / 2
                                                        color: whenControl.down ? root.colSidebarActive
                                                            : whenControl.hovered ? root.colSidebarHover : "transparent"
                                                        border.width: 1
                                                        border.color: root.colBorder
                                                    }
                                                    contentItem: RowLayout {
                                                        spacing: 6
                                                        Icon {
                                                            name: "calendar"
                                                            size: 14
                                                            color: root.colTextSecondary
                                                        }
                                                        Text {
                                                            objectName: "pillLabel"
                                                            text: whenControl.label
                                                            font: whenControl.font
                                                            color: whenControl.hasValue ? root.colTextPrimary : root.colTextSecondary
                                                        }
                                                        Icon {
                                                            objectName: "pillChevron"
                                                            name: "chevron-down"
                                                            size: 12
                                                            color: root.colTextSecondary
                                                        }
                                                    }
                                                    ToolTip.visible: hovered
                                                    ToolTip.text: "Change when (Ctrl+S)"
                                                    onClicked: root.openWhenFor(taskRow.taskId)
                                                }

                                                ToolButton {
                                                    id: deadlineControl
                                                    objectName: "schedule-deadline"
                                                    focusPolicy: Qt.NoFocus
                                                    font.pixelSize: 12
                                                    property string label: taskRow.deadline === "" ? "Deadline"
                                                        : root.describeDeadline(taskRow.deadline, taskRow.deadlineStatus)
                                                    readonly property bool hasValue: taskRow.deadline !== ""
                                                    Accessible.name: label
                                                    Accessible.role: Accessible.ButtonMenu
                                                    leftPadding: 10
                                                    rightPadding: 8
                                                    topPadding: 4
                                                    bottomPadding: 4
                                                    background: Rectangle {
                                                        implicitHeight: 26
                                                        radius: height / 2
                                                        color: deadlineControl.down ? root.colSidebarActive
                                                            : deadlineControl.hovered ? root.colSidebarHover : "transparent"
                                                        border.width: 1
                                                        border.color: root.colBorder
                                                    }
                                                    contentItem: RowLayout {
                                                        spacing: 6
                                                        Icon {
                                                            name: "deadline"
                                                            size: 14
                                                            color: root.colTextSecondary
                                                        }
                                                        Text {
                                                            objectName: "pillLabel"
                                                            text: deadlineControl.label
                                                            font: deadlineControl.font
                                                            color: deadlineControl.hasValue ? root.colTextPrimary : root.colTextSecondary
                                                        }
                                                        Icon {
                                                            objectName: "pillChevron"
                                                            name: "chevron-down"
                                                            size: 12
                                                            color: root.colTextSecondary
                                                        }
                                                    }
                                                    ToolTip.visible: hovered
                                                    ToolTip.text: "Change the deadline (Ctrl+D)"
                                                    onClicked: root.openDeadlineFor(taskRow.taskId)
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
                text: (root.undoKind === "project" ? "Project" : root.undoKind === "area" ? "Area" : "Task")
                    + " “" + root.undoTitle + "” moved to Trash"
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

    // Open Quick Find result `index`: an area or project opens its view; a
    // task opens the view that holds it, with the task open and in view.
    function openSearchResult(index) {
        const results = searchResultsList.model;
        if (index < 0 || index >= results.length)
            return;
        const result = results[index];
        if (result.kind === "area") {
            controller.selectArea(result.id);
        } else if (result.kind === "project") {
            controller.selectProject(result.id);
        } else {
            root.pendingRevealId = result.id;
            taskListView.expandedTaskId = result.id;
            controller.revealTask(result.id);
        }
        quickFindDialog.close();
    }
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
        // The last search is kept, selected, so typing starts a new one.
        onOpened: {
            searchInput.forceActiveFocus();
            searchInput.selectAll();
        }
        onClosed: taskListView.forceActiveFocus()

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
                onTextChanged: {
                    controller.search(text);
                    searchResultsList.currentIndex = 0;
                }
                // The results are chosen from the field: Up and Down move the
                // highlight, Enter opens it.
                Keys.onUpPressed: searchResultsList.decrementCurrentIndex()
                Keys.onDownPressed: searchResultsList.incrementCurrentIndex()
                Keys.onReturnPressed: root.openSearchResult(searchResultsList.currentIndex)
                Keys.onEnterPressed: root.openSearchResult(searchResultsList.currentIndex)
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
                    objectName: "searchResult"
                    required property var modelData
                    required property int index
                    readonly property string kind: modelData.kind
                    width: searchResultsList.width
                    height: 36
                    radius: 6
                    color: resultItem.ListView.isCurrentItem ? root.colSidebarActive
                        : resMouse.containsMouse ? root.colSidebarHover : "transparent"
                    Accessible.role: Accessible.ListItem
                    Accessible.name: (kind === "area" ? "Area: " : kind === "project" ? "Project: " : "") + modelData.title

                    RowLayout {
                        anchors.fill: parent
                        anchors.leftMargin: 10
                        anchors.rightMargin: 10
                        spacing: 8

                        // An area or project shows its sidebar icon; a task,
                        // an empty checkbox like its row.
                        Item {
                            Layout.preferredWidth: 16
                            Layout.preferredHeight: 16
                            Icon {
                                anchors.centerIn: parent
                                visible: resultItem.kind !== "task"
                                name: resultItem.kind === "area" ? "area" : "project"
                                size: 16
                                color: resultItem.kind === "area" ? root.colTextSecondary : root.colAccent
                            }
                            Rectangle {
                                anchors.centerIn: parent
                                visible: resultItem.kind === "task"
                                width: 12
                                height: 12
                                radius: 6
                                color: "transparent"
                                border.width: 1.5
                                border.color: root.colTextSecondary
                            }
                        }

                        Text {
                            text: resultItem.modelData.title
                            font.pixelSize: 13
                            color: root.colTextPrimary
                            Layout.fillWidth: true
                            elide: Text.ElideRight
                        }

                        Text {
                            objectName: "searchResultKind"
                            visible: resultItem.kind !== "task"
                            text: resultItem.kind === "area" ? "Area" : "Project"
                            font.pixelSize: 11
                            color: root.colTextSecondary
                        }
                    }

                    MouseArea {
                        id: resMouse
                        anchors.fill: parent
                        hoverEnabled: true
                        cursorShape: Qt.PointingHandCursor
                        onClicked: root.openSearchResult(resultItem.index)
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
        id: newAreaDialog
        objectName: "newAreaDialog"
        title: "Create New Area"
        standardButtons: Dialog.Ok | Dialog.Cancel
        x: (parent.width - width) / 2
        y: (parent.height - height) / 2
        width: 360

        TextField {
            id: areaTitleInput
            objectName: "areaTitleInput"
            width: parent.width
            placeholderText: "Area name, e.g. Home or Work"
            focus: true
        }

        onAccepted: {
            controller.createArea(areaTitleInput.text);
            areaTitleInput.text = "";
        }
    }

    Dialog {
        id: renameDialog
        objectName: "renameDialog"
        // What is being named (`openRename`), and the heading's id.
        property string kind: "area"
        property string targetId: ""
        title: kind === "project" ? "Rename Project"
            : kind === "heading" ? "Rename Heading"
            : kind === "newHeading" ? "New Heading" : "Rename Area"
        standardButtons: Dialog.Ok | Dialog.Cancel
        x: (parent.width - width) / 2
        y: (parent.height - height) / 2
        width: 360

        TextField {
            id: renameInput
            objectName: "renameInput"
            width: parent.width
            focus: true
        }

        // A blank name is refused and the old one kept.
        onAccepted: {
            if (kind === "project")
                controller.renameProject(controller.currentProjectId, renameInput.text);
            else if (kind === "heading")
                controller.renameHeading(targetId, renameInput.text);
            else if (kind === "newHeading")
                controller.addHeading(renameInput.text);
            else
                controller.renameArea(controller.currentAreaId, renameInput.text);
        }
    }

    Dialog {
        id: newProjectDialog
        objectName: "newProjectDialog"
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
    Shortcut {
        sequence: "F9"
        onActivated: root.sidebarShown = !root.sidebarShown
    }
    Shortcut {
        sequence: "Ctrl+N"
        enabled: controller.acceptsNewTasks
        onActivated: newTaskInput.forceActiveFocus()
    }
    Shortcut {
        sequence: "Ctrl+Shift+N"
        onActivated: newProjectDialog.open()
    }
    Shortcut {
        sequence: "Ctrl+S"
        enabled: taskListView.selectedTaskId !== ""
        onActivated: root.openWhenMenu()
    }
    Shortcut {
        sequence: "Ctrl+D"
        enabled: taskListView.selectedTaskId !== ""
        onActivated: root.openDeadlineFor(taskListView.selectedTaskId)
    }
    // Text editors keep Ctrl+Z for their own undo.
    Shortcut {
        sequence: StandardKey.Undo
        enabled: root.undoKind !== ""
        onActivated: root.undo()
    }

    // When popover for a task (Ctrl+S, Date…): quick choices, a typed date
    // with a preview of what it means, and a calendar (#59).
    Popup {
        id: whenPopup
        objectName: "whenPopup"
        property string taskId: ""
        // "when" sets the task's When; "deadline" its deadline.
        property string mode: "when"
        // The day the calendar's keyboard cursor is on.
        property date cursor: new Date()

        x: (parent.width - width) / 2
        y: 120
        // As wide as its contents need, at least 320: the quick buttons keep
        // their natural width, which depends on the style and the language,
        // and a narrower popover would push its contents past its edge.
        width: Math.min(parent.width - 32, Math.max(320, implicitContentWidth + leftPadding + rightPadding))
        modal: true
        dim: false
        focus: true
        closePolicy: Popup.CloseOnEscape | Popup.CloseOnPressOutside

        background: Rectangle {
            color: root.isDark ? "#202124" : "#FFFFFF"
            radius: 10
            border.color: root.colBorder
        }

        onOpened: {
            whenInput.text = "";
            const today = new Date();
            today.setHours(0, 0, 0, 0);
            cursor = today;
            calendar.month = today.getMonth();
            calendar.year = today.getFullYear();
            whenInput.forceActiveFocus();
        }
        onClosed: taskListView.forceActiveFocus()

        function moveCursor(days) {
            const next = new Date(cursor);
            next.setDate(next.getDate() + days);
            const today = new Date();
            today.setHours(0, 0, 0, 0);
            if (next < today)
                return;
            cursor = next;
            calendar.month = next.getMonth();
            calendar.year = next.getFullYear();
        }

        function showMonth(delta) {
            const first = new Date(calendar.year, calendar.month + delta, 1);
            calendar.month = first.getMonth();
            calendar.year = first.getFullYear();
        }

        contentItem: ColumnLayout {
            spacing: 8

            Label {
                objectName: "whenTitle"
                text: whenPopup.mode === "deadline" ? "Deadline" : "When"
                font.pixelSize: 15
                font.weight: Font.DemiBold
                color: root.colTextPrimary
            }

            TextField {
                id: whenInput
                objectName: "whenInput"
                Layout.fillWidth: true
                placeholderText: whenPopup.mode === "deadline"
                    ? "Due: tomorrow, fri, dec 24"
                    : "Type a date: tomorrow, fri, dec 24"
                // Enter applies what was typed; a refused text keeps the
                // popover open.
                onAccepted: {
                    const applied = whenPopup.mode === "deadline"
                        ? controller.setTaskDeadlineTyped(whenPopup.taskId, text, root.monthFirst)
                        : controller.scheduleTaskTyped(whenPopup.taskId, text, root.monthFirst);
                    if (applied)
                        whenPopup.close();
                }
                // Down moves into the calendar.
                Keys.onDownPressed: calendar.forceActiveFocus()
            }

            Label {
                id: whenPreview
                objectName: "whenPreview"
                readonly property string meaning: whenPopup.mode === "deadline"
                    ? controller.previewDeadline(whenInput.text, root.monthFirst)
                    : controller.previewWhen(whenInput.text, root.monthFirst)
                Layout.fillWidth: true
                visible: whenInput.text.trim() !== ""
                text: meaning === "" ? "Not a date" : root.describeWhen(meaning)
                color: meaning === "" ? root.colTextSecondary : root.colAccent
                font.pixelSize: 12
            }

            RowLayout {
                spacing: 4
                // One set of buttons for both modes (rebuilding them on each
                // mode switch left new, not yet laid out buttons); a deadline
                // has no This Evening or Someday.
                Repeater {
                    model: [
                        { label: "Today", when: "today" },
                        { label: "This Evening", when: "evening" },
                        { label: "Someday", when: "someday" },
                        { label: "Clear", when: "none" }
                    ]
                    delegate: Button {
                        required property var modelData
                        visible: whenPopup.mode === "when" || modelData.when === "today" || modelData.when === "none"
                        objectName: (whenPopup.mode === "deadline" ? "deadline-" : "when-") + modelData.when
                        text: modelData.label
                        font.pixelSize: 11
                        onClicked: {
                            if (whenPopup.mode === "deadline") {
                                const today = Qt.formatDate(new Date(), "yyyy-MM-dd");
                                controller.setTaskDeadline(whenPopup.taskId, modelData.when === "today" ? today : "");
                            } else {
                                controller.scheduleTask(whenPopup.taskId, modelData.when);
                            }
                            whenPopup.close();
                        }
                    }
                }
            }

            RowLayout {
                Layout.fillWidth: true
                // The calendar's labels take Matters' colours, not the
                // style's palette, which follows the desktop and can differ
                // from Matters' own light or dark scheme.
                ToolButton {
                    objectName: "whenPrevMonth"
                    Accessible.name: "Previous month"
                    contentItem: Icon {
                        name: "chevron-left"
                        size: 16
                        color: root.colTextSecondary
                    }
                    onClicked: whenPopup.showMonth(-1)
                }
                Label {
                    objectName: "whenMonth"
                    Layout.fillWidth: true
                    horizontalAlignment: Text.AlignHCenter
                    text: Qt.formatDate(new Date(calendar.year, calendar.month, 1), "MMMM yyyy")
                    font.weight: Font.DemiBold
                    color: root.colTextPrimary
                }
                ToolButton {
                    objectName: "whenNextMonth"
                    Accessible.name: "Next month"
                    contentItem: Icon {
                        name: "chevron-right"
                        size: 16
                        color: root.colTextSecondary
                    }
                    onClicked: whenPopup.showMonth(1)
                }
            }

            DayOfWeekRow {
                Layout.fillWidth: true
                locale: calendar.locale
                delegate: Text {
                    required property string shortName
                    text: shortName
                    horizontalAlignment: Text.AlignHCenter
                    verticalAlignment: Text.AlignVCenter
                    font.pixelSize: 12
                    font.weight: Font.DemiBold
                    color: root.colTextSecondary
                }
            }

            MonthGrid {
                id: calendar
                objectName: "whenCalendar"
                Layout.fillWidth: true
                // Six week rows. MonthGrid takes no height from its cells, so
                // without this the calendar was laid out with none at all.
                Layout.preferredHeight: 6 * 32
                locale: Qt.locale()
                activeFocusOnTab: true

                delegate: Rectangle {
                    id: day
                    required property var model
                    readonly property date date: model.date
                    readonly property bool past: {
                        const today = new Date();
                        today.setHours(0, 0, 0, 0);
                        return date < today;
                    }
                    readonly property bool isCursor: calendar.activeFocus
                        && date.getFullYear() === whenPopup.cursor.getFullYear()
                        && date.getMonth() === whenPopup.cursor.getMonth()
                        && date.getDate() === whenPopup.cursor.getDate()
                    objectName: "day-" + Qt.formatDate(date, "yyyy-MM-dd")
                    implicitHeight: 30
                    radius: 15
                    color: isCursor ? root.colAccent : (model.today ? root.colSidebarActive : "transparent")
                    opacity: model.month === calendar.month ? 1 : 0.35

                    Text {
                        anchors.centerIn: parent
                        text: day.model.day
                        font.pixelSize: 12
                        color: day.isCursor ? "#FFFFFF" : (day.past ? root.colTextSecondary : root.colTextPrimary)
                    }
                }

                onClicked: (date) => root.applyWhenDate(date)

                // Arrows move the cursor (days, weeks), Enter picks it, Up from
                // the first week returns to the text field.
                Keys.onPressed: (event) => {
                    switch (event.key) {
                    case Qt.Key_Left:
                        whenPopup.moveCursor(-1);
                        break;
                    case Qt.Key_Right:
                        whenPopup.moveCursor(1);
                        break;
                    case Qt.Key_Up:
                        whenPopup.moveCursor(-7);
                        break;
                    case Qt.Key_Down:
                        whenPopup.moveCursor(7);
                        break;
                    case Qt.Key_Return:
                    case Qt.Key_Enter:
                        root.applyWhenDate(whenPopup.cursor);
                        break;
                    default:
                        return;
                    }
                    event.accepted = true;
                }
            }
        }
    }
}
