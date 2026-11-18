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
            controller.selectView("today");
            list.expandedTaskId = "";
            wait(0);
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
            clickRowBody(row);
            verify(row.isExpanded);
            clickRowBody(row);
            verify(!row.isExpanded);
        }

        function test_delete_button_deletes_the_task() {
            const row = addToToday("delete-me");
            mouseMove(row, row.width / 2, 24);
            const button = findChild(row, "deleteButton");
            tryVerify(() => button.visible, 1000, "delete button shows on hover");
            mouseClick(button);
            waitForNoRow("delete-me");
        }

        function test_when_button_reschedules_the_task() {
            const row = addToToday("defer-me");
            clickRowBody(row);
            verify(row.isExpanded);
            mouseClick(findChild(row, "schedule-someday"));
            waitForNoRow("defer-me");
            controller.selectView("someday");
            waitForRow("defer-me");
        }

        function test_typing_notes_keeps_the_row_open_and_focused() {
            const row = addToToday("note-me");
            clickRowBody(row);
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
            clickRowBody(row);
            const notes = findChild(row, "notesInput");
            mouseClick(notes);
            verify(notes.activeFocus);

            controller.deleteTask(above.taskId); // a row above disappears
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
