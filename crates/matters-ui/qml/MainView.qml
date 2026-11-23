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
    // The window reaches the controller for activation and idle exit (#203).
    property alias controller: controller

    // The open list's name: a fixed view's in the user's language, or the
    // project's or area's own.
    readonly property string title: {
        const fixed = fixedViews.find(v => v.id === controller.currentView);
        return fixed ? fixed.title : controller.currentTitle;
    }
    // Exposed for hosts and tests.
    readonly property alias appController: controller
    // Right to left in a right-to-left language (#75): rows, the sidebar
    // and layouts mirror; Qt reverses text itself.
    LayoutMirroring.enabled: Qt.application.layoutDirection === Qt.RightToLeft
    LayoutMirroring.childrenInherit: true
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

    // Whether finished tasks wait in their lists to be logged (#67): Log
    // Completed is offered then.
    readonly property bool logsByHand: controller.logPolicy === "daily" || controller.logPolicy === "manually"

    // ---- Task actions (#65) ------------------------------------------------

    // Cancel the open task `id`, or reopen it if it is finished.
    // With several selected (#64), every selected task: cancelled if task
    // `id` is open, reopened if it is finished.
    function cancelOrReopen(id: string) {
        const row = taskModel.rowOf(id);
        if (row < 0)
            return;
        const ids = targetsOf(id);
        if (taskModel.statusAt(row) === "open")
            actOn(ids.filter(t => taskModel.statusAt(taskModel.rowOf(t)) === "open"), t => controller.cancelTask(t), n => qsTr("%n tasks canceled", "", n));
        else
            actOn(ids.filter(t => taskModel.statusAt(taskModel.rowOf(t)) !== "open"), t => controller.toggleTask(t), n => qsTr("%n tasks reopened", "", n));
    }

    // Copy task `id` below itself and select the copy; with several
    // selected, copy each (#64).
    function duplicateTask(id: string) {
        let copy = "";
        for (const original of targetsOf(id))
            copy = controller.duplicateTask(original) || copy;
        if (copy !== "") {
            pendingRevealId = copy;
            Qt.callLater(root.reloadTasks);
        }
    }

    // The delegate showing task `id`, if it has one.
    function rowItem(id: string): Item {
        return taskListView.itemAtIndex(taskModel.rowOf(id));
    }

    // Open the menu for task `id`, at `pos` in `item` (a click), or below
    // the selected row (the keyboard).
    function openTaskMenu(id: string, pos: point, item: Item) {
        if (id === "" || controller.currentView === "trash")
            return;
        taskListView.selectedTaskId = id;
        taskMenu.taskId = id;
        taskMenu.taskCount = targetsOf(id).length;
        taskMenu.open_ = taskModel.statusAt(taskModel.rowOf(id)) === "open";
        taskMenu.parent = item;
        taskMenu.popup(item, pos);
    }

    Menu {
        id: taskMenu
        objectName: "taskMenu"
        property string taskId: ""
        // Whether the task is open (not done or canceled).
        property bool open_: true
        // How many tasks the menu acts on: the selection it is in (#64).
        property int taskCount: 1
        onClosed: taskListView.forceActiveFocus()

        MenuItem {
            objectName: "menuCancel"
            text: taskMenu.taskCount > 1
                ? (taskMenu.open_ ? qsTr("Cancel %n Tasks", "", taskMenu.taskCount) : qsTr("Reopen %n Tasks", "", taskMenu.taskCount))
                : (taskMenu.open_ ? qsTr("Cancel Task") : qsTr("Reopen Task"))
            onTriggered: root.cancelOrReopen(taskMenu.taskId)
        }
        MenuItem {
            objectName: "menuDuplicate"
            text: qsTr("Duplicate")
            onTriggered: root.duplicateTask(taskMenu.taskId)
        }
        MenuItem {
            objectName: "menuConvert"
            text: qsTr("Convert to Project")
            // One task becomes one project.
            enabled: taskMenu.open_ && taskMenu.taskCount === 1
            onTriggered: controller.convertTaskToProject(taskMenu.taskId)
        }
        MenuSeparator {}
        MenuItem {
            objectName: "menuTrash"
            text: qsTr("Move to Trash")
            onTriggered: root.trashTasks(root.targetsOf(taskMenu.taskId))
        }
    }

    // Opens a URL with the desktop's handler. Tests replace it, so nothing
    // is launched.
    property var openExternally: url => Qt.openUrlExternally(url)

    // Open a link from a task's notes (#63): web and mail links at once,
    // anything else (a file, another program's scheme, no scheme at all)
    // only once the user agrees.
    function openLink(url: string) {
        const scheme = (/^([a-z][a-z0-9+.-]*):/i.exec(url) || ["", ""])[1].toLowerCase();
        if (["http", "https", "mailto"].indexOf(scheme) >= 0)
            openExternally(url);
        else
            openLinkDialog.ask(url);
    }

    // Where in Markdown `source` its formatted text `shown` (a prefix of
    // what the formatted view shows) ends: each shown character is matched
    // to the next equal one in the source, skipping the Markdown syntax
    // between them; one the source hasn't is skipped. Line breaks match
    // any whitespace.
    function sourcePosition(source: string, shown: string): int {
        let at = 0;
        for (let i = 0; i < shown.length; ++i) {
            const c = shown[i];
            const blank = /\s/.test(c) || c === "\u2029" || c === "\u2028";
            let j = at;
            while (j < source.length && !(blank ? /\s/.test(source[j]) : source[j] === c))
                ++j;
            // Not in the source (a list's bullet, say): skip it.
            if (j < source.length)
                at = j + 1;
        }
        return at;
    }

    // The fixed views, in sidebar order; Quick Find finds them by name too.
    readonly property var fixedViews: [
        { id: "inbox", title: qsTr("Inbox"), icon: "inbox", count: controller.inboxCount, color: root.viewColors.inbox },
        { id: "today", title: qsTr("Today"), icon: "today", count: controller.todayCount, color: root.viewColors.today },
        { id: "upcoming", title: qsTr("Upcoming"), icon: "upcoming", count: controller.upcomingCount, color: root.viewColors.upcoming },
        { id: "anytime", title: qsTr("Anytime"), icon: "anytime", count: controller.anytimeCount, color: root.viewColors.anytime },
        { id: "someday", title: qsTr("Someday"), icon: "someday", count: controller.somedayCount, color: root.viewColors.someday },
        { id: "logbook", title: qsTr("Logbook"), icon: "logbook", count: controller.logbookCount, color: root.viewColors.logbook },
        { id: "trash", title: qsTr("Trash"), icon: "trash", count: 0, color: root.viewColors.trash }
    ]

    // Every tag, parents before children, with its depth (#57).
    readonly property var allTags: {
        try {
            return JSON.parse(controller.tagsJson);
        } catch (e) {
            return [];
        }
    }

    // The tags of the open project or area.
    readonly property var currentTags: {
        try {
            return JSON.parse(controller.currentTagsJson);
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
        } else if (undoKind === "checklist") {
            controller.restoreChecklistItem();
        } else if (undoKind === "batch") {
            controller.restoreRememberedTasks();
        }
        hideUndo();
    }

    // Theme color palette (respects light/dark system settings)
    // "system" follows the desktop's light or dark scheme; "light" and "dark"
    // choose one (Preferences, #72). A desktop without a theme ignores an
    // app's request to switch scheme, so the choice is applied here.
    readonly property string appearance: controller.colorScheme

    // The sidebar can be hidden (F9 or the header's toggle) to give the list
    // the whole window.
    property bool sidebarShown: true
    readonly property bool isDark: appearance === "dark"
        || (appearance === "system" && Application.styleHints.colorScheme === Qt.ColorScheme.Dark)
    // The palette (#74). Every colour Matters draws is one of these, in
    // light and in dark. Text colours meet WCAG 2.2 AA (4.5:1) on every
    // surface they are drawn on, and the accent, which also marks focus and
    // ticked boxes, 3:1 on all of them; tst_contrast.qml checks each pair.
    // A colour literal anywhere else in the QML fails qml_lint.
    //
    // The desktop's accent (#76), when it has one and Preferences follow
    // it, takes the place of Matters' blue, made as dark (in light mode) or
    // as light (in dark mode) as those limits need; the selection is tinted
    // with it. When the desktop asks for higher contrast, text and the
    // accent are held to 7:1 (WCAG's AAA) and lines to 3:1. Without either,
    // the colours below are used as they are.
    readonly property string desktopAccent: controller.followAccent ? controller.systemAccent : ""
    readonly property bool highContrast: controller.highContrast
    readonly property real textContrast: highContrast ? 7 : 4.5
    // Matters' own colours, which the desktop's choices adjust.
    readonly property color ownTextPrimary: isDark ? "#F8F9FA" : "#1A1B1E"
    readonly property color ownTextSecondary: isDark ? "#A0A2A5" : "#5E656C"
    readonly property color ownAccent: isDark ? "#4DABF7" : "#1864AB"
    readonly property color ownDanger: isDark ? "#FFA8A8" : "#C02828"
    readonly property color ownEvening: isDark ? "#FAB005" : "#845D03"
    readonly property color ownBorder: isDark ? "#2C2E33" : "#E9ECEF"

    function luminance(c) {
        c = Qt.color(c);
        const linear = v => v <= 0.04045 ? v / 12.92 : Math.pow((v + 0.055) / 1.055, 2.4);
        return 0.2126 * linear(c.r) + 0.7152 * linear(c.g) + 0.0722 * linear(c.b);
    }

    function contrast(a, b) {
        const la = luminance(a), lb = luminance(b);
        return (Math.max(la, lb) + 0.05) / (Math.min(la, lb) + 0.05);
    }

    // `c` itself if it has `need`:1 against each of `on`, or else the
    // nearest colour of its hue and saturation that has: darker in light
    // mode, lighter in dark mode.
    function readable(c, on, need) {
        c = Qt.color(c);
        // A little over, so rounding to the colour's channels keeps it.
        const enough = x => on.every(s => contrast(x, s) >= need + 0.02);
        if (enough(c))
            return c;
        const hue = Math.max(c.hslHue, 0), saturation = c.hslSaturation;
        for (let l = c.hslLightness; l >= 0 && l <= 1; l += isDark ? 0.005 : -0.005) {
            const x = Qt.hsla(hue, saturation, l, 1);
            if (enough(x))
                return x;
        }
        return isDark ? Qt.rgba(1, 1, 1, 1) : Qt.rgba(0, 0, 0, 1);
    }

    // `over` with `tint` laid on it at `amount` (0 to 1).
    function tinted(over, tint, amount) {
        const a = Qt.color(over), b = Qt.color(tint);
        return Qt.rgba(a.r + (b.r - a.r) * amount, a.g + (b.g - a.g) * amount, a.b + (b.b - a.b) * amount, 1);
    }

    // `over` tinted with `tint` up to `amount`, but no further than
    // `texts` stay readable on it (4.5:1).
    function readableTint(over, tint, amount, texts) {
        for (let a = amount; a > 0; a -= 0.01) {
            const x = tinted(over, tint, a);
            if (texts.every(c => contrast(c, x) >= 4.52))
                return x;
        }
        return Qt.color(over);
    }

    // Surfaces.
    readonly property color colBg: isDark ? "#1A1B1E" : "#FFFFFF"
    readonly property color colSidebarBg: isDark ? "#141517" : "#F7F8FA"
    readonly property color colSidebarHover: isDark ? "#25262B" : "#EBECEF"
    readonly property color colSidebarActive: isDark ? "#2C2E33" : "#E2E4E8"
    readonly property color colCardBg: isDark ? "#202124" : "#FFFFFF"
    readonly property color colRowHover: isDark ? "#25262B" : "#F8F9FA"
    readonly property color colRowSelected: desktopAccent === "" ? (isDark ? "#1C3B5A" : "#D0E4F7")
        : readableTint(colBg, desktopAccent, isDark ? 0.25 : 0.16,
            [ownTextPrimary, ownTextSecondary, ownDanger, ownEvening])
    readonly property color colChip: isDark ? "#2C2E33" : "#E9ECEF"
    readonly property color colBadge: isDark ? "#2C2E33" : "#DEE2E6"
    readonly property color colField: isDark ? "#25262B" : "#FFFFFF"
    readonly property color colControl: isDark ? "#2C2E33" : "#F1F3F5"
    readonly property color colInputBar: isDark ? "#25262B" : "#F1F3F5"
    readonly property color colDangerBg: isDark ? "#3B1F1F" : "#FFE3E3"
    readonly property color colEveningBg: isDark ? "#382D16" : "#FFF3BF"
    readonly property color colTooltipBg: isDark ? "#373A40" : "#25262B"
    readonly property color colInverseBg: isDark ? "#E9ECEF" : "#212529"
    // Text, and what is drawn like it.
    readonly property color colTextPrimary: ownTextPrimary
    // What text is drawn on, for the limits above.
    readonly property var textSurfaces: [colBg, colSidebarBg, colSidebarHover, colSidebarActive, colCardBg,
        colRowHover, colRowSelected, colChip, colBadge, colField, colControl, colInputBar, colDangerBg, colEveningBg]
    readonly property color colTextSecondary: !highContrast ? ownTextSecondary
        : readable(ownTextSecondary, textSurfaces, 7)
    // The accent as text (selected entries, headings, previews) and as a
    // ring, tick or mark on every surface.
    readonly property color colAccent: {
        if (desktopAccent === "" && !highContrast)
            return ownAccent;
        const asText = readable(desktopAccent !== "" ? desktopAccent : ownAccent,
            [colBg, colSidebarBg, colSidebarHover, colSidebarActive, colCardBg, colField, colControl, colInputBar, colChip],
            textContrast);
        return readable(asText, textSurfaces, highContrast ? 4.5 : 3);
    }
    // On a fill of the accent: a tick, a selected count. Whichever of white
    // and near-black reads better on it.
    readonly property color colOnAccent: contrast("#FFFFFF", colAccent) >= contrast("#101113", colAccent) ? "#FFFFFF" : "#101113"
    readonly property color colDanger: !highContrast ? ownDanger : readable(ownDanger, textSurfaces, 7)
    // Warm gold for evening.
    readonly property color colEvening: !highContrast ? ownEvening
        : readable(ownEvening, [colBg, colCardBg, colEveningBg, colRowHover, colRowSelected], 7)
    readonly property color colTooltipText: "#F8F9FA"
    readonly property color colInverseText: isDark ? "#212529" : "#F8F9FA"
    // Lines that only separate or frame, which nothing needs to read;
    // with higher contrast, lines that can be seen (3:1).
    readonly property color colBorder: !highContrast ? ownBorder
        : readable(ownBorder, [colBg, colCardBg, colSidebarBg], 3)
    readonly property color colRowSelectedBorder: desktopAccent === "" && !highContrast ? (isDark ? "#2F5F8F" : "#A5C8EC")
        : highContrast ? colAccent : tinted(colBg, desktopAccent, isDark ? 0.45 : 0.4)
    // Each list's own colour, for its icon beside its name.
    readonly property var viewColors: ({
        inbox: "#339AF0", today: "#FCC419", upcoming: "#FF6B6B", anytime: "#20C997",
        someday: "#FAB005", logbook: "#51CF66", trash: "#868E96"
    })

    // Controls (buttons, fields, menus, tooltips) take their colours from
    // this palette, built from the colours above, rather than from the
    // desktop's, which can disagree with Matters' light or dark scheme.
    palette {
        window: colCardBg
        windowText: colTextPrimary
        base: colField
        alternateBase: colSidebarBg
        text: colTextPrimary
        placeholderText: colTextSecondary
        button: colControl
        buttonText: colTextPrimary
        brightText: colOnAccent
        highlight: colAccent
        highlightedText: colOnAccent
        accent: colAccent
        link: colAccent
        light: isDark ? "#373A40" : "#FFFFFF"
        midlight: isDark ? "#303237" : "#E9ECEF"
        mid: isDark ? "#3A3C42" : "#CED4DA"
        dark: isDark ? "#101113" : "#ADB5BD"
        shadow: "#000000"
        toolTipBase: colTooltipBg
        toolTipText: colTooltipText
    }
    // End of the palette.
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

    // Reminders (#60): checked against the wall clock every few seconds, so
    // one comes on time, and within seconds of waking from sleep or a change
    // of clock or time zone. At start, those missed while Matters was closed.
    Timer {
        interval: 5 * 1000
        repeat: true
        running: true
        triggeredOnStart: true
        onTriggered: controller.checkReminders()
    }

    Connections {
        target: controller
        function onReminderOpened(taskId) {
            root.openFromReminder(taskId);
        }
    }

    // Task to scroll to once the list has loaded (set by Quick Find).
    property string pendingRevealId: ""

    // The tag the list is narrowed to (#58), or "" for the whole list.
    // Navigating clears it; choosing one reloads the list.
    property string tagFilter: ""
    onTagFilterChanged: Qt.callLater(root.reloadTasks)
    // The tag the next visit starts narrowed to (a tag chosen in Quick
    // Find), instead of the whole list.
    property string nextTagFilter: ""

    // The tags the current list can be narrowed to, and the chosen one even
    // if no task in the list has it, so it can be seen and unchosen.
    readonly property var presentTags: {
        let present = [];
        try {
            present = JSON.parse(taskModel.presentTagsJson);
        } catch (e) {}
        if (tagFilter !== "" && !present.some(t => t.id === tagFilter))
            present = present.concat(allTags.filter(t => t.id === tagFilter).map(t => ({ id: t.id, name: t.name, depth: 0 })));
        return present;
    }

    function reloadTasks() {
        // A new visit (navigation) starts with nothing selected, and the
        // whole list: the tag filter clears (#58), unless Quick Find chose
        // the tag the visit is for.
        if (controller.visit !== loadedVisit) {
            loadedVisit = controller.visit;
            taskListView.selectedTaskId = "";
            newBelowId = "";
            tagFilter = nextTagFilter;
            nextTagFilter = "";
        }
        // A project or area view loads the tasks of that project or area.
        const container = controller.currentView === "area" ? controller.currentAreaId : controller.currentProjectId;
        taskModel.load(controller.currentView, container, controller.visit, tagFilter);
        // The selection survives the reload, less what left the list (#64).
        const kept = taskListView.selectedIds.filter(id => taskModel.rowOf(id) >= 0);
        if (kept.length !== taskListView.selectedIds.length)
            taskListView.selectedIds = kept;
        if (movedTaskId !== "") {
            if (taskModel.rowOf(movedTaskId) < 0 && taskListView.selectedTaskId === movedTaskId)
                taskListView.selectedTaskId = movedNextId;
            movedTaskId = "";
        }
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

    // ---- Selecting several tasks (#64) -----------------------------------

    // The selected tasks still listed, in list order.
    function selection(): var {
        return taskListView.selectedIds
            .filter(id => taskModel.rowOf(id) >= 0)
            .sort((a, b) => taskModel.rowOf(a) - taskModel.rowOf(b));
    }

    // The tasks an action on task `id` applies to: the selection, when `id`
    // is in it; otherwise `id` alone.
    function targetsOf(id: string): var {
        const selected = selection();
        return selected.indexOf(id) >= 0 ? selected : [id];
    }

    // Select task `id` alone, with the cursor on it.
    function selectOnly(id: string) {
        taskListView.selectedIds = id === "" ? [] : [id];
        taskListView.selectionAnchor = id;
        taskListView.selectedTaskId = id;
    }

    // Select the tasks from the anchor to task `id`, with the cursor on it
    // (Shift+click, Shift+Up/Down).
    function extendSelectionTo(id: string) {
        const anchor = taskModel.rowOf(taskListView.selectionAnchor);
        const to = taskModel.rowOf(id);
        if (to < 0)
            return;
        if (anchor < 0) {
            selectOnly(id);
            return;
        }
        const ids = [];
        for (let row = Math.min(anchor, to); row <= Math.max(anchor, to); ++row) {
            const rowId = taskModel.idAt(row);
            if (rowId !== "")
                ids.push(rowId);
        }
        taskListView.selectedIds = ids;
        taskListView.selectedTaskId = id;
    }

    // Shift+Up/Down: move the cursor `step` rows, extending the selection.
    function extendSelection(step: int) {
        const current = taskModel.rowOf(taskListView.selectedTaskId);
        if (current < 0) {
            moveSelection(step);
            return;
        }
        let row = current + step;
        while (row >= 0 && row < taskListView.count && taskModel.idAt(row) === "")
            row += step;
        if (row < 0 || row >= taskListView.count)
            return;
        extendSelectionTo(taskModel.idAt(row));
        taskListView.positionViewAtIndex(row, ListView.Contain);
    }

    // Ctrl+click: add task `id` to the selection, or take it out.
    function toggleInSelection(id: string) {
        const ids = selection();
        const at = ids.indexOf(id);
        if (at >= 0) {
            ids.splice(at, 1);
            taskListView.selectedIds = ids;
            if (taskListView.selectedTaskId === id)
                taskListView.selectedTaskId = ids.length > 0 ? ids[ids.length - 1] : "";
        } else {
            taskListView.selectedIds = ids.concat([id]);
            taskListView.selectedTaskId = id;
        }
        taskListView.selectionAnchor = id;
    }

    // Run `act` on each of `ids`. For several, they are remembered first,
    // so one Undo puts them all back, and the notice says `done(n)` for how
    // many.
    function actOn(ids: var, act: var, done: var) {
        const several = ids.length > 1;
        if (several)
            controller.rememberTasks(JSON.stringify(ids));
        for (const id of ids)
            act(id);
        if (several)
            showUndo("batch", "", done(ids.length));
    }

    function selectRow(row) {
        const id = taskModel.idAt(row);
        if (id === "")
            return;
        selectOnly(id);
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

    // The task whose tag field should take keyboard focus once it is open
    // (Ctrl+Shift+T, #57).
    property string tagEditId: ""

    function editSelectedTags() {
        const id = taskListView.selectedTaskId;
        if (taskModel.rowOf(id) < 0)
            return;
        const ids = targetsOf(id);
        if (ids.length > 1) {
            batchTagPopup.taskIds = ids;
            batchTagPopup.open();
            return;
        }
        taskListView.expandedTaskId = id;
        tagEditId = id;
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
        // A selected task drags the whole selection with it (#64).
        taskListView.selectedTaskId = id;
    }

    function updateTaskDrag(scenePos) {
        taskDragPos = scenePos;
        const from = taskModel.rowOf(dragTaskId);
        if (from < 0) {
            taskDrop = null;
            return;
        }
        // Over the sidebar, the task goes to the entry under the pointer
        // (#54) instead of a place in the list.
        sidebarDrop = sidebarTargetAt(scenePos, ["inbox", "today", "someday", "area", "project"]);
        if (sidebarDrop) {
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
        const target = sidebarDrop;
        dragTaskId = "";
        taskDrop = null;
        sidebarDrop = null;
        const ids = id === "" ? [] : targetsOf(id);
        if (target && id !== "") {
            sendTasks(ids, target.kind, target.id);
        } else if (drop && id !== "") {
            // The dragged tasks land in their order, one after another.
            let after = drop.afterId;
            actOn(ids.filter(moved => moved !== after), moved => {
                if (controller.currentView === "today")
                    controller.placeTodayTask(moved, drop.section, after);
                else if (controller.currentView === "project")
                    controller.placeProjectTask(moved, drop.section, after);
                else
                    controller.placeTask(moved, after);
                after = moved;
            }, n => qsTr("%n tasks moved", "", n));
            Qt.callLater(root.scrollToTask, id);
        }
    }

    property string dragProjectId: ""
    property var projectDrop: null

    property point projectDragPos

    function beginProjectDrag(id) {
        dragProjectId = id;
        projectDrop = null;
    }

    // A project is dragged anywhere in the sidebar (#164): among the projects
    // with no area, within its area, or to a place in another area; dropped
    // on an area's own row, it goes last in that area (#54).
    function updateProjectDrag(scenePos) {
        projectDragPos = scenePos;
        sidebarDrop = sidebarTargetAt(scenePos, ["area"]);
        projectDrop = sidebarDrop ? null
            : sidebarSlot(sidebarRows(["projectItem", "areaProjectItem"]), dragProjectId, scenePos);
    }

    function endProjectDrag() {
        const id = dragProjectId;
        const drop = projectDrop;
        const target = sidebarDrop;
        dragProjectId = "";
        projectDrop = null;
        sidebarDrop = null;
        if (target && id !== "")
            controller.setProjectArea(id, target.id);
        else if (drop && id !== "")
            controller.placeProjectInArea(id, drop.group, drop.afterId);
    }

    // Areas are dragged among themselves (#164), by the grip on their row;
    // the drop line goes between whole areas, projects included.
    property string dragAreaId: ""
    property var areaDrop: null

    function updateAreaDrag(scenePos) {
        projectDragPos = scenePos;
        areaDrop = sidebarSlot(sidebarRows(["areaBlock"]), dragAreaId, scenePos);
    }

    function endAreaDrag() {
        const id = dragAreaId;
        const drop = areaDrop;
        dragAreaId = "";
        areaDrop = null;
        if (drop && id !== "")
            controller.placeArea(id, drop.afterId);
    }

    // The sidebar's visible rows named `names`, top to bottom.
    function sidebarRows(names) {
        const rows = [];
        const walk = item => {
            for (let i = 0; i < item.children.length; ++i) {
                const child = item.children[i];
                if (!child.visible)
                    continue;
                if (names.indexOf(child.objectName) >= 0)
                    rows.push(child);
                walk(child);
            }
        };
        walk(projectsList.contentItem);
        return rows.sort((a, b) => a.mapToItem(null, 0, 0).y - b.mapToItem(null, 0, 0).y);
    }

    // A sidebar row's id and group: its area's id ("" for none, and for an
    // area itself). Rows are plain Items to the type checker.
    function sidebarRowInfo(row: var): var {
        if (row.objectName === "areaBlock")
            return { id: row.modelData.id, group: "" };
        return { id: row.modelData.id, group: row.modelData.area_id || "" };
    }

    // Where a row dragged over `rows` would land: in the group of the row
    // under the pointer, after the row above it in that group (or first),
    // with `y` the line's place in the projects list. Null when the pointer
    // is over no row, or over the dragged row itself.
    function sidebarSlot(rows, draggedId, scenePos) {
        for (let i = 0; i < rows.length; ++i) {
            const row = rows[i];
            const p = row.mapFromItem(null, scenePos.x, scenePos.y);
            if (p.x < 0 || p.x > row.width || p.y < 0 || p.y >= row.height)
                continue;
            const info = sidebarRowInfo(row);
            if (info.id === draggedId)
                return null;
            const before = p.y < row.height / 2;
            let afterId = before ? "" : info.id;
            if (before) {
                for (let j = i - 1; j >= 0; --j) {
                    const above = sidebarRowInfo(rows[j]);
                    if (above.group !== info.group)
                        break;
                    if (above.id !== draggedId) {
                        afterId = above.id;
                        break;
                    }
                }
            }
            const y = row.mapToItem(projectsList.contentItem, 0, before ? 0 : row.height).y;
            return { group: info.group, afterId: afterId, y: y };
        }
        return null;
    }

    // ---- Move (Ctrl+Shift+M, dragging onto the sidebar; #54) -----------

    // The sidebar entry under a dragged task or project, as { kind, id }.
    // Entries say what they are with `dropKind` and `dropId`; only `kinds`
    // are wanted.
    property var sidebarDrop: null

    function sidebarTargetAt(scenePos, kinds) {
        if (!root.sidebarShown)
            return null;
        return dropTargetUnder(sidebar, scenePos, kinds);
    }

    // The outermost wanted entry under the pointer within `item`. Every
    // visible child under the point is searched, topmost first: `childAt`
    // stops at the first child there, which in a ListView can be one of its
    // own empty items rather than the delegate.
    function dropTargetUnder(item: Item, scenePos: point, kinds: var): var {
        if (!item.visible || !item.contains(item.mapFromItem(null, scenePos.x, scenePos.y)))
            return null;
        const target = dropTargetOf(item);
        if (target && kinds.indexOf(target.kind) >= 0)
            return target;
        for (let i = item.children.length - 1; i >= 0; --i) {
            const found = dropTargetUnder(item.children[i], scenePos, kinds);
            if (found)
                return found;
        }
        return null;
    }

    // What a sidebar entry accepts, or null for any other item. Entries are
    // plain Items to the type checker, so this reads them untyped.
    function dropTargetOf(item: var): var {
        return typeof item.dropKind === "string" && item.dropKind !== ""
            ? { kind: item.dropKind, id: item.dropId } : null;
    }

    // A task sent elsewhere, and the row to select if it leaves the list,
    // so repeated moves work through the list as Delete does.
    property string movedTaskId: ""
    property string movedNextId: ""

    // Send a task to `kind` (inbox, area, project or heading, with `id`), or
    // schedule it for Today or Someday.
    function sendTask(id, kind, targetId) {
        const row = taskModel.rowOf(id);
        movedTaskId = id;
        movedNextId = row < 0 ? "" : taskModel.idAt(row + 1) || taskModel.idAt(row - 1);
        if (kind === "today" || kind === "someday")
            controller.scheduleTask(id, kind);
        else
            controller.moveTaskTo(id, kind, targetId);
    }

    // Send tasks `ids` as `sendTask` does; one Undo brings them all back.
    function sendTasks(ids: var, kind: string, targetId: string) {
        const scheduled = kind === "today" || kind === "someday";
        actOn(ids, id => sendTask(id, kind, targetId), n => scheduled ? qsTr("%n tasks scheduled", "", n) : qsTr("%n tasks moved", "", n));
    }

    function openMoveFor(id) {
        if (taskModel.rowOf(id) < 0)
            return;
        movePopup.taskIds = targetsOf(id);
        taskListView.selectedTaskId = id;
        scrollToTask(id);
        movePopup.taskId = id;
        movePopup.open();
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
            // Not while the task is over the sidebar.
            if (p.x < 0 || p.x > taskListView.width)
                return;
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

    // Scroll the sidebar's projects and areas while a dragged task or
    // project is held near their top or bottom edge, so every entry can be
    // reached (#54).
    Timer {
        interval: 30
        repeat: true
        running: root.dragTaskId !== "" || root.dragProjectId !== "" || root.dragAreaId !== ""
        onTriggered: {
            const pos = root.dragTaskId !== "" ? root.taskDragPos : root.projectDragPos;
            const p = projectsList.mapFromItem(null, pos.x, pos.y);
            if (p.x < 0 || p.x > projectsList.width || p.y < 0 || p.y > projectsList.height)
                return;
            const edge = 30;
            const top = projectsList.originY;
            const bottom = Math.max(top, top + projectsList.contentHeight - projectsList.height);
            let y = projectsList.contentY;
            if (p.y < edge)
                y = Math.max(top, y - 10);
            else if (p.y > projectsList.height - edge)
                y = Math.min(bottom, y + 10);
            if (y === projectsList.contentY)
                return;
            projectsList.contentY = y;
            if (root.dragTaskId !== "")
                root.updateTaskDrag(pos);
            else if (root.dragAreaId !== "")
                root.updateAreaDrag(pos);
            else
                root.updateProjectDrag(pos);
        }
    }

    // Complete or cancel the open project (#55). With tasks still open, a
    // dialog asks what happens to them: nothing is closed silently.
    function askFinishProject(outcome) {
        const id = controller.currentProjectId;
        if (controller.currentProjectOpenTasks === 0) {
            controller.finishProject(id, outcome, outcome);
            return;
        }
        finishProjectDialog.outcome = outcome;
        finishProjectDialog.open();
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
        trashTasks(targetsOf(taskListView.selectedTaskId));
    }

    // Move tasks `ids` to the Trash and select the next task left, so
    // repeated presses work through the list. One Undo brings them back.
    function trashTasks(ids: var) {
        if (ids.length === 0 || taskModel.rowOf(ids[0]) < 0)
            return;
        const last = taskModel.rowOf(ids[ids.length - 1]);
        let next = "";
        for (let row = last + 1; row < taskListView.count && next === ""; ++row) {
            const id = taskModel.idAt(row);
            if (id !== "" && ids.indexOf(id) < 0)
                next = id;
        }
        for (let row = taskModel.rowOf(ids[0]) - 1; row >= 0 && next === ""; --row) {
            const id = taskModel.idAt(row);
            if (id !== "" && ids.indexOf(id) < 0)
                next = id;
        }
        if (ids.length === 1)
            showUndo("task", ids[0], taskModel.titleAt(taskModel.rowOf(ids[0])));
        selectOnly(next);
        actOn(ids, id => controller.trashTask(id), n => qsTr("%n tasks moved to Trash", "", n));
    }

    // Ctrl+.: complete the selected tasks, or reopen them all when none is
    // open (#64).
    function completeSelected() {
        const ids = selection();
        if (ids.length === 0)
            return;
        const open = ids.filter(id => taskModel.statusAt(taskModel.rowOf(id)) === "open");
        if (open.length > 0)
            actOn(open, id => controller.toggleTask(id), n => qsTr("%n tasks completed", "", n));
        else
            actOn(ids, id => controller.toggleTask(id), n => qsTr("%n tasks reopened", "", n));
    }

    // ---- When popover (Ctrl+S, the Date… button; #59) ------------------

    // Numeric dates typed by the user follow the locale: 12/24 or 24/12.
    readonly property bool monthFirst: {
        const format = Qt.locale().dateFormat(Locale.ShortFormat);
        return format.indexOf("M") < format.indexOf("d");
    }

    // A reminder time (`HH:MM`, #60) in the locale's short format.
    function describeTime(hhmm) {
        if (hhmm === "")
            return "";
        const [h, m] = hhmm.split(":").map(Number);
        return new Date(2000, 0, 1, h, m).toLocaleTimeString(Qt.locale(), Locale.ShortFormat);
    }

    // Open was chosen on a reminder (#60): the task, open and in view, in
    // the list that holds it; Today for a reminder of several.
    function openFromReminder(id) {
        if (id === "") {
            controller.selectView("today");
        } else {
            pendingRevealId = id;
            taskListView.expandedTaskId = id;
            controller.revealTask(id);
        }
        const window = root.Window.window;
        if (window)
            window.requestActivate();
    }

    // The popover sets a task's When or, in "deadline" mode, its deadline
    // (Ctrl+D, Deadline…; #20).
    function openDatePopup(id, mode) {
        const row = taskModel.rowOf(id);
        if (row < 0)
            return;
        whenPopup.taskIds = targetsOf(id);
        taskListView.selectedTaskId = id;
        scrollToTask(id);
        whenPopup.taskId = id;
        whenPopup.forProject = false;
        whenPopup.mode = mode;
        whenPopup.reminder = taskModel.reminderAt(row);
        whenPopup.repeat = taskModel.repeatAt(row);
        whenPopup.open();
    }

    // The same popover for the open project's When or deadline (#56).
    function openProjectDatePopup(mode) {
        whenPopup.taskId = controller.currentProjectId;
        whenPopup.forProject = true;
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
        whenPopup.apply(Qt.formatDate(date, "yyyy-MM-dd"));
        whenPopup.close();
    }

    // A local date-time from `YYYY-MM-DDTHH:MM:SS`.
    function parseLocal(iso) {
        const [d, t] = iso.split("T");
        const [y, mo, day] = d.split("-").map(Number);
        const [h, mi] = (t || "0:0").split(":").map(Number);
        return new Date(y, mo - 1, day, h, mi);
    }

    // When a calendar event happens on the day it is listed under (#62):
    // "All day" for an all-day event, or one that started earlier and goes
    // on past the day; its times, in the locale's short format; "Until" the
    // end for one that started on an earlier day; "From" the start for one
    // that goes on past the day.
    function describeEventTime(day, start, end, allDay) {
        if (allDay)
            return qsTr("All day");
        const time = d => d.toLocaleTimeString(Qt.locale(), Locale.ShortFormat);
        const from = parseLocal(start);
        const to = parseLocal(end);
        const dayStart = parseLocal(day);
        const nextDay = new Date(dayStart.getFullYear(), dayStart.getMonth(), dayStart.getDate() + 1);
        const startsBefore = from < dayStart;
        const endsAfter = to > nextDay;
        if (startsBefore && (endsAfter || to.getTime() === nextDay.getTime()))
            return qsTr("All day");
        if (startsBefore)
            return qsTr("Until %1").arg(time(to));
        if (endsAfter)
            return qsTr("From %1").arg(time(from));
        if (to <= from)
            return time(from);
        return qsTr("%1 – %2", "a time range").arg(time(from)).arg(time(to));
    }

    // The user's calendars (#62): [{id, name, color}], and the ids of those
    // not shown.
    readonly property var calendars: {
        try {
            return JSON.parse(controller.calendarsJson);
        } catch (e) {
            return [];
        }
    }
    readonly property var hiddenCalendars: {
        try {
            return JSON.parse(controller.hiddenCalendarsJson);
        } catch (e) {
            return [];
        }
    }

    // The heading for a section key (`QueryEngine::section_keys`, #25).
    function describeSection(key) {
        if (key === "")
            return "";
        if (key === "evening")
            return qsTr("This Evening");
        const colon = key.indexOf(":");
        const kind = key.slice(0, colon);
        const rest = key.slice(colon + 1);
        if (kind === "project" || kind === "area" || kind === "heading") {
            // <id>:<title>; the title may itself contain colons.
            const title = rest.slice(rest.indexOf(":") + 1);
            return title;
        }
        if (kind === "done" && rest === "")
            return qsTr("Earlier");
        const parts = rest.split("-");
        const date = new Date(Number(parts[0]), Number(parts[1]) - 1, Number(parts[2]));
        const today = new Date();
        today.setHours(0, 0, 0, 0);
        const days = Math.round((date - today) / 86400000);
        if (kind === "date" && days === 1)
            return qsTr("Tomorrow");
        if (kind === "done" && days === 0)
            return qsTr("Today");
        if (kind === "done" && days === -1)
            return qsTr("Yesterday");
        return localDate(date, "weekday-day-month");
    }

    // `date` as the locale writes it (locale_format.rs): "day-month",
    // "day-month-year", "weekday-day-month", "weekday-day-month-year",
    // "month-year" or "weekday" (its short name).
    function localDate(date, style) {
        return controller.formatDate(Qt.formatDate(date, "yyyy-MM-dd"), style);
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
            return qsTr("Someday");
        if (evening)
            return qsTr("This Evening");
        return iso === "" ? qsTr("When") : describeDay(iso);
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
            return qsTr("Today");
        if (days === 1)
            return qsTr("Tomorrow");
        if (days > 1 && days < 7)
            return localDate(date, "weekday");
        return localDate(date, date.getFullYear() === today.getFullYear() ? "day-month" : "day-month-year");
    }

    // A deadline as shown on a row: "Due today", "Overdue: Nov 10" or the date.
    function describeDeadline(iso, status) {
        if (iso === "")
            return "";
        if (status === "today")
            return qsTr("Due today");
        const parts = iso.split("-");
        const date = localDate(new Date(Number(parts[0]), Number(parts[1]) - 1, Number(parts[2])), "day-month");
        return status === "overdue" ? qsTr("Overdue: %1").arg(date) : date;
    }

    // How a typed When reads back, for the preview under the field.
    function describeWhen(preview) {
        if (preview === "today")
            return qsTr("Today");
        if (preview === "evening")
            return qsTr("This Evening");
        if (preview === "someday")
            return qsTr("Someday");
        const parts = preview.split("-");
        const date = new Date(Number(parts[0]), Number(parts[1]) - 1, Number(parts[2]));
        return localDate(date, "weekday-day-month-year");
    }

    Component.onCompleted: {
        // Calendar events (#62); after a read that changes them, the lists
        // reload.
        controller.refreshCalendars();
        reloadTasks();
    }

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
                    objectName: "quickFindButton"
                    activeFocusOnTab: true
                    Accessible.role: Accessible.Button
                    Accessible.name: qsTr("Quick Find")
                    Accessible.description: qsTr("Ctrl+K")
                    Accessible.onPressAction: quickFindDialog.open()
                    Keys.onSpacePressed: quickFindDialog.open()
                    Keys.onReturnPressed: quickFindDialog.open()
                    Keys.onEnterPressed: quickFindDialog.open()
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
                            text: qsTr("Quick Find (Ctrl+K)")
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

                    FocusRing {
                        color: root.colAccent
                    }
                }

                // Core GTD Navigation
                ColumnLayout {
                    Layout.fillWidth: true
                    spacing: 2

                    Repeater {
                        model: root.fixedViews

                        delegate: Rectangle {
                            id: navItem
                            objectName: "nav-" + modelData.id
                            activeFocusOnTab: true
                            Accessible.role: Accessible.ListItem
                            Accessible.name: navItem.modelData.title
                            Accessible.selectable: true
                            Accessible.selected: navItem.isSelected
                            Accessible.onPressAction: controller.selectView(navItem.modelData.id)
                            Keys.onSpacePressed: controller.selectView(navItem.modelData.id)
                            Keys.onReturnPressed: controller.selectView(navItem.modelData.id)
                            Keys.onEnterPressed: controller.selectView(navItem.modelData.id)
                            Accessible.description: navItem.modelData.count > 0 ? qsTr("%n open", "", navItem.modelData.count) : ""
                            required property var modelData
                            Layout.fillWidth: true
                            height: 32
                            radius: 6
                            readonly property bool isSelected: controller.currentView === navItem.modelData.id
                            // A task dropped here moves to the Inbox or is
                            // scheduled for Today or Someday (#54).
                            readonly property string dropKind: ["inbox", "today", "someday"].indexOf(modelData.id) >= 0 ? modelData.id : ""
                            readonly property string dropId: ""
                            color: isSelected ? root.colSidebarActive : (navItemMouse.containsMouse ? root.colSidebarHover : "transparent")
                            border.width: root.sidebarDrop && root.sidebarDrop.kind === dropKind ? 2 : 0
                            border.color: root.colAccent

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
                                    color: navItem.isSelected ? root.colAccent : root.colBadge

                                    Text {
                                        id: countText
                                        anchors.centerIn: parent
                                        text: navItem.modelData.count.toString()
                                        font.pixelSize: 11
                                        font.weight: Font.DemiBold
                                        color: navItem.isSelected ? root.colOnAccent : root.colTextSecondary
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

                            FocusRing {
                                color: root.colAccent
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
                        text: qsTr("PROJECTS")
                        color: root.colTextSecondary
                        font.pixelSize: 11
                        font.weight: Font.Bold
                        Layout.fillWidth: true
                    }

                    ToolButton {
                        objectName: "tagsButton"
                        Accessible.name: qsTr("Tags")
                        icon.width: 16
                        contentItem: Icon {
                            name: "tag"
                            size: 16
                            color: root.colTextSecondary
                        }
                        ToolTip.visible: hovered
                        ToolTip.text: qsTr("Tags")
                        onClicked: tagsDialog.open()
                    }

                    ToolButton {
                        objectName: "preferencesButton"
                        icon.width: 16
                        Accessible.name: qsTr("Preferences")
                        contentItem: Icon {
                            name: "settings"
                            size: 16
                            color: root.colTextSecondary
                        }
                        ToolTip.visible: hovered
                        ToolTip.text: qsTr("Preferences (Ctrl+,)")
                        onClicked: preferencesDialog.open()
                    }

                    // Sync (#42): how it is going, and the way in.
                    ToolButton {
                        objectName: "syncButton"
                        icon.width: 16
                        Accessible.name: qsTr("Sync: %1").arg(root.syncStatusText())
                        contentItem: Icon {
                            name: "sync"
                            size: 16
                            color: controller.syncPhase === "on" && controller.syncHealth !== "up-to-date"
                                   && controller.syncHealth !== "syncing" && controller.syncHealth !== "idle"
                                   ? root.colDanger : root.colTextSecondary
                        }
                        ToolTip.visible: hovered
                        ToolTip.text: root.syncStatusText()
                        onClicked: syncDialog.open()
                    }

                    ToolButton {
                        objectName: "newContainerButton"
                        Accessible.name: qsTr("New Project or Area")
                        icon.width: 16
                        contentItem: Icon {
                            name: "plus"
                            size: 16
                            color: root.colTextSecondary
                        }
                        ToolTip.visible: hovered
                        ToolTip.text: qsTr("New Project or Area")
                        onClicked: newContainerMenu.popup()

                        Menu {
                            id: newContainerMenu
                            MenuItem {
                                objectName: "newProjectItem"
                                text: qsTr("New Project")
                                onTriggered: newProjectDialog.open()
                            }
                            MenuItem {
                                objectName: "newAreaItem"
                                text: qsTr("New Area")
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
                                    objectName: "areaBlock"
                                    required property var modelData
                                    width: projectsList.width
                                    spacing: 2

                                    Rectangle {
                                        id: areaRow
                                        objectName: "areaItem"
                                        activeFocusOnTab: true
                                        Accessible.role: Accessible.ListItem
                                        Accessible.name: areaBlock.modelData.title
                                        Accessible.selectable: true
                                        Accessible.selected: areaRow.isSelected
                                        Accessible.onPressAction: controller.selectArea(areaBlock.modelData.id)
                                        Keys.onSpacePressed: controller.selectArea(areaBlock.modelData.id)
                                        Keys.onReturnPressed: controller.selectArea(areaBlock.modelData.id)
                                        Keys.onEnterPressed: controller.selectArea(areaBlock.modelData.id)
                                        readonly property string areaTitle: areaBlock.modelData.title
                                        readonly property bool isSelected: controller.currentView === "area"
                                            && controller.currentAreaId === areaBlock.modelData.id
                                        readonly property string dropKind: "area"
                                        readonly property string dropId: areaBlock.modelData.id
                                        width: parent.width
                                        height: 30
                                        radius: 6
                                        color: isSelected ? root.colSidebarActive : (areaMouse.containsMouse ? root.colSidebarHover : "transparent")
                                        border.width: root.sidebarDrop && root.sidebarDrop.kind === dropKind && root.sidebarDrop.id === dropId ? 2 : 0
                                        border.color: root.colAccent

                                        RowLayout {
                                            anchors.fill: parent
                                            anchors.leftMargin: 22
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

                                        FocusRing {
                                            color: root.colAccent
                                        }

                                        // Drag grip: reorders the areas (#164).
                                        Item {
                                            objectName: "areaGrip"
                                            anchors.left: parent.left
                                            anchors.verticalCenter: parent.verticalCenter
                                            width: 20
                                            height: parent.height
                                            opacity: areaMouse.containsMouse || areaDrag.active ? 1 : 0

                                            Icon {
                                                anchors.centerIn: parent
                                                name: "grip"
                                                size: 14
                                                color: root.colTextSecondary
                                            }
                                            HoverHandler { cursorShape: Qt.OpenHandCursor }
                                            DragHandler {
                                                id: areaDrag
                                                target: null
                                                onActiveChanged: {
                                                    if (active) {
                                                        root.dragAreaId = areaBlock.modelData.id;
                                                        root.areaDrop = null;
                                                    } else {
                                                        root.endAreaDrag();
                                                    }
                                                }
                                                onCentroidChanged: if (active) root.updateAreaDrag(centroid.scenePosition)
                                            }
                                        }
                                    }

                                    Repeater {
                                        model: root.projects.filter(p => p.area_id === areaBlock.modelData.id)

                                        delegate: Rectangle {
                                            id: areaProject
                                            objectName: "areaProjectItem"
                                            activeFocusOnTab: true
                                            Accessible.role: Accessible.ListItem
                                            Accessible.name: areaProject.modelData.title
                                            Accessible.selectable: true
                                            Accessible.selected: areaProject.isSelected
                                            Accessible.onPressAction: controller.selectProject(areaProject.modelData.id)
                                            Keys.onSpacePressed: controller.selectProject(areaProject.modelData.id)
                                            Keys.onReturnPressed: controller.selectProject(areaProject.modelData.id)
                                            Keys.onEnterPressed: controller.selectProject(areaProject.modelData.id)
                                            required property var modelData
                                            readonly property string projectTitle: modelData.title
                                            readonly property bool isSelected: controller.currentView === "project"
                                                && controller.currentProjectId === modelData.id
                                            readonly property string dropKind: "project"
                                            readonly property string dropId: modelData.id
                                            width: areaBlock.width
                                            height: 28
                                            radius: 6
                                            color: isSelected ? root.colSidebarActive : (areaProjectMouse.containsMouse ? root.colSidebarHover : "transparent")
                                            border.width: root.sidebarDrop && root.sidebarDrop.kind === dropKind && root.sidebarDrop.id === dropId ? 2 : 0
                                            border.color: root.colAccent

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

                                            FocusRing {
                                                color: root.colAccent
                                            }

                                            // Drag grip, as for projects with no
                                            // area: within the area or elsewhere (#164).
                                            Item {
                                                objectName: "projectGrip"
                                                x: 8
                                                anchors.verticalCenter: parent.verticalCenter
                                                width: 20
                                                height: parent.height
                                                opacity: areaProjectMouse.containsMouse || areaProjectDrag.active ? 1 : 0

                                                Icon {
                                                    anchors.centerIn: parent
                                                    name: "grip"
                                                    size: 14
                                                    color: root.colTextSecondary
                                                }
                                                HoverHandler { cursorShape: Qt.OpenHandCursor }
                                                DragHandler {
                                                    id: areaProjectDrag
                                                    target: null
                                                    onActiveChanged: active ? root.beginProjectDrag(areaProject.modelData.id) : root.endProjectDrag()
                                                    onCentroidChanged: if (active) root.updateProjectDrag(centroid.scenePosition)
                                                }
                                            }
                                        }
                                    }
                                }
                            }
                        }

                        // Where a dragged project will land.
                        Rectangle {
                            objectName: "projectDropIndicator"
                            readonly property var drop: root.dragAreaId !== "" ? root.areaDrop : root.projectDrop
                            visible: (root.dragProjectId !== "" || root.dragAreaId !== "") && drop !== null
                            z: 10
                            x: 4
                            width: projectsList.width - 8
                            height: 2
                            radius: 1
                            y: drop ? drop.y - 1 : 0
                            color: root.colAccent
                        }
                        // Projects with no area; area projects are listed under
                        // their area (footer).
                        model: root.projects.filter(p => !p.area_id)

                        delegate: Rectangle {
                            id: projItem
                            objectName: "projectItem"
                            activeFocusOnTab: true
                            Accessible.role: Accessible.ListItem
                            Accessible.name: projItem.modelData.title
                            Accessible.selectable: true
                            Accessible.selected: projItem.isSelected
                            Accessible.onPressAction: controller.selectProject(projItem.modelData.id)
                            Keys.onSpacePressed: controller.selectProject(projItem.modelData.id)
                            Keys.onReturnPressed: controller.selectProject(projItem.modelData.id)
                            Keys.onEnterPressed: controller.selectProject(projItem.modelData.id)
                            required property var modelData
                            width: projectsList.width
                            height: 30
                            radius: 6
                            readonly property bool isSelected: controller.currentView === "project" && controller.currentProjectId === projItem.modelData.id
                            readonly property string dropKind: "project"
                            readonly property string dropId: modelData.id
                            color: isSelected ? root.colSidebarActive : (projMouse.containsMouse ? root.colSidebarHover : "transparent")
                            border.width: root.sidebarDrop && root.sidebarDrop.kind === dropKind && root.sidebarDrop.id === dropId ? 2 : 0
                            border.color: root.colAccent

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

                            FocusRing {
                                color: root.colAccent
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
                                    onActiveChanged: active ? root.beginProjectDrag(projItem.modelData.id) : root.endProjectDrag()
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
                        Accessible.name: root.sidebarShown ? qsTr("Hide the sidebar") : qsTr("Show the sidebar")
                        focusPolicy: Qt.NoFocus
                        contentItem: Icon {
                            name: "sidebar"
                            size: 18
                            color: root.colTextSecondary
                        }
                        ToolTip.visible: hovered
                        ToolTip.text: root.sidebarShown ? qsTr("Hide the sidebar (F9)") : qsTr("Show the sidebar (F9)")
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
                        ToolTip.text: qsTr("%1% done").arg(Math.round(progress * 100))
                    }

                    // The title, and beside it whether a project is finished
                    // (#55).
                    Row {
                        Layout.fillWidth: true
                        spacing: 12

                        Text {
                            id: viewTitle
                            text: root.title
                            font.pixelSize: 28
                            font.weight: Font.Bold
                            color: projectStatusLabel.visible ? root.colTextSecondary : root.colTextPrimary
                        }

                        Label {
                            id: projectStatusLabel
                            objectName: "projectStatusLabel"
                            readonly property string status: controller.currentProjectStatus
                            anchors.verticalCenter: viewTitle.verticalCenter
                            visible: status === "completed" || status === "canceled"
                            text: status === "completed" ? qsTr("Completed") : qsTr("Canceled")
                            font.pixelSize: 12
                            font.weight: Font.DemiBold
                            color: root.colTextSecondary
                            leftPadding: 8
                            rightPadding: 8
                            topPadding: 3
                            bottomPadding: 3
                            background: Rectangle {
                                radius: 9
                                color: root.colChip
                            }
                        }
                    }

                    // Once every task is done, the project offers to be
                    // completed (#55).
                    Button {
                        objectName: "completeProjectButton"
                        text: qsTr("Complete Project")
                        visible: controller.currentView === "project" && controller.currentProjectStatus === "open"
                            && controller.currentProjectProgress >= 1
                        onClicked: root.askFinishProject("completed")
                    }

                    // Finished tasks waiting in a list go to the Logbook
                    // (#67).
                    Button {
                        objectName: "logCompletedButton"
                        text: qsTr("Log Completed")
                        visible: root.logsByHand
                            && ["inbox", "today", "upcoming", "anytime", "someday"].indexOf(controller.currentView) >= 0
                        onClicked: controller.logCompleted()
                    }

                    Button {
                        id: emptyTrashButton
                        objectName: "emptyTrashButton"
                        text: qsTr("Empty Trash")
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
                        Accessible.name: controller.currentView === "area" ? qsTr("Area actions") : qsTr("Project actions")
                        contentItem: Icon {
                            name: "more"
                            size: 20
                            color: root.colTextSecondary
                        }
                        visible: controller.currentView === "project" || controller.currentView === "area"
                        ToolTip.visible: hovered
                        ToolTip.text: controller.currentView === "area" ? qsTr("Area actions") : qsTr("Project actions")
                        onClicked: (controller.currentView === "area" ? areaMenu : projectMenu).popup(containerMenuButton, 0, height)

                        Menu {
                            id: projectMenu
                            objectName: "projectMenu"

                            // Which area the project is filed in.
                            Menu {
                                id: projectAreaMenu
                                objectName: "projectAreaMenu"
                                title: qsTr("Area")

                                Instantiator {
                                    model: [{ id: "", title: qsTr("No Area") }].concat(root.areas)
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
                                text: qsTr("Rename…")
                                onTriggered: root.openRename("project")
                            }

                            MenuItem {
                                objectName: "addHeadingItem"
                                text: qsTr("Add Heading…")
                                onTriggered: root.openRename("newHeading", "", "")
                            }

                            MenuSeparator {}

                            // Finishing (#55); a finished project is reopened
                            // from the same item.
                            MenuItem {
                                objectName: "completeProjectItem"
                                readonly property bool open: controller.currentProjectStatus === "open"
                                text: open ? qsTr("Complete Project") : qsTr("Reopen Project")
                                onTriggered: {
                                    if (open)
                                        root.askFinishProject("completed");
                                    else
                                        controller.reopenProject(controller.currentProjectId);
                                }
                            }
                            MenuItem {
                                objectName: "cancelProjectItem"
                                text: qsTr("Cancel Project")
                                enabled: controller.currentProjectStatus === "open"
                                onTriggered: root.askFinishProject("canceled")
                            }

                            MenuSeparator {}

                            MenuItem {
                                objectName: "trashProjectItem"
                                text: qsTr("Move to Trash")
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
                                text: qsTr("Rename…")
                                onTriggered: root.openRename("area")
                            }

                            MenuSeparator {}

                            MenuItem {
                                objectName: "trashAreaItem"
                                text: qsTr("Move to Trash")
                                onTriggered: {
                                    const id = controller.currentAreaId;
                                    root.showUndo("area", id, controller.currentTitle);
                                    controller.trashArea(id);
                                }
                            }
                        }
                    }
                }

                // Another open of the database took over as this device's
                // writer (#45): changes here are refused until it reloads.
                Rectangle {
                    objectName: "supersededBanner"
                    visible: controller.databaseSuperseded
                    Layout.fillWidth: true
                    implicitHeight: supersededRow.implicitHeight + 16
                    radius: 8
                    color: root.colDangerBg

                    RowLayout {
                        id: supersededRow
                        anchors.fill: parent
                        anchors.margins: 8
                        spacing: 8

                        Text {
                            objectName: "supersededText"
                            text: qsTr("Matters was opened again elsewhere, for example by matters-syncd, so changes here are not being saved. Everything saved before is kept. Reload to continue with the latest.")
                            color: root.colDanger
                            font.pixelSize: 13
                            wrapMode: Text.Wrap
                            Layout.fillWidth: true
                        }

                        Button {
                            objectName: "reloadDatabase"
                            text: qsTr("Reload")
                            Accessible.name: qsTr("Reload the database")
                            onClicked: controller.reloadDatabase()
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
                    color: root.colDangerBg

                    RowLayout {
                        id: errorRow
                        anchors.fill: parent
                        anchors.margins: 8
                        spacing: 8

                        Text {
                            text: errorBanner.message
                            color: root.colDanger
                            font.pixelSize: 13
                            wrapMode: Text.Wrap
                            Layout.fillWidth: true
                        }

                        ToolButton {
                            contentItem: Icon {
                                name: "close"
                                size: 14
                                color: root.colDanger
                            }
                            ToolTip.visible: hovered
                            ToolTip.text: qsTr("Dismiss")
                            Accessible.name: qsTr("Dismiss")
                            visible: controller.errorMessage !== ""
                            onClicked: controller.clearError()
                        }
                    }
                }

                // A project's When, deadline and notes (#56), under its title.
                ColumnLayout {
                    objectName: "projectDetails"
                    visible: controller.currentView === "project"
                    Layout.fillWidth: true
                    // In line with the title, past the toggle and the pie.
                    Layout.leftMargin: 78
                    Layout.topMargin: -10
                    spacing: 6

                    RowLayout {
                        spacing: 8

                        DatePill {
                            objectName: "projectWhen"
                            label: root.describeWhenOf(controller.currentProjectStartDate,
                                controller.currentProjectEvening, controller.currentProjectSomeday)
                            hasValue: controller.currentProjectStartDate !== ""
                                || controller.currentProjectEvening || controller.currentProjectSomeday
                            iconName: "calendar"
                            textPrimary: root.colTextPrimary
                            textSecondary: root.colTextSecondary
                            lineColor: root.colBorder
                            hoverColor: root.colSidebarHover
                            downColor: root.colSidebarActive
                            ToolTip.visible: hovered
                            ToolTip.text: qsTr("When the project starts")
                            onClicked: root.openProjectDatePopup("when")
                        }

                        DatePill {
                            objectName: "projectDeadline"
                            label: controller.currentProjectDeadline === "" ? qsTr("Deadline")
                                : root.describeDeadline(controller.currentProjectDeadline, controller.currentProjectDeadlineStatus)
                            hasValue: controller.currentProjectDeadline !== ""
                            iconName: "deadline"
                            textPrimary: root.colTextPrimary
                            textSecondary: root.colTextSecondary
                            lineColor: root.colBorder
                            hoverColor: root.colSidebarHover
                            downColor: root.colSidebarActive
                            ToolTip.visible: hovered
                            ToolTip.text: qsTr("The project's deadline")
                            onClicked: root.openProjectDatePopup("deadline")
                        }
                    }

                    TagField {
                        objectName: "projectTags"
                        Layout.fillWidth: true
                        kind: "project"
                        itemId: controller.currentProjectId
                        tags: root.currentTags
                        allTags: root.allTags
                        controller: controller
                        textPrimary: root.colTextPrimary
                        textSecondary: root.colTextSecondary
                        chipColor: root.colChip
                        highlightColor: root.colSidebarActive
                        onLeave: taskListView.forceActiveFocus()
                    }

                    // Saves after a pause in typing and when editing ends,
                    // like a task's notes.
                    TextArea {
                        id: projectNotes
                        objectName: "projectNotes"
                        Accessible.name: qsTr("Project notes")
                        // The project whose notes are in the editor, and what
                        // was last loaded or saved for it.
                        property string projectId: ""
                        property string saved: ""
                        Layout.fillWidth: true
                        Layout.preferredHeight: Math.max(32, contentHeight + topPadding + bottomPadding)
                        placeholderText: qsTr("Notes")
                        font.pixelSize: 13
                        color: root.colTextPrimary
                        wrapMode: TextArea.Wrap
                        leftPadding: 0
                        background: null

                        function save() {
                            projectNotesTimer.stop();
                            if (projectId !== "" && text !== saved) {
                                saved = text;
                                controller.updateProjectNotes(projectId, text);
                            }
                        }

                        // Not a binding: while the user is typing, the stored
                        // notes must not overwrite the editor.
                        function load() {
                            projectId = controller.currentProjectId;
                            saved = controller.currentProjectNotes;
                            text = saved;
                        }
                        Component.onCompleted: load()
                        Connections {
                            target: controller
                            // The id changes before the controller has
                            // switched projects: what was typed is saved to
                            // the project it was typed for at once, and the
                            // next project's notes are loaded once it is open.
                            function onCurrentProjectIdChanged() {
                                projectNotes.save();
                                Qt.callLater(projectNotes.load);
                            }
                            function onCurrentProjectNotesChanged() {
                                if (!projectNotes.activeFocus && projectNotes.projectId === controller.currentProjectId) {
                                    projectNotes.saved = controller.currentProjectNotes;
                                    projectNotes.text = projectNotes.saved;
                                }
                            }
                        }

                        onTextChanged: if (activeFocus) projectNotesTimer.restart()
                        onEditingFinished: save()
                        Keys.onEscapePressed: taskListView.forceActiveFocus()

                        Timer {
                            id: projectNotesTimer
                            interval: 600
                            onTriggered: projectNotes.save()
                        }
                    }
                }

                // An area's tags (#57), under its title.
                TagField {
                    objectName: "areaTags"
                    visible: controller.currentView === "area"
                    Layout.fillWidth: true
                    Layout.leftMargin: 46
                    Layout.topMargin: -10
                    kind: "area"
                    itemId: controller.currentAreaId
                    tags: root.currentTags
                    allTags: root.allTags
                    controller: controller
                    textPrimary: root.colTextPrimary
                    textSecondary: root.colTextSecondary
                    chipColor: root.colChip
                    highlightColor: root.colSidebarActive
                    onLeave: taskListView.forceActiveFocus()
                }

                // Quick Task Input Bar (not in the Logbook or the Trash)
                Rectangle {
                    objectName: "quickAddBar"
                    visible: controller.acceptsNewTasks
                    Layout.fillWidth: true
                    Layout.preferredHeight: 42
                    radius: 8
                    color: root.colInputBar
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
                            Accessible.name: qsTr("New task")
                            Layout.fillWidth: true
                            font.pixelSize: 14
                            color: root.colTextPrimary
                            clip: true

                            Text {
                                anchors.fill: parent
                                text: qsTr("Add a task")
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
                                    text: qsTr("Put Back")
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
                                    text: qsTr("Put Back")
                                    font.pixelSize: 11
                                    onClicked: controller.restoreProject(trashedProject.modelData.id)
                                }
                            }
                        }
                    }
                }

                // The tags present in the list (#58): choosing one narrows the
                // list to it (a parent includes its children); choosing it
                // again, or All, shows everything.
                Flow {
                    objectName: "tagBar"
                    visible: root.presentTags.length > 0
                    Layout.fillWidth: true
                    Layout.topMargin: -8
                    spacing: 6

                    Repeater {
                        model: [{ id: "", name: qsTr("All"), depth: 0 }].concat(root.presentTags)
                        delegate: Rectangle {
                            id: filterChip
                            objectName: "filterTag"
                            required property var modelData
                            readonly property string tagId: modelData.id
                            readonly property string name: modelData.name
                            readonly property bool chosen: root.tagFilter === tagId
                            height: 24
                            width: filterText.contentWidth + 20
                            radius: 12
                            color: chosen ? root.colAccent : (filterMouse.containsMouse ? root.colSidebarHover : "transparent")
                            border.width: 1
                            border.color: chosen ? root.colAccent : root.colBorder
                            Accessible.role: Accessible.Button
                            Accessible.name: tagId === "" ? qsTr("Show all tasks") : qsTr("Show only %1").arg(name)
                            Accessible.checked: chosen

                            Text {
                                id: filterText
                                anchors.centerIn: parent
                                text: filterChip.name
                                font.pixelSize: 12
                                color: filterChip.chosen ? root.colOnAccent : root.colTextSecondary
                            }
                            MouseArea {
                                id: filterMouse
                                anchors.fill: parent
                                hoverEnabled: true
                                cursorShape: Qt.PointingHandCursor
                                onClicked: root.tagFilter = filterChip.chosen ? "" : filterChip.tagId
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
                        // The task the keyboard is on (the cursor).
                        property string selectedTaskId: ""
                        // The selected tasks (#64): the cursor's task, or several
                        // around it. Setting the cursor to a task outside them
                        // selects that task alone; within them, they stay.
                        property var selectedIds: []
                        // Where Shift extends the selection from.
                        property string selectionAnchor: ""
                        onSelectedTaskIdChanged: {
                            if (selectedIds.indexOf(selectedTaskId) < 0) {
                                selectedIds = selectedTaskId === "" ? [] : [selectedTaskId];
                                selectionAnchor = selectedTaskId;
                            }
                        }

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
                                Accessible.name: qsTr("Heading actions")
                                contentItem: Icon {
                                    name: "more"
                                    size: 16
                                    color: root.colTextSecondary
                                }
                                ToolTip.visible: hovered
                                ToolTip.text: qsTr("Heading actions")
                                onClicked: headingMenu.popup(headingMenuButton, 0, height)

                                Menu {
                                    id: headingMenu
                                    objectName: "headingMenu"

                                    MenuItem {
                                        objectName: "renameHeadingItem"
                                        text: qsTr("Rename…")
                                        onTriggered: root.openRename("heading", sectionHeading.headingId, sectionHeading.title)
                                    }
                                    MenuItem {
                                        objectName: "moveHeadingUpItem"
                                        text: qsTr("Move Up")
                                        onTriggered: controller.moveHeading(sectionHeading.headingId, -1)
                                    }
                                    MenuItem {
                                        objectName: "moveHeadingDownItem"
                                        text: qsTr("Move Down")
                                        onTriggered: controller.moveHeading(sectionHeading.headingId, 1)
                                    }
                                    // To another project, with its tasks (#169).
                                    Menu {
                                        id: headingProjectMenu
                                        objectName: "moveHeadingToProjectMenu"
                                        title: qsTr("Move to Project")
                                        Instantiator {
                                            model: root.projects.filter(p => p.id !== controller.currentProjectId)
                                            delegate: MenuItem {
                                                required property var modelData
                                                objectName: "moveHeadingTo-" + modelData.id
                                                text: modelData.title
                                                onTriggered: controller.moveHeadingToProject(sectionHeading.headingId, modelData.id)
                                            }
                                            onObjectAdded: (index, object) => headingProjectMenu.insertItem(index, object)
                                            onObjectRemoved: (index, object) => headingProjectMenu.removeItem(object)
                                        }
                                    }
                                    MenuSeparator {}
                                    // Its tasks stay in the project (decided on #22).
                                    MenuItem {
                                        objectName: "deleteHeadingItem"
                                        text: qsTr("Delete Heading")
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
                            // Shift+Up/Down selects several tasks (#64).
                            if (event.modifiers === Qt.ShiftModifier
                                    && (event.key === Qt.Key_Up || event.key === Qt.Key_Down)) {
                                root.extendSelection(event.key === Qt.Key_Up ? -1 : 1);
                                event.accepted = true;
                                return;
                            }
                            // Ctrl+. completes the selected tasks (#64).
                            if (event.modifiers === Qt.ControlModifier && event.key === Qt.Key_Period) {
                                root.completeSelected();
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
                                // Typing a name searches for it (#66): any
                                // printable key opens Quick Find with it.
                                if (!root.isTyped(event.text))
                                    return;
                                root.findTyped(event.text);
                                break;
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
                            text: qsTr("No tasks here. Enjoy your peace of mind.")
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

                            // A repeating task's coming occurrence (#61), in
                            // Upcoming: what will be created on its day.
                            // Quieter than a task, and not a task yet.
                            DelegateChoice {
                                roleValue: "preview"
                                delegate: Item {
                                    id: previewRow
                                    objectName: "previewRow"
                                    required property string taskId
                                    required property string title
                                    required property string startDate
                                    required property string repeat
                                    required property string section
                                    required property int index
                                    readonly property real targetHeight: height
                                    width: taskListView.width
                                    height: 36
                                    opacity: 0.65
                                    Accessible.role: Accessible.StaticText
                                    Accessible.name: qsTr("Will repeat: %1, %2").arg(title).arg(repeat)

                                    RowLayout {
                                        anchors.fill: parent
                                        anchors.leftMargin: 32
                                        anchors.rightMargin: 16
                                        spacing: 10

                                        Icon {
                                            name: "repeat"
                                            size: 16
                                            color: root.colTextSecondary
                                        }
                                        Text {
                                            objectName: "previewTitle"
                                            Layout.fillWidth: true
                                            text: previewRow.title
                                            elide: Text.ElideRight
                                            font.pixelSize: 14
                                            color: root.colTextSecondary
                                        }
                                    }
                                }
                            }

                            // A calendar event (#62): read-only context in
                            // Today and Upcoming, drawn quieter than a task,
                            // with its calendar's colour. Nothing happens on
                            // a click: events are edited in their calendar.
                            DelegateChoice {
                                roleValue: "event"
                                delegate: Item {
                                    id: eventRow
                                    objectName: "eventRow"
                                    required property string taskId
                                    required property string title
                                    required property string startDate
                                    required property string eventStart
                                    required property string eventEnd
                                    required property bool allDay
                                    required property string calendarColor
                                    required property string calendarName
                                    required property string section
                                    required property int index
                                    readonly property string when: root.describeEventTime(startDate, eventStart, eventEnd, allDay)
                                    readonly property real targetHeight: height
                                    width: taskListView.width
                                    height: 32
                                    Accessible.role: Accessible.StaticText
                                    Accessible.name: calendarName !== "" ? qsTr("Event: %1, %2, %3").arg(title).arg(when).arg(calendarName)
                                        : qsTr("Event: %1, %2").arg(title).arg(when)

                                    RowLayout {
                                        anchors.fill: parent
                                        // Lined up with the tasks' checkboxes.
                                        anchors.leftMargin: 32
                                        anchors.rightMargin: 16
                                        spacing: 10

                                        Rectangle {
                                            objectName: "eventColor"
                                            Layout.preferredWidth: 4
                                            Layout.preferredHeight: 18
                                            radius: 2
                                            color: eventRow.calendarColor !== "" ? eventRow.calendarColor : root.colTextSecondary
                                        }
                                        Text {
                                            objectName: "eventTime"
                                            Layout.preferredWidth: 110
                                            text: eventRow.when
                                            elide: Text.ElideRight
                                            font.pixelSize: 13
                                            color: root.colTextSecondary
                                        }
                                        Text {
                                            objectName: "eventTitle"
                                            Layout.fillWidth: true
                                            text: eventRow.title
                                            elide: Text.ElideRight
                                            font.pixelSize: 14
                                            color: root.colTextPrimary
                                        }
                                    }
                                }
                            }

                            // A project listed as a row: finished, in the
                            // Logbook, standing for its tasks (#55); due in
                            // Today, starting in Upcoming, or deferred in Someday
                            // (#56). Clicking it opens the project.
                            DelegateChoice {
                                roleValue: "project"
                                delegate: Rectangle {
                                    id: projectRow
                                    objectName: "projectRow"
                                    required property string taskId
                                    required property string title
                                    required property bool done
                                    required property string status
                                    required property string deadline
                                    required property string deadlineStatus
                                    required property int index
                                    readonly property bool finished: status === "completed" || status === "canceled"
                                    readonly property real targetHeight: height
                                    width: taskListView.width
                                    height: 48
                                    radius: 8
                                    color: projectRowMouse.containsMouse ? root.colRowHover : "transparent"
                                    Accessible.role: Accessible.ListItem
                                    Accessible.name: projectOutcome.text + ": " + title

                                    RowLayout {
                                        anchors.fill: parent
                                        // Lined up with the tasks' checkboxes.
                                        anchors.leftMargin: 32
                                        anchors.rightMargin: 16
                                        spacing: 12

                                        // A ring for an open project, a full pie
                                        // when completed, a crossed ring when
                                        // canceled.
                                        Rectangle {
                                            Layout.preferredWidth: 20
                                            Layout.preferredHeight: 20
                                            radius: 10
                                            color: "transparent"
                                            border.width: 2
                                            border.color: projectRow.status === "canceled" ? root.colTextSecondary : root.colAccent

                                            Rectangle {
                                                visible: projectRow.done
                                                anchors.centerIn: parent
                                                width: 12
                                                height: 12
                                                radius: 6
                                                color: root.colAccent
                                            }
                                            Icon {
                                                visible: projectRow.status === "canceled"
                                                anchors.centerIn: parent
                                                name: "close"
                                                size: 12
                                                color: root.colTextSecondary
                                            }
                                        }
                                        Text {
                                            text: projectRow.title
                                            font.pixelSize: 15
                                            font.weight: Font.DemiBold
                                            color: projectRow.finished ? root.colTextSecondary : root.colTextPrimary
                                            elide: Text.ElideRight
                                            Layout.fillWidth: true
                                        }
                                        // What the row is, or for a due project
                                        // its deadline, in red like a task's.
                                        Text {
                                            id: projectOutcome
                                            objectName: "projectRowLabel"
                                            readonly property bool urgent: !projectRow.finished
                                                && (projectRow.deadlineStatus === "today" || projectRow.deadlineStatus === "overdue")
                                            text: projectRow.status === "completed" ? qsTr("Completed project")
                                                : projectRow.status === "canceled" ? qsTr("Canceled project")
                                                : urgent ? root.describeDeadline(projectRow.deadline, projectRow.deadlineStatus)
                                                : qsTr("Project")
                                            font.pixelSize: 12
                                            font.weight: urgent ? Font.DemiBold : Font.Normal
                                            color: urgent ? root.colDanger : root.colTextSecondary
                                        }
                                    }

                                    MouseArea {
                                        id: projectRowMouse
                                        anchors.fill: parent
                                        hoverEnabled: true
                                        cursorShape: Qt.PointingHandCursor
                                        onClicked: controller.selectProject(projectRow.taskId)
                                    }
                                }
                            }

                            DelegateChoice {
                                roleValue: "task"
                                delegate: FocusScope {
                                    id: taskRow
                                    objectName: "taskRow"
                                    Accessible.role: Accessible.ListItem
                                    Accessible.name: title
                                    Accessible.description: [done ? qsTr("Completed") : canceled ? qsTr("Canceled") : "",
        repeat !== "" ? qsTr("Repeats %1").arg(repeat) : "",
        reminder !== "" ? qsTr("Reminder at %1").arg(root.describeTime(reminder)) : ""].filter(s => s !== "").join(qsTr(", ", "between parts of a list"))
                                    Accessible.selectable: true
                                    Accessible.selected: isSelected
                                    // The list keeps the keyboard focus and moves its own
                                    // cursor (Up, Down), so the row under the cursor tells a
                                    // screen reader it is focused, and the reader follows.
                                    Accessible.focused: taskListView.activeFocus && taskListView.selectedTaskId === taskId
                                    Accessible.onPressAction: {
                                        root.selectOnly(taskId);
                                        taskListView.expandedTaskId = isExpanded ? "" : taskId;
                                        taskListView.forceActiveFocus();
                                    }
                                    required property string taskId
                                    required property string title
                                    required property string notes
                                    required property bool done
                                    // `open`, `completed` or `canceled` (#65).
                                    required property string status
                                    readonly property bool canceled: status === "canceled"
                                    // Finished: done or canceled.
                                    readonly property bool finished: status !== "open"
                                    required property string startDate
                                    required property bool evening
                                    required property string deadline
                                    required property string deadlineStatus
                                    // `HH:MM` while the task has a reminder due (#60).
                                    required property string reminder
                                    // The rule it carries, in words, or "" (#61).
                                    required property string repeat
                                    required property bool someday
                                    required property int checklistDone
                                    required property int checklistTotal
                                    // JSON `[{id, name}]`: the task's own tags (#57).
                                    required property string tags
                                    readonly property var tagList: JSON.parse(tags)
                                    required property int index

                                    readonly property bool isExpanded: taskListView.expandedTaskId === taskId
                                    readonly property bool isSelected: taskListView.selectedIds.indexOf(taskId) >= 0

                                    // F2 asked to edit this task's title: focus it once open.
                                    // The new-task field is open under this row (Enter).
                                    readonly property bool newBelowHere: root.newBelowId === taskId
                                    onNewBelowHereChanged: if (newBelowHere) Qt.callLater(taskRow.focusNewBelow)
                                    Component.onCompleted: if (newBelowHere) Qt.callLater(taskRow.focusNewBelow)
                                    function focusNewBelow() {
                                        if (newBelowHere)
                                            newBelowInput.forceActiveFocus();
                                    }

                                    readonly property bool tagEditRequested: root.tagEditId === taskId && isExpanded
                                    onTagEditRequestedChanged: if (tagEditRequested) Qt.callLater(taskRow.beginTagEdit)
                                    function beginTagEdit() {
                                        root.tagEditId = "";
                                        taskTags.focusInput();
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
                                            : taskRow.isSelected ? root.colRowSelected
                                            : rowHover.hovered ? root.colRowHover : "transparent"
                                        border.color: !taskRow.isExpanded ? "transparent"
                                            : taskRow.isSelected ? root.colRowSelectedBorder : root.colBorder
                                        border.width: 1
                                    }

                                    Behavior on height {
                                        enabled: !root.reduceMotion
                                        NumberAnimation { duration: 180; easing.type: Easing.OutCubic }
                                    }

                                    // Passive hover tracking: unlike a MouseArea it never
                                    // blocks clicks from reaching the controls in the row.
                                    HoverHandler { id: rowHover }

                                    // A right click opens the task's menu (#65).
                                    TapHandler {
                                        acceptedButtons: Qt.RightButton
                                        onTapped: (point, button) => root.openTaskMenu(taskRow.taskId, taskRow.mapFromItem(null, point.scenePosition.x, point.scenePosition.y), taskRow)
                                    }

                                    // Click on empty row space toggles expansion. Declared
                                    // first, so it sits beneath the row's controls and only
                                    // receives clicks that none of them accept.
                                    MouseArea {
                                        anchors.fill: parent
                                        onClicked: mouse => {
                                            // Shift extends the selection to this task,
                                            // Ctrl adds or removes it (#64).
                                            if (mouse.modifiers & Qt.ShiftModifier)
                                                root.extendSelectionTo(taskRow.taskId);
                                            else if (mouse.modifiers & Qt.ControlModifier)
                                                root.toggleInSelection(taskRow.taskId);
                                            else {
                                                root.selectOnly(taskRow.taskId);
                                                taskListView.expandedTaskId = taskRow.isExpanded ? "" : taskRow.taskId;
                                            }
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
                                                Accessible.role: Accessible.CheckBox
                                                Accessible.name: taskRow.canceled ? qsTr("Reopen %1").arg(taskRow.title) : qsTr("Complete %1").arg(taskRow.title)
                                                Accessible.description: qsTr("Ctrl+.")
                                                Accessible.checkable: true
                                                Accessible.checked: taskRow.done
                                                Accessible.onToggleAction: controller.toggleTask(taskRow.taskId)
                                                Accessible.onPressAction: controller.toggleTask(taskRow.taskId)
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
                                                    color: root.colOnAccent
                                                    visible: taskRow.done
                                                }
                                                // Canceled (#65): a cross, not a tick.
                                                Icon {
                                                    objectName: "canceledMark"
                                                    anchors.centerIn: parent
                                                    name: "close"
                                                    size: 12
                                                    strokeWidth: 2.4
                                                    color: root.colTextSecondary
                                                    visible: taskRow.canceled
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
                                                font.strikeout: taskRow.finished
                                                color: taskRow.finished ? root.colTextSecondary : root.colTextPrimary
                                                Layout.fillWidth: true
                                                elide: Text.ElideRight
                                            }

                                            TextInput {
                                                id: titleInput
                                                objectName: "titleInput"
                                                Accessible.name: qsTr("Title")
                                                visible: taskRow.isExpanded
                                                text: taskRow.title
                                                font.pixelSize: 14
                                                font.strikeout: taskRow.finished
                                                color: taskRow.finished ? root.colTextSecondary : root.colTextPrimary
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
                                                // Hidden (the task closed), it gives up the focus: the
                                                // row is a focus scope and would hand the keys back to
                                                // it, unseen, the next time the list takes the focus.
                                                onVisibleChanged: if (!visible) focus = false
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
                                                color: root.colEveningBg

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
                                                    text: qsTr("Evening")
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
                                                color: root.colChip

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
                                                color: urgent ? root.colDangerBg : root.colChip

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
                                                    color: parent.urgent ? root.colDanger : root.colTextSecondary
                                                }
                                            }

                                            // The task's tags, while the row is closed.
                                            Repeater {
                                                model: taskRow.isExpanded ? [] : taskRow.tagList.slice(0, 3)
                                                delegate: Rectangle {
                                                    id: rowTag
                                                    objectName: "rowTag"
                                                    required property var modelData
                                                    readonly property string name: modelData.name
                                                    Layout.preferredHeight: 20
                                                    Layout.preferredWidth: rowTagText.contentWidth + 14
                                                    radius: 10
                                                    color: root.colChip
                                                    Text {
                                                        id: rowTagText
                                                        anchors.centerIn: parent
                                                        text: rowTag.name
                                                        font.pixelSize: 11
                                                        color: root.colTextSecondary
                                                    }
                                                }
                                            }

                                            // It repeats (#61): the newest occurrence
                                            // of a series carries the rule.
                                            Icon {
                                                objectName: "repeatMarker"
                                                visible: taskRow.repeat !== ""
                                                name: "repeat"
                                                size: 14
                                                color: root.colTextSecondary
                                                Accessible.role: Accessible.StaticText
                                                Accessible.name: qsTr("Repeats: %1").arg(taskRow.repeat)
                                            }

                                            // A reminder (#60): a bell and its time.
                                            Rectangle {
                                                objectName: "reminderBadge"
                                                visible: taskRow.reminder !== ""
                                                Layout.preferredHeight: 20
                                                Layout.preferredWidth: reminderLabel.contentWidth + 28
                                                radius: 4
                                                color: root.colChip
                                                Accessible.role: Accessible.StaticText
                                                Accessible.name: qsTr("Reminder at %1").arg(reminderLabel.text)

                                                Icon {
                                                    id: reminderIcon
                                                    name: "bell"
                                                    size: 12
                                                    color: root.colTextSecondary
                                                    anchors.left: parent.left
                                                    anchors.leftMargin: 6
                                                    anchors.verticalCenter: parent.verticalCenter
                                                }

                                                Text {
                                                    id: reminderLabel
                                                    objectName: "reminderLabel"
                                                    anchors.left: reminderIcon.right
                                                    anchors.leftMargin: 4
                                                    anchors.verticalCenter: parent.verticalCenter
                                                    text: root.describeTime(taskRow.reminder)
                                                    font.pixelSize: 11
                                                    color: root.colTextSecondary
                                                }
                                            }

                                            // The task has notes, while the row is closed.
                                            Icon {
                                                objectName: "notesMarker"
                                                visible: taskRow.notes.trim() !== "" && !taskRow.isExpanded
                                                name: "notes"
                                                size: 14
                                                color: root.colTextSecondary
                                                Accessible.role: Accessible.StaticText
                                                Accessible.name: qsTr("Has notes")
                                            }

                                            // Checklist progress, while the row is closed.
                                            Rectangle {
                                                objectName: "checklistBadge"
                                                visible: taskRow.checklistTotal > 0 && !taskRow.isExpanded
                                                Layout.preferredHeight: 20
                                                Layout.preferredWidth: checklistLabel.contentWidth + 28
                                                radius: 4
                                                color: root.colChip
                                                Accessible.role: Accessible.StaticText
                                                Accessible.name: qsTr("Checklist: %1 of %2 done").arg(taskRow.checklistDone).arg(taskRow.checklistTotal)

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
                                                ToolTip.text: qsTr("Move to Trash")
                                                onClicked: {
                                                    root.showUndo("task", taskRow.taskId, taskRow.title);
                                                    controller.trashTask(taskRow.taskId);
                                                }
                                            }

                                            // In the Trash, each task can be put back.
                                            ToolButton {
                                                objectName: "restoreButton"
                                                focusPolicy: Qt.NoFocus
                                                text: qsTr("Put Back")
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
                                            color: root.colBg
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
                                                // Hidden, it gives up the focus (as the
                                                // title and notes do).
                                                onVisibleChanged: if (!visible) focus = false

                                                Text {
                                                    anchors.fill: parent
                                                    verticalAlignment: Text.AlignVCenter
                                                    text: qsTr("New to-do")
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
                                                Accessible.name: qsTr("Notes")
                                                // Editing a task's notes selects that task.
                                                onActiveFocusChanged: if (activeFocus) taskListView.selectedTaskId = taskRow.taskId
                                                // Hidden (the task closed), it gives up the focus: the
                                                // row is a focus scope and would hand the keys back to
                                                // it, unseen, the next time the list takes the focus.
                                                onVisibleChanged: if (!visible) focus = false
                                                Layout.fillWidth: true
                                                // Not being edited, notes with text show it
                                                // formatted (#63).
                                                readonly property bool rendered: !activeFocus && text.trim() !== ""
                                                // Grows with what is written, or with how it
                                                // is shown.
                                                Layout.preferredHeight: Math.max(44, (rendered ? notesView.contentHeight : contentHeight) + topPadding + bottomPadding)
                                                placeholderText: qsTr("Notes")
                                                font.pixelSize: 13
                                                // The source is unseen under its formatted view.
                                                color: rendered ? "transparent" : root.colTextPrimary
                                                selectionColor: rendered ? "transparent" : palette.highlight
                                                wrapMode: TextArea.Wrap
                                                leftPadding: 0
                                                background: null

                                                // The notes formatted as Markdown (CommonMark,
                                                // with GitHub's extensions, which link bare
                                                // URLs). A click on a link opens it; anywhere
                                                // else it edits the notes, with the caret where
                                                // the click was.
                                                TextEdit {
                                                    id: notesView
                                                    objectName: "notesView"
                                                    visible: notesInput.rendered
                                                    x: notesInput.leftPadding
                                                    y: notesInput.topPadding
                                                    width: notesInput.width - notesInput.leftPadding - notesInput.rightPadding
                                                    textFormat: TextEdit.MarkdownText
                                                    text: notesInput.text
                                                    readOnly: true
                                                    activeFocusOnPress: false
                                                    selectByMouse: false
                                                    wrapMode: TextEdit.Wrap
                                                    font.pixelSize: 13
                                                    color: root.colTextPrimary
                                                    Accessible.role: Accessible.StaticText
                                                    Accessible.name: qsTr("Notes: %1").arg(getText(0, length))

                                                    MouseArea {
                                                        objectName: "notesViewMouse"
                                                        anchors.fill: parent
                                                        cursorShape: notesView.linkAt(mouseX, mouseY) !== "" ? Qt.PointingHandCursor : Qt.IBeamCursor
                                                        hoverEnabled: true
                                                        onClicked: mouse => {
                                                            const link = notesView.linkAt(mouse.x, mouse.y);
                                                            if (link !== "") {
                                                                root.openLink(link);
                                                                return;
                                                            }
                                                            const at = notesView.positionAt(mouse.x, mouse.y);
                                                            const shown = notesView.getText(0, at);
                                                            notesInput.forceActiveFocus(Qt.MouseFocusReason);
                                                            notesInput.cursorPosition = root.sourcePosition(notesInput.text, shown);
                                                        }
                                                    }
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

                                            // Its tags (#57).
                                            TagField {
                                                id: taskTags
                                                objectName: "taskTags"
                                                Layout.fillWidth: true
                                                kind: "task"
                                                itemId: taskRow.taskId
                                                tags: taskRow.tagList
                                                allTags: root.allTags
                                                controller: controller
                                                textPrimary: root.colTextPrimary
                                                textSecondary: root.colTextSecondary
                                                chipColor: root.colChip
                                                highlightColor: root.colSidebarActive
                                                // Escape goes on to the list, which closes the task.
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
                                                    onItemDeleted: (itemId, title) => root.showUndo("checklist", itemId, title)
                                                    textPrimary: root.colTextPrimary
                                                    textSecondary: root.colTextSecondary
                                                    accent: root.colAccent
                                                    tickColor: root.colOnAccent
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
                                                    Accessible.name: qsTr("Add a checklist item")
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
                                                    ToolTip.text: qsTr("Add a checklist item")
                                                    onClicked: if (checklistLoader.editor) checklistLoader.editor.startAdding("")
                                                }

                                                DatePill {
                                                    objectName: "schedule-date"
                                                    // With its reminder's time (#60).
                                                    label: root.describeWhenOf(taskRow.startDate, taskRow.evening, taskRow.someday)
                                                        + (taskRow.reminder !== "" ? ", " + root.describeTime(taskRow.reminder) : "")
                                                    hasValue: taskRow.startDate !== "" || taskRow.evening || taskRow.someday
                                                    iconName: "calendar"
                                                    textPrimary: root.colTextPrimary
                                                    textSecondary: root.colTextSecondary
                                                    lineColor: root.colBorder
                                                    hoverColor: root.colSidebarHover
                                                    downColor: root.colSidebarActive
                                                    ToolTip.visible: hovered
                                                    ToolTip.text: qsTr("Change when (Ctrl+S)")
                                                    onClicked: root.openWhenFor(taskRow.taskId)
                                                }

                                                DatePill {
                                                    objectName: "schedule-deadline"
                                                    label: taskRow.deadline === "" ? qsTr("Deadline")
                                                        : root.describeDeadline(taskRow.deadline, taskRow.deadlineStatus)
                                                    hasValue: taskRow.deadline !== ""
                                                    iconName: "deadline"
                                                    textPrimary: root.colTextPrimary
                                                    textSecondary: root.colTextSecondary
                                                    lineColor: root.colBorder
                                                    hoverColor: root.colSidebarHover
                                                    downColor: root.colSidebarActive
                                                    ToolTip.visible: hovered
                                                    ToolTip.text: qsTr("Change the deadline (Ctrl+D)")
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
        color: root.colInverseBg

        RowLayout {
            id: undoRow
            anchors.centerIn: parent
            spacing: 12

            Text {
                objectName: "undoText"
                // Headings and checklist items are deleted, not trashed.
                text: root.undoKind === "batch" ? root.undoTitle
                    : root.undoKind === "heading" ? qsTr("Heading “%1” deleted").arg(root.undoTitle)
                    : root.undoKind === "checklist" ? qsTr("Checklist item “%1” deleted").arg(root.undoTitle)
                    : root.undoKind === "project" ? qsTr("Project “%1” moved to Trash").arg(root.undoTitle)
                    : root.undoKind === "area" ? qsTr("Area “%1” moved to Trash").arg(root.undoTitle)
                    : qsTr("Task “%1” moved to Trash").arg(root.undoTitle)
                color: root.colInverseText
                font.pixelSize: 13
                elide: Text.ElideMiddle
                Layout.maximumWidth: 360
            }

            Button {
                objectName: "undoButton"
                text: qsTr("Undo")
                flat: true
                onClicked: root.undo()
            }
        }
    }

    // =========================================================================
    // QUICK FIND POPUP DIALOG (Ctrl+K)
    // =========================================================================

    // Whether `text`, a key's text, is something typed: printable, not a
    // control character (Tab, Enter, Backspace, Delete, Escape).
    function isTyped(text: string): bool {
        if (text.length === 0)
            return false;
        const code = text.charCodeAt(0);
        return code >= 0x20 && code !== 0x7f;
    }

    // Open Quick Find searching for `text`, typed on the list (#66). Keys
    // typed before it has opened add to the search.
    function findTyped(text: string) {
        if (quickFindDialog.visible) {
            searchInput.insert(searchInput.cursorPosition, text);
            return;
        }
        quickFindDialog.typed = true;
        searchInput.text = text;
        quickFindDialog.open();
        searchInput.forceActiveFocus();
    }

    // What Quick Find lists for `text`: the fixed views named like it, the
    // areas and projects found, the tags named like it, then the tasks
    // found (#66). A name matches when it contains the text, in either
    // case.
    function findResults(text: string, found: var): var {
        if (text === "")
            return [];
        const typed = text.toLowerCase();
        const named = name => name.toLowerCase().indexOf(typed) >= 0;
        const views = fixedViews.filter(v => named(v.title)).map(v => ({ kind: "view", id: v.id, title: v.title, icon: v.icon, color: v.color }));
        const tags = allTags.filter(t => named(t.name)).map(t => ({ kind: "tag", id: t.id, title: t.name }));
        const lists = found.filter(r => r.kind !== "task");
        const tasks = found.filter(r => r.kind === "task");
        return views.concat(lists, tags, tasks);
    }

    // Open Quick Find result `index`: a view, area or project opens it; a
    // tag opens Anytime narrowed to it; a task opens the view that holds
    // it, with the task open and in view.
    function openSearchResult(index) {
        const results = searchResultsList.model;
        if (index < 0 || index >= results.length)
            return;
        const result = results[index];
        if (result.kind === "view") {
            controller.selectView(result.id);
        } else if (result.kind === "tag") {
            root.nextTagFilter = result.id;
            controller.selectView("anytime");
        } else if (result.kind === "area") {
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
        // Opened by typing on the list: the search is what was typed.
        property bool typed: false
        // The last search is kept, selected, so typing starts a new one;
        // after typing on the list, typing goes on after it.
        onOpened: {
            searchInput.forceActiveFocus();
            if (typed)
                searchInput.deselect();
            else
                searchInput.selectAll();
            typed = false;
        }
        onClosed: taskListView.forceActiveFocus()

        background: Rectangle {
            color: root.colCardBg
            radius: 10
            border.color: root.colBorder
            border.width: 1
        }

        contentItem: ColumnLayout {
            spacing: 12

            TextField {
                id: searchInput
                objectName: "searchInput"
                Accessible.name: qsTr("Quick Find")
                Layout.fillWidth: true
                placeholderText: qsTr("Find lists, areas, projects, tags and tasks...")
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
                    let found = [];
                    try {
                        found = JSON.parse(controller.searchResultsJson);
                    } catch (e) {}
                    return root.findResults(searchInput.text, found);
                }

                delegate: Rectangle {
                    id: resultItem
                    objectName: "searchResult"
                    required property var modelData
                    required property int index
                    readonly property string kind: modelData.kind
                    // What the result is, as its row and screen readers say:
                    // nothing for a task.
                    readonly property string kindName: ({ view: qsTr("List"), area: qsTr("Area"), project: qsTr("Project"), tag: qsTr("Tag") })[kind] ?? ""
                    width: searchResultsList.width
                    height: 36
                    radius: 6
                    color: resultItem.ListView.isCurrentItem ? root.colSidebarActive
                        : resMouse.containsMouse ? root.colSidebarHover : "transparent"
                    Accessible.role: Accessible.ListItem
                    Accessible.name: kindName !== "" ? qsTr("%1: %2", "the kind of a result, then its name").arg(kindName).arg(modelData.title) : modelData.title

                    RowLayout {
                        anchors.fill: parent
                        anchors.leftMargin: 10
                        anchors.rightMargin: 10
                        spacing: 8

                        // A view, area or project shows its sidebar icon, a
                        // tag the tag icon; a task, an empty checkbox like
                        // its row.
                        Item {
                            Layout.preferredWidth: 16
                            Layout.preferredHeight: 16
                            Icon {
                                anchors.centerIn: parent
                                visible: resultItem.kind !== "task"
                                name: resultItem.kind === "view" ? resultItem.modelData.icon
                                    : resultItem.kind === "project" ? "project"
                                    : resultItem.kind
                                size: 16
                                color: resultItem.kind === "view" ? resultItem.modelData.color
                                    : resultItem.kind === "project" ? root.colAccent
                                    : root.colTextSecondary
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
                            text: resultItem.kindName
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

    // Asks before opening a link that isn't a web or mail link (#63).
    Dialog {
        id: openLinkDialog
        objectName: "openLinkDialog"
        title: qsTr("Open this link?")
        modal: true
        x: (parent.width - width) / 2
        y: (parent.height - height) / 2
        width: 420
        property string url: ""

        function ask(link: string) {
            url = link;
            open();
        }

        Label {
            objectName: "openLinkUrl"
            width: parent.width
            wrapMode: Text.WrapAnywhere
            text: openLinkDialog.url
        }

        footer: DialogButtonBox {
            Button {
                objectName: "confirmOpenLink"
                text: qsTr("Open")
                DialogButtonBox.buttonRole: DialogButtonBox.AcceptRole
            }
            Button {
                text: qsTr("Cancel")
                DialogButtonBox.buttonRole: DialogButtonBox.RejectRole
            }
        }
        onAccepted: root.openExternally(url)
        onClosed: taskListView.forceActiveFocus()
    }

    // Tag several tasks at once (Ctrl+Shift+T on a selection, #64): type a
    // tag, existing or new; Enter gives it to every selected task.
    Popup {
        id: batchTagPopup
        objectName: "batchTagPopup"
        property var taskIds: []
        // The tags matching what is typed, as the tag field suggests them.
        readonly property var suggestions: {
            const typed = batchTagInput.text.trim().toLowerCase();
            return typed === "" ? [] : root.allTags.filter(t => t.name.toLowerCase().indexOf(typed) >= 0).slice(0, 6);
        }
        x: (parent.width - width) / 2
        y: 120
        width: 320
        modal: true
        focus: true
        onOpened: {
            batchTagInput.text = "";
            batchTagInput.forceActiveFocus();
        }
        onClosed: taskListView.forceActiveFocus()

        function commit() {
            const first = suggestions[0];
            const typed = batchTagInput.text.trim();
            const name = first && first.name.toLowerCase().indexOf(typed.toLowerCase()) === 0 ? first.name : typed;
            close();
            if (name === "")
                return;
            root.actOn(taskIds, id => controller.tagByName("task", id, name), n => qsTr("%n tasks tagged %1", "", n).arg(name));
        }

        background: Rectangle {
            color: root.colCardBg
            radius: 10
            border.color: root.colBorder
        }

        contentItem: ColumnLayout {
            spacing: 8
            Text {
                text: qsTr("Tag %n tasks", "", batchTagPopup.taskIds.length)
                font.pixelSize: 13
                font.bold: true
                color: root.colTextPrimary
            }
            TextField {
                id: batchTagInput
                objectName: "batchTagInput"
                Accessible.name: qsTr("Tag")
                Layout.fillWidth: true
                placeholderText: qsTr("Tag")
                Keys.onReturnPressed: batchTagPopup.commit()
                Keys.onEnterPressed: batchTagPopup.commit()
            }
            Repeater {
                model: batchTagPopup.suggestions
                delegate: Text {
                    required property var modelData
                    objectName: "batchTagSuggestion"
                    text: modelData.name
                    font.pixelSize: 12
                    color: root.colTextSecondary
                }
            }
        }
    }

    // Sync (#42), in plain words. A change saved here and not on the relay
    // is never called backed up.
    function syncStatusText() {
        if (!controller.syncAvailable)
            return qsTr("Sync is unavailable on this device")
        switch (controller.syncPhase) {
        case "off":
            return qsTr("Not syncing: everything is on this device only")
        case "joining":
            return qsTr("Waiting for the other device to add this one")
        case "recovering":
            return qsTr("Recovering from the recovery kit")
        }
        switch (controller.syncHealth) {
        case "up-to-date":
            return qsTr("Synced")
        case "syncing":
            return qsTr("Syncing")
        case "retrying":
            return qsTr("Can't reach the relay. Changes are saved on this device and will be sent later.")
        case "waiting-for-relay":
            return qsTr("Waiting for the relay to be set up for this collection")
        case "removed":
            return qsTr("This device was removed from sync. Its tasks stay here.")
        case "storage-full":
            return qsTr("The relay's storage is full. Changes are saved on this device only.")
        case "conflict":
            return qsTr("The relay refused a change. Changes are saved on this device only.")
        case "unsupported":
            return qsTr("The relay doesn't work with this version of Matters.")
        }
        return qsTr("Sync is starting")
    }

    function syncUnsentText() {
        if (controller.syncUnsent === 1)
            return qsTr("1 change is saved on this device and not yet on the relay.")
        return qsTr("%1 changes are saved on this device and not yet on the relay.").arg(controller.syncUnsent)
    }

    // Sync (#42): set up, add devices, recover, stop.
    Dialog {
        id: syncDialog
        objectName: "syncDialog"
        title: qsTr("Sync")
        modal: true
        x: (parent.width - width) / 2
        y: 60
        width: 480
        standardButtons: Dialog.Close
        onOpened: (controller.syncPhase === "off" && controller.syncAvailable
                   ? syncEndpointField : syncCloseFocus).forceActiveFocus(Qt.TabFocusReason)
        onClosed: taskListView.forceActiveFocus()

        ColumnLayout {
            id: syncCloseFocus
            width: parent.width
            spacing: 6
            activeFocusOnTab: false

            Label {
                objectName: "syncStatus"
                Layout.fillWidth: true
                wrapMode: Text.WordWrap
                text: root.syncStatusText()
                Accessible.name: text
            }
            Label {
                objectName: "syncUnsent"
                Layout.fillWidth: true
                wrapMode: Text.WordWrap
                visible: controller.syncPhase === "on" && controller.syncUnsent > 0
                text: root.syncUnsentText()
                color: root.colTextSecondary
            }
            Label {
                objectName: "syncError"
                Layout.fillWidth: true
                wrapMode: Text.WordWrap
                visible: controller.syncError !== ""
                text: controller.syncError
                color: root.colDanger
            }
            Label {
                objectName: "syncUnavailable"
                Layout.fillWidth: true
                wrapMode: Text.WordWrap
                visible: !controller.syncAvailable
                text: qsTr("Sync keeps this device's keys in the desktop's keyring, and none is running. Matters works on this device without it.")
            }

            // Not syncing: three ways in.
            ColumnLayout {
                visible: controller.syncAvailable && controller.syncPhase === "off"
                Layout.fillWidth: true
                spacing: 6

                Label {
                    Layout.fillWidth: true
                    wrapMode: Text.WordWrap
                    text: qsTr("Your tasks are encrypted on this device before they leave it. The relay stores them but can't read them; it can see how much you store and when you sync.")
                    color: root.colTextSecondary
                }
                Label {
                    text: qsTr("Relay address")
                    font.bold: true
                }
                TextField {
                    id: syncEndpointField
                    objectName: "syncEndpoint"
                    Layout.fillWidth: true
                    placeholderText: qsTr("https://relay.example.org")
                    text: controller.syncEndpoint
                    Accessible.name: qsTr("Relay address")
                    inputMethodHints: Qt.ImhUrlCharactersOnly
                }
                Button {
                    objectName: "syncCreate"
                    text: qsTr("Start syncing from this device")
                    enabled: syncEndpointField.text.length > 0
                    onClicked: controller.syncCreate(syncEndpointField.text)
                }

                Label {
                    Layout.topMargin: 10
                    text: qsTr("Join from another device")
                    font.bold: true
                }
                TextArea {
                    id: syncInvitationField
                    objectName: "syncInvitationField"
                    Layout.fillWidth: true
                    wrapMode: TextEdit.WrapAnywhere
                    placeholderText: qsTr("Paste the invitation the other device shows")
                    Accessible.name: qsTr("Invitation from the other device")
                }
                Button {
                    objectName: "syncJoin"
                    text: qsTr("Join")
                    enabled: syncEndpointField.text.length > 0 && syncInvitationField.text.length > 0
                    onClicked: controller.syncJoin(syncEndpointField.text, syncInvitationField.text)
                }

                Label {
                    Layout.topMargin: 10
                    text: qsTr("Recover after losing every device")
                    font.bold: true
                }
                TextField {
                    id: syncCollectionField
                    objectName: "syncCollectionField"
                    Layout.fillWidth: true
                    placeholderText: qsTr("Collection ID from the recovery kit")
                    Accessible.name: qsTr("Collection ID")
                }
                TextArea {
                    id: syncWordsField
                    objectName: "syncWordsField"
                    Layout.fillWidth: true
                    wrapMode: TextEdit.WordWrap
                    placeholderText: qsTr("The 24 recovery words")
                    Accessible.name: qsTr("Recovery words")
                }
                Button {
                    objectName: "syncRecover"
                    text: qsTr("Recover")
                    enabled: syncEndpointField.text.length > 0 && syncCollectionField.text.length > 0
                             && syncWordsField.text.length > 0
                    onClicked: controller.syncRecover(syncEndpointField.text, syncCollectionField.text,
                                                      syncWordsField.text)
                }
            }

            // A new collection the relay doesn't know yet.
            ColumnLayout {
                visible: controller.syncProvisionCommand !== "" && controller.syncHealth === "waiting-for-relay"
                Layout.fillWidth: true
                Label {
                    Layout.fillWidth: true
                    wrapMode: Text.WordWrap
                    text: qsTr("The relay's operator sets it up for this collection with:")
                }
                TextArea {
                    objectName: "syncProvisionCommand"
                    Layout.fillWidth: true
                    readOnly: true
                    selectByMouse: true
                    wrapMode: TextEdit.WrapAnywhere
                    text: controller.syncProvisionCommand
                    Accessible.name: qsTr("Relay setup command")
                }
            }

            // Syncing: the devices, adding one, stopping.
            Label {
                Layout.topMargin: 10
                visible: controller.syncPhase === "on"
                text: qsTr("Devices")
                font.bold: true
            }
            Repeater {
                model: controller.syncPhase === "on" ? JSON.parse(controller.syncDevicesJson) : []
                delegate: RowLayout {
                    required property var modelData
                    Layout.fillWidth: true
                    Label {
                        Layout.fillWidth: true
                        text: modelData.thisDevice
                              ? (modelData.admin ? qsTr("%1 (this device, admin)") : qsTr("%1 (this device)")).arg(modelData.short)
                              : (modelData.admin ? qsTr("%1 (admin)") : "%1").arg(modelData.short)
                    }
                    Button {
                        objectName: "syncRemove-" + modelData.short
                        visible: controller.syncAdmin && !modelData.thisDevice
                        text: qsTr("Remove")
                        Accessible.name: qsTr("Remove device %1").arg(modelData.short)
                        onClicked: controller.syncRemoveDevice(modelData.id)
                    }
                }
            }
            Button {
                objectName: "syncInvite"
                visible: controller.syncPhase === "on" && controller.syncAdmin && controller.syncInvitation === ""
                         && !controller.syncInviting
                text: qsTr("Add a device")
                onClicked: controller.syncInvite()
            }
            // The relay registers the invitation before it is shown (#45).
            RowLayout {
                visible: controller.syncInviting
                Layout.fillWidth: true
                Label {
                    objectName: "syncInviting"
                    Layout.fillWidth: true
                    wrapMode: Text.WordWrap
                    text: qsTr("Preparing the invitation with the relay…")
                }
                Button {
                    objectName: "syncCancelInviting"
                    text: qsTr("Cancel")
                    onClicked: controller.syncCancelInvitation()
                }
            }
            ColumnLayout {
                visible: controller.syncInvitation !== ""
                Layout.fillWidth: true
                Label {
                    Layout.fillWidth: true
                    wrapMode: Text.WordWrap
                    text: qsTr("On the new device, open Sync, choose Join and paste this invitation. It works once, within an hour. Anyone who sees it can ask to join, so share it only with your own device.")
                }
                TextArea {
                    id: syncInvitationText
                    objectName: "syncInvitation"
                    Layout.fillWidth: true
                    readOnly: true
                    selectByMouse: true
                    wrapMode: TextEdit.WrapAnywhere
                    text: controller.syncInvitation
                    Accessible.name: qsTr("Invitation for the new device")
                }
                RowLayout {
                    Button {
                        objectName: "syncCopyInvitation"
                        text: qsTr("Copy")
                        onClicked: {
                            syncInvitationText.selectAll()
                            syncInvitationText.copy()
                            syncInvitationText.deselect()
                        }
                    }
                    Button {
                        objectName: "syncCancelInvitation"
                        text: qsTr("Cancel")
                        onClicked: controller.syncCancelInvitation()
                    }
                }
            }
            Button {
                objectName: "syncLeave"
                Layout.topMargin: 10
                visible: controller.syncPhase !== "off"
                text: qsTr("Stop syncing on this device")
                onClicked: syncLeaveDialog.open()
            }
        }
    }

    // Stopping sync (#42): it keeps every task here; changes not on the
    // relay yet are said plainly, and kept only by an explicit choice.
    Dialog {
        id: syncLeaveDialog
        objectName: "syncLeaveDialog"
        title: qsTr("Stop syncing on this device?")
        modal: true
        x: (parent.width - width) / 2
        y: (parent.height - height) / 2
        width: 420
        onOpened: syncLeaveCancel.forceActiveFocus(Qt.TabFocusReason)

        ColumnLayout {
            width: parent.width
            Label {
                objectName: "syncLeaveText"
                Layout.fillWidth: true
                wrapMode: Text.WordWrap
                text: controller.syncUnsent > 0
                      ? qsTr("%1 Stopping now keeps them on this device only: your other devices will never get them.").arg(root.syncUnsentText())
                      : qsTr("Every task stays on this device. Your other devices keep syncing without it.")
            }
            RowLayout {
                Layout.alignment: Qt.AlignRight
                Button {
                    id: syncLeaveCancel
                    objectName: "syncLeaveCancel"
                    text: qsTr("Keep syncing")
                    onClicked: syncLeaveDialog.close()
                }
                Button {
                    objectName: "syncLeaveConfirm"
                    text: controller.syncUnsent > 0 ? qsTr("Stop and keep changes here only") : qsTr("Stop syncing")
                    onClicked: {
                        // Refused when changes not yet on the relay came in
                        // since the dialog opened: it stays, with their
                        // count, for the choice to be made again.
                        if (controller.syncLeave(controller.syncUnsent > 0) <= 0)
                            syncLeaveDialog.close()
                    }
                }
            }
        }
    }

    // The recovery kit (#42), shown once after starting a collection.
    Dialog {
        id: recoveryKitDialog
        objectName: "recoveryKitDialog"
        title: qsTr("Your recovery kit")
        modal: true
        closePolicy: Popup.NoAutoClose
        visible: controller.syncKitJson !== ""
        x: (parent.width - width) / 2
        y: 60
        width: 480
        readonly property var kit: controller.syncKitJson !== "" ? JSON.parse(controller.syncKitJson) : null
        onOpened: recoveryKitKept.forceActiveFocus(Qt.TabFocusReason)

        ColumnLayout {
            width: parent.width
            spacing: 6
            Label {
                Layout.fillWidth: true
                wrapMode: Text.WordWrap
                text: qsTr("Write these down and keep them somewhere safe. If every device is lost, they are the only way to get your tasks back. Nobody else holds them: not the relay's operator, and not Matters.")
            }
            Label {
                text: qsTr("Relay address")
                font.bold: true
            }
            TextArea {
                objectName: "recoveryKitEndpoint"
                Layout.fillWidth: true
                readOnly: true
                selectByMouse: true
                text: recoveryKitDialog.kit ? recoveryKitDialog.kit.endpoint : ""
                Accessible.name: qsTr("Relay address")
            }
            Label {
                text: qsTr("Collection ID")
                font.bold: true
            }
            TextArea {
                objectName: "recoveryKitCollection"
                Layout.fillWidth: true
                readOnly: true
                selectByMouse: true
                wrapMode: TextEdit.WrapAnywhere
                text: recoveryKitDialog.kit ? recoveryKitDialog.kit.collection : ""
                Accessible.name: qsTr("Collection ID")
            }
            Label {
                text: qsTr("Recovery words")
                font.bold: true
            }
            TextArea {
                objectName: "recoveryKitWords"
                Layout.fillWidth: true
                readOnly: true
                selectByMouse: true
                wrapMode: TextEdit.WordWrap
                text: recoveryKitDialog.kit ? recoveryKitDialog.kit.words : ""
                Accessible.name: qsTr("Recovery words")
            }
            Button {
                id: recoveryKitKept
                objectName: "recoveryKitKept"
                Layout.alignment: Qt.AlignRight
                text: qsTr("I've written them down")
                onClicked: controller.syncKitKept()
            }
        }
    }

    // Preferences (#72): this device's settings. Each takes effect at once.
    Dialog {
        id: preferencesDialog
        objectName: "preferencesDialog"
        title: qsTr("Preferences")
        modal: true
        x: (parent.width - width) / 2
        y: (parent.height - height) / 2
        width: 420
        standardButtons: Dialog.Close
        // The keyboard starts on the current choice.
        onOpened: (logPolicyGroup.checkedButton ?? whenLeavingButton).forceActiveFocus(Qt.TabFocusReason)
        onClosed: taskListView.forceActiveFocus()

        ColumnLayout {
            width: parent.width
            spacing: 4

            Label {
                text: qsTr("Move completed tasks to the Logbook")
                font.bold: true
            }

            ButtonGroup {
                id: logPolicyGroup
            }

            RadioButton {
                id: whenLeavingButton
                objectName: "logPolicy-when-leaving"
                text: qsTr("When I leave the list")
                ButtonGroup.group: logPolicyGroup
                checked: controller.logPolicy === "when-leaving"
                onClicked: controller.chooseLogPolicy("when-leaving")
            }
            RadioButton {
                objectName: "logPolicy-immediately"
                text: qsTr("Immediately")
                ButtonGroup.group: logPolicyGroup
                checked: controller.logPolicy === "immediately"
                onClicked: controller.chooseLogPolicy("immediately")
            }
            RadioButton {
                objectName: "logPolicy-daily"
                text: qsTr("At the end of the day")
                ButtonGroup.group: logPolicyGroup
                checked: controller.logPolicy === "daily"
                onClicked: controller.chooseLogPolicy("daily")
            }
            RadioButton {
                objectName: "logPolicy-manually"
                text: qsTr("When I choose Log Completed (Ctrl+Shift+L)")
                ButtonGroup.group: logPolicyGroup
                checked: controller.logPolicy === "manually"
                onClicked: controller.chooseLogPolicy("manually")
            }

            // Light or dark (#72).
            Label {
                Layout.topMargin: 12
                text: qsTr("Appearance")
                font.bold: true
            }
            ButtonGroup {
                id: colorSchemeGroup
            }
            Repeater {
                model: [
                    { key: "system", text: qsTr("Follow the system") },
                    { key: "light", text: qsTr("Light") },
                    { key: "dark", text: qsTr("Dark") }
                ]
                delegate: RadioButton {
                    required property var modelData
                    objectName: "colorScheme-" + modelData.key
                    text: modelData.text
                    ButtonGroup.group: colorSchemeGroup
                    checked: controller.colorScheme === modelData.key
                    onClicked: controller.chooseColorScheme(modelData.key)
                }
            }
            // The desktop's accent colour (#76), made readable on Matters'
            // surfaces.
            CheckBox {
                objectName: "followAccentBox"
                text: qsTr("Use the desktop's accent colour")
                enabled: controller.systemAccent !== ""
                checked: controller.followAccent
                onClicked: controller.chooseFollowAccent(checked)
            }
            Label {
                objectName: "noAccent"
                Layout.leftMargin: 24
                Layout.fillWidth: true
                visible: controller.systemAccent === ""
                text: qsTr("Your desktop doesn't offer an accent colour.")
                wrapMode: Text.WordWrap
                color: root.colTextSecondary
            }

            // Calendar events in Today and Upcoming (#62).
            Label {
                Layout.topMargin: 12
                text: qsTr("Calendar events")
                font.bold: true
            }
            CheckBox {
                id: showEventsBox
                objectName: "showEventsBox"
                text: qsTr("Show events in Today and Upcoming")
                checked: controller.showEvents
                onClicked: controller.chooseShowEvents(checked)
            }
            Repeater {
                model: root.calendars
                delegate: RowLayout {
                    required property var modelData
                    Layout.leftMargin: 24
                    spacing: 4
                    CheckBox {
                        objectName: "calendarBox-" + modelData.id
                        text: modelData.name
                        enabled: controller.showEvents
                        checked: !root.hiddenCalendars.includes(modelData.id)
                        onClicked: controller.showCalendar(modelData.id, checked)
                    }
                    // The calendar's colour, as its events show it.
                    Rectangle {
                        Layout.preferredWidth: 10
                        Layout.preferredHeight: 10
                        radius: 5
                        color: modelData.color !== "" ? modelData.color : root.colTextSecondary
                    }
                }
            }
            // Why there are no calendars to choose from.
            Label {
                objectName: "calendarProblem"
                Layout.fillWidth: true
                visible: text !== ""
                text: controller.calendarProblem !== "" ? controller.calendarProblem
                    : root.calendars.length === 0 ? qsTr("No calendars were found.") : ""
                wrapMode: Text.WordWrap
                color: root.colTextSecondary
            }
        }
    }

    // New Project Dialog
    Dialog {
        id: emptyTrashDialog
        objectName: "emptyTrashDialog"
        title: qsTr("Empty Trash?")
        modal: true
        x: (parent.width - width) / 2
        y: (parent.height - height) / 2
        width: 380

        Label {
            width: parent.width
            wrapMode: Text.WordWrap
            text: qsTr("Everything in the Trash will be permanently deleted, including the tasks in trashed projects. This can't be undone.")
        }

        footer: DialogButtonBox {
            Button {
                objectName: "confirmEmptyTrash"
                text: qsTr("Delete Permanently")
                DialogButtonBox.buttonRole: DialogButtonBox.AcceptRole
            }
            Button {
                objectName: "cancelEmptyTrash"
                text: qsTr("Cancel")
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

    // What happens to a project's open tasks when it is completed or
    // canceled (#55): they are completed or canceled with it, or the project
    // stays open.
    Dialog {
        id: finishProjectDialog
        objectName: "finishProjectDialog"
        // "completed" or "canceled": what the project becomes.
        property string outcome: "completed"
        readonly property int openTasks: controller.currentProjectOpenTasks
        title: outcome === "completed" ? qsTr("Complete “%1”?").arg(controller.currentTitle) : qsTr("Cancel “%1”?").arg(controller.currentTitle)
        modal: true
        x: (parent.width - width) / 2
        y: (parent.height - height) / 2
        width: 420

        Label {
            objectName: "finishProjectText"
            width: parent.width
            wrapMode: Text.WordWrap
            text: qsTr("It has %n open tasks. Complete or cancel them with the project?", "", finishProjectDialog.openTasks)
        }

        footer: DialogButtonBox {
            Button {
                objectName: "completeOpenTasks"
                text: qsTr("Complete Tasks")
                onClicked: finishProjectDialog.finishWith("completed")
            }
            Button {
                objectName: "cancelOpenTasks"
                text: qsTr("Cancel Tasks")
                onClicked: finishProjectDialog.finishWith("canceled")
            }
            Button {
                objectName: "keepProjectOpen"
                text: qsTr("Keep Open")
                DialogButtonBox.buttonRole: DialogButtonBox.RejectRole
            }
        }

        function finishWith(openTasks) {
            controller.finishProject(controller.currentProjectId, outcome, openTasks);
            close();
        }
        onClosed: taskListView.forceActiveFocus()
    }

    // Every tag (#57): renamed in place, nested under another tag or brought
    // to the top level, deleted (its items keep everything else), and new
    // ones added.
    Dialog {
        id: tagsDialog
        objectName: "tagsDialog"
        title: qsTr("Tags")
        modal: true
        x: (parent.width - width) / 2
        y: 80
        width: 420
        standardButtons: Dialog.Close
        onClosed: taskListView.forceActiveFocus()

        ColumnLayout {
            width: parent.width
            spacing: 6

            Label {
                visible: root.allTags.length === 0
                text: qsTr("No tags yet.")
                color: root.colTextSecondary
            }

            Repeater {
                model: root.allTags
                delegate: RowLayout {
                    id: tagRow
                    objectName: "tagRow"
                    required property var modelData
                    readonly property string tagId: modelData.id
                    readonly property string name: modelData.name
                    Layout.fillWidth: true
                    spacing: 6

                    TextField {
                        objectName: "tagName"
                        Layout.fillWidth: true
                        Layout.leftMargin: 16 * tagRow.modelData.depth
                        text: tagRow.name
                        font.pixelSize: 13
                        // A refused name (blank or taken) puts the old one back.
                        onEditingFinished: if (text !== tagRow.name && !controller.renameTag(tagRow.tagId, text)) text = tagRow.name
                    }

                    ToolButton {
                        objectName: "nestTag"
                        text: qsTr("Nest")
                        font.pixelSize: 11
                        ToolTip.visible: hovered
                        ToolTip.text: qsTr("Put this tag under another, or at the top")
                        onClicked: nestMenu.popup()

                        Menu {
                            id: nestMenu
                            Instantiator {
                                model: [{ id: "", name: qsTr("Top Level"), depth: 0 }].concat(
                                    root.allTags.filter(t => t.id !== tagRow.tagId))
                                delegate: MenuItem {
                                    required property var modelData
                                    objectName: "nestUnder-" + (modelData.id === "" ? "top" : modelData.id)
                                    text: modelData.name
                                    leftPadding: 12 + 12 * modelData.depth
                                    checkable: true
                                    checked: (tagRow.modelData.parent_id || "") === modelData.id
                                    onTriggered: controller.setTagParent(tagRow.tagId, modelData.id)
                                }
                                onObjectAdded: (index, object) => nestMenu.insertItem(index, object)
                                onObjectRemoved: (index, object) => nestMenu.removeItem(object)
                            }
                        }
                    }

                    ToolButton {
                        objectName: "deleteTag"
                        Accessible.name: qsTr("Delete the tag %1").arg(tagRow.name)
                        contentItem: Icon {
                            name: "trash"
                            size: 14
                            color: root.colTextSecondary
                        }
                        ToolTip.visible: hovered
                        ToolTip.text: qsTr("Delete the tag; what it was on is kept")
                        onClicked: controller.deleteTag(tagRow.tagId)
                    }
                }
            }

            TextField {
                objectName: "newTagInput"
                Accessible.name: qsTr("New tag")
                Layout.fillWidth: true
                placeholderText: qsTr("New tag")
                font.pixelSize: 13
                onAccepted: {
                    if (controller.createTag(text, "") !== "")
                        text = "";
                }
            }
        }
    }

    Dialog {
        id: newAreaDialog
        objectName: "newAreaDialog"
        title: qsTr("Create New Area")
        standardButtons: Dialog.Ok | Dialog.Cancel
        x: (parent.width - width) / 2
        y: (parent.height - height) / 2
        width: 360

        TextField {
            id: areaTitleInput
            objectName: "areaTitleInput"
            Accessible.name: qsTr("Area name")
            width: parent.width
            placeholderText: qsTr("Area name, e.g. Home or Work")
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
        title: kind === "project" ? qsTr("Rename Project")
            : kind === "heading" ? qsTr("Rename Heading")
            : kind === "newHeading" ? qsTr("New Heading") : qsTr("Rename Area")
        standardButtons: Dialog.Ok | Dialog.Cancel
        x: (parent.width - width) / 2
        y: (parent.height - height) / 2
        width: 360

        TextField {
            id: renameInput
            objectName: "renameInput"
            Accessible.name: renameDialog.kind === "area" ? qsTr("Area name") : renameDialog.kind === "project" ? qsTr("Project name") : qsTr("Heading")
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
        title: qsTr("Create New Project")
        standardButtons: Dialog.Ok | Dialog.Cancel
        x: (parent.width - width) / 2
        y: (parent.height - height) / 2
        width: 360

        ColumnLayout {
            spacing: 12
            TextField {
                id: projTitleInput
                objectName: "projectTitleInput"
                Accessible.name: qsTr("Project name")
                placeholderText: qsTr("Project Title...")
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
    // Task actions on the selection (#65); not in the Trash.
    Shortcut {
        sequence: "Ctrl+Alt+K"
        enabled: taskListView.selectedTaskId !== "" && controller.currentView !== "trash"
        onActivated: root.cancelOrReopen(taskListView.selectedTaskId)
    }
    Shortcut {
        sequence: "Ctrl+Shift+D"
        enabled: taskListView.selectedTaskId !== "" && controller.currentView !== "trash"
        onActivated: root.duplicateTask(taskListView.selectedTaskId)
    }
    Shortcut {
        sequences: ["Menu", "Shift+F10"]
        enabled: taskListView.selectedTaskId !== ""
        onActivated: {
            const row = root.rowItem(taskListView.selectedTaskId);
            if (row)
                root.openTaskMenu(taskListView.selectedTaskId, Qt.point(40, row.height), row);
        }
    }
    Shortcut {
        sequence: "Ctrl+,"
        onActivated: preferencesDialog.open()
    }
    Shortcut {
        sequence: "Ctrl+Shift+L"
        enabled: root.logsByHand
        onActivated: controller.logCompleted()
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
    Shortcut {
        sequence: "Ctrl+Shift+T"
        enabled: taskListView.selectedTaskId !== "" && controller.currentView !== "trash"
        onActivated: root.editSelectedTags()
    }
    // Trashed tasks can't be moved; Put Back first.
    Shortcut {
        sequence: "Ctrl+Shift+M"
        enabled: taskListView.selectedTaskId !== "" && controller.currentView !== "trash"
        onActivated: root.openMoveFor(taskListView.selectedTaskId)
    }
    // Text editors keep Ctrl+Z for their own undo.
    Shortcut {
        sequence: StandardKey.Undo
        enabled: root.undoKind !== ""
        onActivated: root.undo()
    }

    // Move popover (Ctrl+Shift+M; #54): files the task in the Inbox, an area,
    // a project or one of its headings, listed in sidebar order. Typing
    // filters the list (a project's headings match its name too); Up and
    // Down pick, Enter moves.
    Popup {
        id: movePopup
        objectName: "movePopup"
        property string taskId: ""
        // The tasks being moved: the selection the task is in (#64).
        property var taskIds: []
        property var destinations: []
        readonly property var shown: {
            const text = moveFilter.text.trim().toLowerCase();
            return text === "" ? destinations
                : destinations.filter(d => (d.title + " " + d.context).toLowerCase().indexOf(text) >= 0);
        }

        x: (parent.width - width) / 2
        y: 80
        width: Math.min(parent.width - 32, 380)
        padding: 12
        modal: true
        dim: false
        focus: true
        closePolicy: Popup.CloseOnEscape | Popup.CloseOnPressOutside

        onAboutToShow: {
            try {
                destinations = JSON.parse(controller.moveDestinations(taskId));
            } catch (e) {
                destinations = [];
            }
            moveFilter.text = "";
            moveList.currentIndex = 0;
        }
        onOpened: moveFilter.forceActiveFocus()
        onClosed: taskListView.forceActiveFocus()

        function choose(index) {
            const destination = shown[index];
            if (!destination)
                return;
            close();
            root.sendTasks(taskIds, destination.kind, destination.id);
        }

        background: Rectangle {
            color: root.colCardBg
            radius: 10
            border.color: root.colBorder
        }

        contentItem: ColumnLayout {
            spacing: 10

            Text {
                text: qsTr("Move")
                font.pixelSize: 13
                font.weight: Font.DemiBold
                color: root.colTextPrimary
            }

            TextField {
                id: moveFilter
                objectName: "moveFilter"
                Accessible.name: qsTr("Filter areas, projects and headings")
                Layout.fillWidth: true
                placeholderText: qsTr("Filter areas, projects and headings")
                font.pixelSize: 13
                onTextChanged: moveList.currentIndex = 0
                Keys.onUpPressed: moveList.decrementCurrentIndex()
                Keys.onDownPressed: moveList.incrementCurrentIndex()
                Keys.onReturnPressed: movePopup.choose(moveList.currentIndex)
                Keys.onEnterPressed: movePopup.choose(moveList.currentIndex)
            }

            // As tall as its entries, up to what fits below the popover's top.
            ListView {
                id: moveList
                objectName: "moveDestinations"
                Layout.fillWidth: true
                Layout.preferredHeight: Math.min(contentHeight, Math.max(120, root.height - 240))
                clip: true
                model: movePopup.shown
                highlightMoveDuration: 0

                delegate: Rectangle {
                    id: destinationRow
                    objectName: "moveDestination"
                    required property var modelData
                    required property int index
                    readonly property string kind: modelData.kind
                    width: moveList.width
                    height: 30
                    radius: 6
                    color: destinationRow.ListView.isCurrentItem ? root.colSidebarActive
                        : destinationMouse.containsMouse ? root.colSidebarHover : "transparent"
                    Accessible.role: Accessible.ListItem
                    // The Inbox's name is the window's to give (in the user's
                    // language); the rest are the user's own.
                    readonly property string title: kind === "inbox" ? qsTr("Inbox") : modelData.title
                    Accessible.name: kind === "area" ? qsTr("Area: %1").arg(title)
                        : kind === "project" ? qsTr("Project: %1").arg(title)
                        : kind === "heading" ? qsTr("Heading: %1").arg(title) : title

                    RowLayout {
                        anchors.fill: parent
                        // Filtered, the list is flat; otherwise it is laid
                        // out like the sidebar.
                        anchors.leftMargin: 8 + (moveFilter.text === "" ? destinationRow.modelData.depth * 18 : 0)
                        anchors.rightMargin: 8
                        spacing: 8

                        Item {
                            Layout.preferredWidth: 16
                            Layout.preferredHeight: 16
                            visible: destinationRow.kind !== "heading"
                            Icon {
                                anchors.centerIn: parent
                                name: destinationRow.kind
                                size: destinationRow.kind === "project" ? 13 : 16
                                color: destinationRow.kind === "inbox" ? root.viewColors.inbox
                                    : destinationRow.kind === "project" ? root.colAccent : root.colTextSecondary
                            }
                        }

                        Text {
                            text: destinationRow.title
                            font.pixelSize: 13
                            font.weight: destinationRow.kind === "area" || destinationRow.kind === "heading"
                                ? Font.DemiBold : Font.Normal
                            color: destinationRow.kind === "heading" ? root.colAccent : root.colTextPrimary
                            elide: Text.ElideRight
                            Layout.fillWidth: true
                        }

                        Text {
                            visible: moveFilter.text !== "" && destinationRow.modelData.context !== ""
                            text: destinationRow.modelData.context
                            font.pixelSize: 11
                            color: root.colTextSecondary
                            elide: Text.ElideRight
                            Layout.maximumWidth: 120
                        }

                        // Where the task is now.
                        Icon {
                            objectName: "moveCurrent"
                            visible: destinationRow.modelData.current
                            name: "check"
                            size: 14
                            color: root.colAccent
                        }
                    }

                    MouseArea {
                        id: destinationMouse
                        anchors.fill: parent
                        hoverEnabled: true
                        cursorShape: Qt.PointingHandCursor
                        onClicked: movePopup.choose(destinationRow.index)
                    }
                }
            }
        }
    }

    // How a task repeats (#61): every N days, weeks (on chosen days), months
    // (on a day, or a weekday of the month) or years; on a schedule or after
    // completion; until a day or for a number of times; paused.
    Dialog {
        id: repeatDialog
        objectName: "repeatDialog"
        title: qsTr("Repeat")
        modal: true
        x: (parent.width - width) / 2
        y: (parent.height - height) / 2
        width: 440
        standardButtons: Dialog.Ok | Dialog.Cancel

        // The tasks it applies to, and the day the schedule starts from.
        property var taskIds: []
        property date anchor: new Date()
        // Whether the tasks repeat already (Paused is offered then).
        property bool existing: false
        // Monday first; the locale's names (its Sunday is day 0).
        readonly property var weekdays: [1, 2, 3, 4, 5, 6, 0].map(d => Qt.locale().dayName(d, Locale.ShortFormat))
        readonly property var weekdayNames: [1, 2, 3, 4, 5, 6, 0].map(d => Qt.locale().dayName(d, Locale.LongFormat))
        // Monday is 1.
        readonly property int anchorWeekday: anchor.getDay() === 0 ? 7 : anchor.getDay()
        readonly property int anchorNth: Math.floor((anchor.getDate() - 1) / 7) + 1
        property var days: []

        function iso(d) {
            return Qt.formatDate(d, "yyyy-MM-dd");
        }

        // Open for tasks `ids`, with their rule `json` ("" for none), whose
        // date is `startDate` ("" for today).
        function edit(ids, json, startDate) {
            taskIds = ids;
            anchor = startDate !== "" ? root.parseLocal(startDate) : new Date();
            existing = json !== "";
            let rule = null;
            try {
                rule = json !== "" ? JSON.parse(json) : null;
            } catch (e) {}
            const kinds = ["daily", "weekly", "monthly_on_day", "monthly_on_weekday", "yearly"];
            const kind = rule ? rule.frequency.kind : "weekly";
            frequencyBox.currentIndex = rule ? [1, 2, 3, 3, 4][kinds.indexOf(kind)] : 0;
            everyBox.value = rule ? rule.every : 1;
            days = rule && kind === "weekly" ? rule.frequency.days.slice() : [anchorWeekday];
            monthlyBox.currentIndex = !rule ? 0
                : kind === "monthly_on_day" ? (rule.frequency.day === -1 ? 3 : 0)
                : kind === "monthly_on_weekday" ? (rule.frequency.nth === -1 ? 2 : 1) : 0;
            modeBox.currentIndex = rule && rule.after_completion ? 1 : 0;
            endsBox.currentIndex = !rule ? 0 : rule.until ? 1 : rule.left !== null && rule.left !== undefined ? 2 : 0;
            untilInput.text = rule && rule.until ? rule.until : "";
            countBox.value = rule && rule.left !== null && rule.left !== undefined ? rule.left + 1 : 5;
            pausedBox.checked = rule ? rule.paused === true : false;
            open();
        }

        // The rule as `RepeatRule` serializes it, or "" for Never.
        function ruleJson() {
            const f = frequencyBox.currentIndex;
            if (f === 0)
                return "";
            let frequency;
            if (f === 1)
                frequency = { kind: "daily" };
            else if (f === 2)
                frequency = { kind: "weekly", days: days.slice().sort() };
            else if (f === 3) {
                const m = monthlyBox.currentIndex;
                frequency = m === 0 ? { kind: "monthly_on_day", day: anchor.getDate() }
                    : m === 1 ? { kind: "monthly_on_weekday", nth: Math.min(anchorNth, 5), weekday: anchorWeekday }
                    : m === 2 ? { kind: "monthly_on_weekday", nth: -1, weekday: anchorWeekday }
                    : { kind: "monthly_on_day", day: -1 };
            } else
                frequency = { kind: "yearly", month: anchor.getMonth() + 1, day: anchor.getDate() };
            const until = endsBox.currentIndex === 1 ? controller.previewDeadline(untilInput.text, root.monthFirst) : "";
            return JSON.stringify({
                every: everyBox.value,
                frequency: frequency,
                after_completion: modeBox.currentIndex === 1,
                start: iso(anchor),
                until: until !== "" ? until : null,
                left: endsBox.currentIndex === 2 ? countBox.value - 1 : null,
                paused: existing && pausedBox.checked
            });
        }

        readonly property string json: ruleJson()
        readonly property bool usable: json === "" || controller.describeRepeat(json) !== ""
        function okButton() {
            return standardButton(Dialog.Ok);
        }
        // OK only for a rule that can be used.
        function updateOk() {
            const ok = okButton();
            if (ok)
                ok.enabled = usable;
        }
        onUsableChanged: updateOk()

        onAccepted: {
            const rule = json;
            if (!usable)
                return;
            root.actOn(taskIds, id => controller.setTaskRepeat(id, rule),
                n => rule === "" ? qsTr("%n tasks no longer repeat", "", n) : qsTr("%n tasks repeat", "", n));
        }
        onOpened: {
            updateOk();
            frequencyBox.forceActiveFocus();
        }
        onClosed: taskListView.forceActiveFocus()

        ColumnLayout {
            width: parent.width
            spacing: 8

            RowLayout {
                spacing: 8
                ComboBox {
                    id: frequencyBox
                    objectName: "repeatFrequency"
                    Accessible.name: qsTr("Repeats")
                    model: [qsTr("Never"), qsTr("Daily"), qsTr("Weekly"), qsTr("Monthly"), qsTr("Yearly")]
                }
                Label {
                    visible: frequencyBox.currentIndex > 0
                    text: qsTr("every")
                }
                SpinBox {
                    id: everyBox
                    objectName: "repeatEvery"
                    visible: frequencyBox.currentIndex > 0
                    Accessible.name: qsTr("Every")
                    from: 1
                    to: 999
                    editable: true
                }
                Label {
                    visible: frequencyBox.currentIndex > 0
                    // The unit after the count, in the plural form for it.
                    text: ["", qsTr("days", "after a count", everyBox.value), qsTr("weeks", "after a count", everyBox.value),
                        qsTr("months", "after a count", everyBox.value), qsTr("years", "after a count", everyBox.value)][frequencyBox.currentIndex]
                }
            }

            // Weekly: the days.
            RowLayout {
                visible: frequencyBox.currentIndex === 2 && modeBox.currentIndex === 0
                spacing: 2
                Repeater {
                    model: 7
                    delegate: Button {
                        required property int index
                        objectName: "repeatDay-" + (index + 1)
                        text: repeatDialog.weekdays[index]
                        checkable: true
                        checked: repeatDialog.days.includes(index + 1)
                        font.pixelSize: 11
                        implicitWidth: 52
                        onClicked: {
                            const d = index + 1;
                            const days = repeatDialog.days.filter(x => x !== d);
                            if (checked)
                                days.push(d);
                            repeatDialog.days = days;
                        }
                    }
                }
            }

            // Monthly: which day of the month.
            ComboBox {
                id: monthlyBox
                objectName: "repeatMonthly"
                visible: frequencyBox.currentIndex === 3 && modeBox.currentIndex === 0
                Layout.fillWidth: true
                Accessible.name: qsTr("On")
                model: [
                    qsTr("On day %1").arg(repeatDialog.anchor.getDate()),
                    [qsTr("On the first %1"), qsTr("On the second %1"), qsTr("On the third %1"), qsTr("On the fourth %1"),
                        qsTr("On the fifth %1")][Math.min(repeatDialog.anchorNth, 5) - 1]
                        .arg(repeatDialog.weekdayNames[repeatDialog.anchorWeekday - 1]),
                    qsTr("On the last %1").arg(repeatDialog.weekdayNames[repeatDialog.anchorWeekday - 1]),
                    qsTr("On the last day")
                ]
            }

            ComboBox {
                id: modeBox
                objectName: "repeatMode"
                visible: frequencyBox.currentIndex > 0
                Layout.fillWidth: true
                Accessible.name: qsTr("Counted")
                model: [qsTr("On a schedule"), qsTr("After each is completed")]
            }

            RowLayout {
                visible: frequencyBox.currentIndex > 0
                spacing: 8
                ComboBox {
                    id: endsBox
                    objectName: "repeatEnds"
                    Accessible.name: qsTr("Ends")
                    model: [qsTr("Never ends"), qsTr("Ends on a day"), qsTr("Ends after")]
                }
                TextField {
                    id: untilInput
                    objectName: "repeatUntil"
                    visible: endsBox.currentIndex === 1
                    Layout.fillWidth: true
                    placeholderText: qsTr("dec 24, in 3 months")
                    Accessible.name: qsTr("Ends on")
                }
                SpinBox {
                    id: countBox
                    objectName: "repeatCount"
                    visible: endsBox.currentIndex === 2
                    Accessible.name: qsTr("Times")
                    from: 1
                    to: 999
                    editable: true
                }
                Label {
                    visible: endsBox.currentIndex === 2
                    text: qsTr("times", "after a count", countBox.value)
                }
            }

            CheckBox {
                id: pausedBox
                objectName: "repeatPaused"
                visible: repeatDialog.existing && frequencyBox.currentIndex > 0
                text: qsTr("Paused")
            }

            Label {
                objectName: "repeatDescription"
                Layout.fillWidth: true
                wrapMode: Text.WordWrap
                text: repeatDialog.json === "" ? qsTr("Doesn't repeat")
                    : repeatDialog.usable ? controller.describeRepeat(repeatDialog.json)
                    : qsTr("Choose at least one day, and an end after the start")
                color: repeatDialog.usable ? root.colTextPrimary : root.colTextSecondary
            }
        }
    }

    // When popover for a task (Ctrl+S, Date…): quick choices, a typed date
    // with a preview of what it means, and a calendar (#59).
    Popup {
        id: whenPopup
        objectName: "whenPopup"
        // The task, or with `forProject` the project (#56), being dated.
        property string taskId: ""
        // The tasks being dated: the selection the task is in (#64).
        property var taskIds: []
        property bool forProject: false
        // "when" sets the When; "deadline" the deadline.
        property string mode: "when"
        // The day the calendar's keyboard cursor is on.
        property date cursor: new Date()
        // The task's reminder time (#60), `HH:MM` or "".
        property string reminder: ""
        readonly property bool remindable: mode === "when" && !forProject
        // The rule the task carries (#61), as JSON, or "".
        property string repeat: ""

        // A typed reminder time for the tasks; false when it is refused.
        function applyReminder(text) {
            const several = taskIds.length > 1;
            if (controller.previewReminder(text) === "")
                return false;
            if (several)
                controller.rememberTasks(JSON.stringify(taskIds));
            for (const id of taskIds)
                controller.setTaskReminderTyped(id, text);
            if (several)
                root.showUndo("batch", "", qsTr("Reminder set for %n tasks", "", taskIds.length));
            return true;
        }

        function clearReminder() {
            root.actOn(taskIds, id => controller.clearTaskReminder(id), n => qsTr("Reminder cleared for %n tasks", "", n));
        }

        // Set the When (a `Schedule::parse` key or ISO date) or, in deadline
        // mode, the deadline (an ISO date, or "" to clear it).
        function apply(value) {
            if (mode === "deadline") {
                if (forProject)
                    controller.setProjectDeadline(taskId, value);
                else
                    root.actOn(taskIds, id => controller.setTaskDeadline(id, value), n => qsTr("Deadline set for %n tasks", "", n));
            } else if (forProject) {
                controller.scheduleProject(taskId, value);
            } else {
                root.actOn(taskIds, id => controller.scheduleTask(id, value), n => qsTr("%n tasks scheduled", "", n));
            }
        }

        // Typed text; false when it is refused.
        function applyTyped(text) {
            if (forProject)
                return mode === "deadline" ? controller.setProjectDeadlineTyped(taskId, text, root.monthFirst)
                    : controller.scheduleProjectTyped(taskId, text, root.monthFirst);
            const typed = id => mode === "deadline" ? controller.setTaskDeadlineTyped(id, text, root.monthFirst)
                : controller.scheduleTaskTyped(id, text, root.monthFirst);
            // The first task says whether the text is understood; then the
            // rest follow (#64).
            const several = taskIds.length > 1;
            if (several)
                controller.rememberTasks(JSON.stringify(taskIds));
            if (!typed(taskIds[0]))
                return false;
            for (const id of taskIds.slice(1))
                typed(id);
            if (several)
                root.showUndo("batch", "", mode === "deadline" ? qsTr("Deadline set for %n tasks", "", taskIds.length)
                    : qsTr("%n tasks scheduled", "", taskIds.length));
            return true;
        }

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
            color: root.colCardBg
            radius: 10
            border.color: root.colBorder
        }

        onOpened: {
            whenInput.text = "";
            reminderInput.text = "";
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
                text: whenPopup.mode === "deadline" ? qsTr("Deadline") : qsTr("When")
                font.pixelSize: 15
                font.weight: Font.DemiBold
                color: root.colTextPrimary
            }

            TextField {
                id: whenInput
                objectName: "whenInput"
                Accessible.name: whenPopup.mode === "deadline" ? qsTr("Deadline") : qsTr("When")
                Layout.fillWidth: true
                placeholderText: whenPopup.mode === "deadline"
                    ? qsTr("Due: tomorrow, fri, dec 24")
                    : qsTr("Type a date: tomorrow, fri, dec 24")
                // Enter applies what was typed; a refused text keeps the
                // popover open.
                onAccepted: {
                    if (whenPopup.applyTyped(text))
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
                text: meaning === "" ? qsTr("Not a date") : root.describeWhen(meaning)
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
                        { label: qsTr("Today"), when: "today" },
                        { label: qsTr("This Evening"), when: "evening" },
                        { label: qsTr("Someday"), when: "someday" },
                        { label: qsTr("Clear"), when: "none" }
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
                                whenPopup.apply(modelData.when === "today" ? today : "");
                            } else {
                                whenPopup.apply(modelData.when);
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
                    Accessible.name: qsTr("Previous month")
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
                    text: root.localDate(new Date(calendar.year, calendar.month, 1), "month-year")
                    font.weight: Font.DemiBold
                    color: root.colTextPrimary
                }
                ToolButton {
                    objectName: "whenNextMonth"
                    Accessible.name: qsTr("Next month")
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
                        color: day.isCursor ? root.colOnAccent : (day.past ? root.colTextSecondary : root.colTextPrimary)
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

            // A reminder (#60): a time of day on the task's date. A task
            // with no date is scheduled for today.
            RowLayout {
                visible: whenPopup.remindable
                Layout.fillWidth: true
                spacing: 6

                Icon {
                    name: "bell"
                    size: 16
                    color: root.colTextSecondary
                }
                TextField {
                    id: reminderInput
                    objectName: "reminderInput"
                    Layout.fillWidth: true
                    placeholderText: whenPopup.reminder === ""
                        ? qsTr("Remind me at: 9:00, 2:30pm")
                        : qsTr("Reminder at %1").arg(root.describeTime(whenPopup.reminder))
                    Accessible.name: qsTr("Reminder time")
                    onAccepted: {
                        if (whenPopup.applyReminder(text))
                            whenPopup.close();
                    }
                    // Up returns to the calendar.
                    Keys.onUpPressed: calendar.forceActiveFocus()
                }
                Button {
                    objectName: "clearReminder"
                    visible: whenPopup.reminder !== ""
                    text: qsTr("Clear")
                    font.pixelSize: 11
                    onClicked: {
                        whenPopup.clearReminder();
                        whenPopup.close();
                    }
                }
            }
            // How it repeats (#61), changed in the Repeat dialog.
            RowLayout {
                visible: whenPopup.remindable
                Layout.fillWidth: true
                spacing: 6

                Icon {
                    name: "repeat"
                    size: 16
                    color: root.colTextSecondary
                }
                Label {
                    objectName: "repeatSummary"
                    Layout.fillWidth: true
                    text: whenPopup.repeat === "" ? qsTr("Doesn't repeat") : controller.describeRepeat(whenPopup.repeat)
                    elide: Text.ElideRight
                    color: whenPopup.repeat === "" ? root.colTextSecondary : root.colTextPrimary
                }
                Button {
                    objectName: "repeatButton"
                    text: qsTr("Repeat…")
                    font.pixelSize: 11
                    onClicked: {
                        const ids = whenPopup.taskIds;
                        const first = root.rowItem(whenPopup.taskId);
                        whenPopup.close();
                        repeatDialog.edit(ids, whenPopup.repeat, first ? first.startDate : "");
                    }
                }
            }

            Label {
                objectName: "reminderPreview"
                readonly property string meaning: controller.previewReminder(reminderInput.text)
                Layout.fillWidth: true
                visible: whenPopup.remindable && reminderInput.text.trim() !== ""
                text: meaning === "" ? qsTr("Not a time") : qsTr("Remind at %1").arg(root.describeTime(meaning))
                color: meaning === "" ? root.colTextSecondary : root.colAccent
                font.pixelSize: 12
            }
        }
    }
}
