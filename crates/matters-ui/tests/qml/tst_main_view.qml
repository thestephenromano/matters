import QtQuick
import QtTest
import matters

// Drives the real MainView with simulated input. Each test makes its own
// uniquely titled tasks, so tests do not depend on each other's data.
//
// The view is the visible root and the TestCase sits inside it: a TestCase is
// itself invisible, so items placed inside it would never receive input.
Item {
    width: 960
    height: 680

    MainView {
        id: view
        anchors.fill: parent
    }

    TestCase {
        id: testCase
        name: "MainView"
        when: windowShown

        readonly property var controller: view.appController
        readonly property var list: view.taskList

        function init() {
            view.hideUndo();
            controller.selectView("today");
            list.expandedTaskId = "";
            settle();
        }

        // Wait until no row is still growing or shrinking. Rows animate their
        // height, which moves the rows below them; a test that clicks before
        // they stop can hit the wrong place, depending on machine speed.
        function settle() {
            tryVerify(() => {
                for (let i = 0; i < list.count; ++i) {
                    const item = list.itemAtIndex(i);
                    if (item && item.height !== item.targetHeight)
                        return false;
                }
                return true;
            }, 2000, "rows finish animating");
        }

        function rowFor(title) {
            for (let i = 0; i < list.count; ++i) {
                const item = list.itemAtIndex(i);
                if (item && item.title === title)
                    return item;
            }
            return null;
        }

        function indexOf(title) {
            for (let i = 0; i < list.count; ++i) {
                const item = list.itemAtIndex(i);
                if (item && item.title === title)
                    return i;
            }
            return -1;
        }

        function waitForRow(title) {
            tryVerify(() => rowFor(title) !== null, 2000, "row '" + title + "' should be listed");
            return rowFor(title);
        }

        function waitForNoRow(title) {
            tryVerify(() => rowFor(title) === null, 2000, "row '" + title + "' should not be listed");
        }

        function addToToday(title) {
            controller.selectView("today");
            controller.addTask(title);
            return waitForRow(title);
        }

        // Click on the row's title area, which has no control of its own.
        function clickRowBody(row) {
            mouseClick(row, row.width / 2, 24);
        }

        // Expand a row the way a user does, and wait until it has finished
        // growing, so its notes and When buttons are on screen. Layouts update
        // before the next frame is drawn, so the row's full height is only
        // known once a frame has gone by; a real user can't click sooner.
        function expandRow(row) {
            settle();
            clickRowBody(row);
            verify(row.isExpanded);
            tryVerify(() => row.targetHeight > 48 && row.height === row.targetHeight, 2000,
                      "row finishes expanding");
        }

        function test_typing_a_task_adds_it_to_the_current_view() {
            const input = findChild(view, "newTaskInput");
            mouseClick(input);
            verify(input.activeFocus);
            for (const ch of "typedtask")
                keyClick(ch);
            keyClick(Qt.Key_Return);
            waitForRow("typedtask");
            compare(input.text, "");
        }

        function test_checkbox_click_completes_the_task_in_place() {
            const row = addToToday("tick-me");
            const before = controller.logbookCount;
            mouseClick(findChild(row, "checkCircle"));
            tryCompare(row, "done", true);
            compare(controller.logbookCount, before + 1, "badge counts update at once");
            compare(rowFor("tick-me"), row, "the ticked row stays in place");
            compare(list.expandedTaskId, "", "a checkbox click must not toggle expansion");

            mouseClick(findChild(row, "checkCircle"));
            tryCompare(row, "done", false);
            compare(controller.logbookCount, before);

            mouseClick(findChild(row, "checkCircle"));
            tryCompare(row, "done", true);
            controller.selectView("inbox");
            controller.selectView("today");
            waitForNoRow("tick-me");
        }

        function test_row_click_toggles_expansion() {
            const row = addToToday("expand-me");
            verify(!row.isExpanded);
            expandRow(row);
            clickRowBody(row);
            verify(!row.isExpanded);
            tryVerify(() => row.height === 48, 2000, "row finishes collapsing");
        }

        function test_delete_button_moves_the_task_to_the_trash() {
            const row = addToToday("delete-me");
            mouseMove(row, row.width / 2, 24);
            const button = findChild(row, "deleteButton");
            tryVerify(() => button.visible, 1000, "delete button shows on hover");
            mouseClick(button);
            waitForNoRow("delete-me");

            controller.selectView("trash");
            const trashed = waitForRow("delete-me");
            verify(!findChild(trashed, "deleteButton").visible, "no delete button in the Trash");
            mouseClick(findChild(trashed, "restoreButton"));
            waitForNoRow("delete-me");

            controller.selectView("today");
            waitForRow("delete-me");
        }

        function test_undo_puts_a_trashed_task_back() {
            const row = addToToday("oops");
            mouseMove(row, row.width / 2, 24);
            const button = findChild(row, "deleteButton");
            tryVerify(() => button.visible, 1000);
            mouseClick(button);
            waitForNoRow("oops");

            const toast = findChild(view, "undoToast");
            verify(toast.visible, "the undo notice shows");
            mouseClick(findChild(toast, "undoButton"));
            waitForRow("oops");
            verify(!toast.visible, "the notice goes away once used");
        }

        function projectIds(json) {
            return JSON.parse(json).map(p => p.id);
        }

        function test_project_moves_to_the_trash_with_its_tasks_and_back() {
            controller.createProject("Shed");
            const id = JSON.parse(controller.projectsJson).find(p => p.title === "Shed").id;
            controller.selectProject(id);
            controller.addTask("paint the shed");
            waitForRow("paint the shed");

            // Move to Trash leaves the project; Undo returns to it.
            mouseClick(findChild(view, "trashProjectButton"));
            compare(controller.currentView, "inbox");
            verify(projectIds(controller.projectsJson).indexOf(id) < 0, "gone from the sidebar");
            const toast = findChild(view, "undoToast");
            verify(toast.visible);
            mouseClick(findChild(toast, "undoButton"));
            compare(controller.currentView, "project");
            compare(controller.currentProjectId, id);
            waitForRow("paint the shed");

            // In the Trash the project stands for its tasks, and Put Back
            // restores both.
            mouseClick(findChild(view, "trashProjectButton"));
            controller.selectView("trash");
            const trashed = findChild(view, "trashedProject");
            verify(trashed !== null && trashed.visible, "the Trash lists the project");
            waitForNoRow("paint the shed"); // its tasks are not listed one by one
            verify(findChild(view, "emptyTrashButton").visible);
            mouseClick(findChild(trashed, "restoreProjectButton"));
            tryVerify(() => projectIds(controller.projectsJson).indexOf(id) >= 0, 1000, "back in the sidebar");
            controller.selectProject(id);
            waitForRow("paint the shed");
        }

        function test_empty_trash_asks_before_deleting_for_good() {
            addToToday("doomed");
            controller.trashTask(rowFor("doomed").taskId);
            controller.selectView("trash");
            waitForRow("doomed");

            const button = findChild(view, "emptyTrashButton");
            const dialog = findChild(view, "emptyTrashDialog");
            verify(button.visible);

            // Cancelling keeps the Trash as it was.
            mouseClick(button);
            tryVerify(() => dialog.opened, 1000, "confirmation opens");
            mouseClick(findChild(dialog.footer, "cancelEmptyTrash"));
            tryVerify(() => !dialog.visible, 1000);
            verify(rowFor("doomed") !== null, "cancel keeps the task");

            mouseClick(button);
            tryVerify(() => dialog.opened, 1000);
            mouseClick(findChild(dialog.footer, "confirmEmptyTrash"));
            waitForNoRow("doomed");
            tryVerify(() => !button.visible, 1000, "nothing left to empty");
            // The modal dialog takes key presses until its close transition
            // ends; later tests need the window back.
            tryVerify(() => !dialog.visible, 1000, "confirmation closes");
            tryVerify(() => list.activeFocus, 1000, "focus goes to the list, not the hidden button");
        }

        function test_when_button_reschedules_the_task() {
            const row = addToToday("defer-me");
            expandRow(row);
            mouseClick(findChild(row, "schedule-someday"));
            waitForNoRow("defer-me");
            controller.selectView("someday");
            waitForRow("defer-me");
        }

        function test_typing_notes_keeps_the_row_open_and_focused() {
            const row = addToToday("note-me");
            expandRow(row);
            const notes = findChild(row, "notesInput");
            mouseClick(notes);
            verify(notes.activeFocus);

            keyClick("a");
            keyClick("b");
            wait(900); // past the save debounce; the save reloads the list
            compare(row.notes, "ab", "notes were saved");
            compare(rowFor("note-me"), row, "the row's delegate survived the reload");
            verify(row.isExpanded);
            const focused = testCase.Window.window.activeFocusItem;
            verify(notes.activeFocus, "notes lost focus to " + focused + " (parent " + (focused ? focused.parent : null) + ")");

            keyClick("c");
            compare(notes.text, "abc");
            wait(900);
            compare(row.notes, "abc");
        }

        // Issue #1: rows changing elsewhere in the list must not take focus
        // from the notes being edited.
        function test_notes_keep_focus_when_other_rows_change() {
            const above = addToToday("focus-a");
            const row = addToToday("focus-b");
            verify(indexOf("focus-a") < indexOf("focus-b"), "focus-a is listed above focus-b");
            expandRow(row);
            const notes = findChild(row, "notesInput");
            mouseClick(notes);
            verify(notes.activeFocus);

            controller.trashTask(above.taskId); // a row above disappears
            waitForNoRow("focus-a");
            wait(50);
            const f = testCase.Window.window.activeFocusItem;
            verify(notes.activeFocus, "after a removal, focus is on " + f + (f === row ? " (the edited row)" : (f && f.title !== undefined ? " (row " + f.title + ")" : "")) + ", currentIndex " + list.currentIndex);

            controller.addTask("focus-c");
            waitForRow("focus-c");
            wait(50);
            verify(notes.activeFocus, "after an insertion, focus is on " + testCase.Window.window.activeFocusItem);
            keyClick("x");
            compare(notes.text, "x");
        }

        function test_quick_find_result_opens_the_containing_view() {
            controller.selectView("someday");
            controller.addTask("zebra-crossing");
            controller.selectView("today");
            waitForNoRow("zebra-crossing");

            keySequence("Ctrl+K");
            const dialog = findChild(view, "quickFindDialog");
            tryVerify(() => dialog.opened, 1000, "Ctrl+K opens Quick Find");
            for (const ch of "zebra")
                keyClick(ch);
            const results = findChild(view, "searchResults");
            tryVerify(() => results.count === 1, 1000, "one search result");
            mouseClick(results.itemAtIndex(0));

            tryCompare(controller, "currentView", "someday");
            const row = waitForRow("zebra-crossing");
            verify(row.isExpanded, "the revealed task is expanded");
            tryVerify(() => !dialog.visible, 1000, "Quick Find closes");
        }
    }
}
