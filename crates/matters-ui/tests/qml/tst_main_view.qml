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
            // Styles that animate popups (Material) may still be closing the
            // previous test's popover or dialog; while one is, it catches
            // clicks, and opening it again does nothing.
            for (const name of ["whenPopup", "newAreaDialog", "renameDialog", "newProjectDialog",
                                "emptyTrashDialog", "quickFindDialog", "movePopup",
                                "finishProjectDialog", "tagsDialog", "preferencesDialog",
                                "openLinkDialog"]) {
                const popup = findChild(view, name);
                if (popup)
                    tryVerify(() => !popup.visible, 2000, name + " has closed");
            }
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
            // Laid out (one pass), and, with motion on, done fading and
            // sliding (#77). Not waitForRendering: with nothing changed no new
            // frame comes, and it would wait out its whole timeout.
            wait(view.reduceMotion ? 30 : 260);
        }

        function rowFor(title) {
            for (let i = 0; i < list.count; ++i) {
                const item = list.itemAtIndex(i);
                if (item && item.title === title)
                    return item;
            }
            return null;
        }

        // Row of the task titled `title` in the model, whether or not its
        // delegate exists (rows far outside the view have none).
        function indexOf(title) {
            for (let i = 0; i < list.count; ++i) {
                if (list.model.titleAt(i) === title)
                    return i;
            }
            return -1;
        }

        // Wait for the task to be listed, scroll it into view as a user would,
        // and return its row.
        function waitForRow(title) {
            // Let a pending reload (after selectView, say) run first, so the
            // list isn't checked while it still shows the previous view.
            wait(0);
            tryVerify(() => indexOf(title) >= 0, 2000, "row '" + title + "' should be listed");
            list.positionViewAtIndex(indexOf(title), ListView.Contain);
            tryVerify(() => rowFor(title) !== null, 2000, "row '" + title + "' should be shown");
            settle();
            return rowFor(title);
        }

        function waitForNoRow(title) {
            wait(0);
            tryVerify(() => indexOf(title) < 0, 2000, "row '" + title + "' should not be listed");
        }

        // Capture a task the way the quick-add field does.
        function addToToday(title) {
            controller.selectView("today");
            view.captureTask(title);
            return waitForRow(title);
        }

        // Click on the row's title area, which has no control of its own.
        // Click the row's empty space, beside the title (which is a text field
        // while the row is open).
        function clickRowBody(row) {
            mouseClick(row, row.width * 0.75, 24);
        }

        // Expand a row the way a user does, and wait until it has finished
        // growing, so its notes and When buttons are on screen. Layouts update
        // before the next frame is drawn, so the row's full height is only
        // known once a frame has gone by; a real user can't click sooner.
        function expandRow(row) {
            // A user scrolls a row into view before clicking it.
            list.positionViewAtIndex(indexOf(row.title), ListView.Contain);
            settle();
            clickRowBody(row);
            verify(row.isExpanded);
            tryVerify(() => row.targetHeight > 48 && row.height === row.targetHeight, 2000,
                      "row finishes expanding");
            // Its notes and controls are laid out, and in view, before anyone
            // clicks them.
            settle();
        }

        function test_typing_a_task_adds_it_to_the_current_view() {
            const input = findChild(view, "newTaskInput");
            mouseClick(input);
            verify(input.activeFocus);
            for (const ch of "typedtask")
                keyClick(ch);
            keyClick(Qt.Key_Return);
            const row = waitForRow("typedtask");
            compare(input.text, "");

            // By now Today holds more tasks than fit; the new one is shown
            // and selected, not left below the fold.
            verify(list.contentHeight > list.height, "the list overflows");
            compare(list.selectedTaskId, row.taskId, "the new task is selected");
            tryVerify(() => row.y >= list.contentY && row.y + row.height <= list.contentY + list.height,
                      1000, "the new task is scrolled into view");
            verify(input.activeFocus, "typing can go on");
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
            menuAction("trashProjectItem");
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
            menuAction("trashProjectItem");
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

        // ---- Keyboard (SPEC §4) --------------------------------------------

        function selectedTitle() {
            return list.model.titleAt(list.model.rowOf(list.selectedTaskId));
        }

        // Give the task list keyboard focus with nothing selected.
        function focusList() {
            list.selectedTaskId = "";
            list.forceActiveFocus();
            verify(list.activeFocus);
        }

        function test_arrow_keys_move_the_selection() {
            addToToday("key-1");
            addToToday("key-2");
            addToToday("key-3");
            settle();
            focusList();

            keyClick(Qt.Key_Down);
            // Projects due today are listed above the tasks (#56).
            let first = 0;
            while (list.model.idAt(first) === "")
                ++first;
            compare(list.selectedTaskId, list.model.idAt(first), "Down selects the first task");
            focusList();
            keyClick(Qt.Key_Up);
            compare(selectedTitle(), "key-3", "Up selects the last task");
            keyClick(Qt.Key_Up);
            compare(selectedTitle(), "key-2");
            keyClick(Qt.Key_Down);
            keyClick(Qt.Key_Down);
            compare(selectedTitle(), "key-3", "the selection stops at the end");
        }

        function test_space_expands_and_escape_collapses_then_deselects() {
            const row = addToToday("space-me");
            settle();
            focusList();
            keyClick(Qt.Key_Up);
            compare(selectedTitle(), "space-me");

            keyClick(Qt.Key_Space);
            verify(row.isExpanded);
            keyClick(Qt.Key_Escape);
            verify(!row.isExpanded);
            compare(selectedTitle(), "space-me", "Escape first closes the row");
            keyClick(Qt.Key_Escape);
            compare(list.selectedTaskId, "", "then clears the selection");
        }

        function test_delete_key_trashes_the_selection_and_ctrl_z_undoes() {
            addToToday("del-a");
            addToToday("del-b");
            settle();
            focusList();
            keyClick(Qt.Key_Up);
            keyClick(Qt.Key_Up);
            compare(selectedTitle(), "del-a");

            keyClick(Qt.Key_Delete);
            waitForNoRow("del-a");
            compare(selectedTitle(), "del-b", "the next task is selected");
            verify(findChild(view, "undoToast").visible);

            keyClick(Qt.Key_Z, Qt.ControlModifier);
            waitForRow("del-a");
        }

        function test_typing_in_notes_does_not_trigger_list_keys() {
            const row = addToToday("typing");
            expandRow(row);
            const notes = findChild(row, "notesInput");
            mouseClick(notes);
            verify(notes.activeFocus);
            compare(list.selectedTaskId, row.taskId, "editing notes selects the task");

            keyClick("a");
            keyClick(Qt.Key_Space);
            keyClick(Qt.Key_Backspace);
            keyClick(Qt.Key_Down);
            compare(notes.text, "a", "the keys edited the notes");
            verify(rowFor("typing") !== null, "Backspace did not trash the task");
            verify(row.isExpanded, "Space did not close the row");

            // Escape leaves the editor and closes the row.
            keyClick(Qt.Key_Escape);
            verify(!row.isExpanded);
            verify(list.activeFocus, "focus is back on the list");
        }

        function test_ctrl_up_and_down_reorder_the_selected_task() {
            addToToday("ord-1");
            addToToday("ord-2");
            const row = addToToday("ord-3");
            settle();
            focusList();
            keyClick(Qt.Key_Up);
            compare(selectedTitle(), "ord-3");
            list.expandedTaskId = row.taskId;
            settle();

            keyClick(Qt.Key_Up, Qt.ControlModifier);
            tryVerify(() => indexOf("ord-3") === indexOf("ord-2") - 1, 2000, "moved above ord-2");
            keyClick(Qt.Key_Up, Qt.ControlModifier);
            tryVerify(() => indexOf("ord-3") === indexOf("ord-1") - 1, 2000, "moved above ord-1");
            compare(rowFor("ord-3"), row, "the row was moved, not recreated");
            verify(row.isExpanded, "and stays open");
            compare(list.selectedTaskId, row.taskId, "and selected");

            keyClick(Qt.Key_Down, Qt.ControlModifier);
            tryVerify(() => indexOf("ord-1") === indexOf("ord-3") - 1 && indexOf("ord-3") === indexOf("ord-2") - 1,
                      2000, "Ctrl+Down moves it back down");

            // Someday mixes areas and projects: not arranged by hand.
            controller.selectView("someday");
            view.captureTask("sd-1");
            view.captureTask("sd-2");
            waitForRow("sd-2");
            settle();
            focusList();
            keyClick(Qt.Key_Down);
            const before = [];
            for (let i = 0; i < list.count; ++i)
                before.push(list.model.idAt(i));
            keyClick(Qt.Key_Down, Qt.ControlModifier);
            wait(100);
            for (let i = 0; i < list.count; ++i)
                compare(list.model.idAt(i), before[i], "Someday keeps its order");
            compare(controller.errorMessage, "", "and shows no error");
        }

        // ---- Drag and drop (#53) ------------------------------------------

        // Drag `grip` with the mouse to `to` (a point in `view`), in steps, as
        // a user would; `during` runs while the button is still held.
        function dragGripTo(grip, to, during) {
            const from = grip.mapToItem(view, grip.width / 2, grip.height / 2);
            mousePress(view, from.x, from.y);
            const steps = 12;
            for (let i = 1; i <= steps; ++i)
                mouseMove(view, from.x, from.y + (to.y - from.y) * i / steps, 10, Qt.LeftButton);
            if (during)
                during();
            mouseRelease(view, from.x, to.y);
        }

        function test_dragging_a_task_by_its_grip_reorders_it() {
            addToToday("drag-1");
            addToToday("drag-2");
            const row = addToToday("drag-3");
            settle();
            const grip = findChild(row, "dragGrip");
            verify(grip.visible, "Today is arranged by hand");

            // Drop onto the upper half of drag-1: drag-3 goes before it.
            const target = rowFor("drag-1");
            const to = target.mapToItem(view, target.width / 2, 10);
            const indicator = findChild(view, "dropIndicator");
            dragGripTo(grip, to, () => verify(indicator.visible, "a line shows where it will land"));
            tryVerify(() => indexOf("drag-3") === indexOf("drag-1") - 1, 2000, "drag-3 moved above drag-1");
            verify(!indicator.visible);
            compare(rowFor("drag-3"), row, "the row was moved, not recreated");
            compare(list.selectedTaskId, row.taskId, "the dragged task is selected");

            // A drag that ends where it started changes nothing.
            settle();
            const order = [];
            for (let i = 0; i < list.count; ++i)
                order.push(list.model.idAt(i));
            const here = row.mapToItem(view, row.width / 2, row.height / 2 + 4);
            dragGripTo(findChild(row, "dragGrip"), here);
            wait(100);
            for (let i = 0; i < list.count; ++i)
                compare(list.model.idAt(i), order[i], "unchanged");
        }

        // Today with one more daytime task and two evening tasks at the end:
        // the reported case, where the daytime task could not be moved.
        function eveningFixture(prefix) {
            addToToday(prefix + "-anchor");
            const day = addToToday(prefix + "-day");
            for (const name of [prefix + "-e1", prefix + "-e2"]) {
                view.captureTask(name);
                waitForRow(name);
                controller.scheduleTask(rowFor(name).taskId, "evening");
            }
            tryVerify(() => indexOf(prefix + "-e2") === list.count - 1
                      && indexOf(prefix + "-e1") === list.count - 2, 2000, "evening tasks last");
            settle();
            return day;
        }

        function clearEvening(prefix) {
            for (const name of [prefix + "-anchor", prefix + "-day", prefix + "-e1", prefix + "-e2"]) {
                const row = rowFor(name);
                if (row)
                    controller.scheduleTask(row.taskId, "someday");
            }
            waitForNoRow(prefix + "-e2");
        }

        // Section headings are children of the list's content item, which
        // findChild does not search. The list may still hold a heading it is
        // about to recycle, so callers look for one in the right place.
        function eveningHeadings() {
            const found = [];
            for (const child of list.contentItem.children) {
                if (child.objectName === "eveningHeading" && child.visible)
                    found.push(child);
            }
            return found;
        }

        function test_today_shows_a_this_evening_heading() {
            eveningFixture("head");
            const day = rowFor("head-day");
            const e1 = rowFor("head-e1");
            tryVerify(() => eveningHeadings().some(h => h.y > day.y && h.y < e1.y), 2000,
                      "a This Evening heading between the daytime and evening tasks");
            clearEvening("head");
            controller.selectView("inbox");
            tryVerify(() => eveningHeadings().length === 0, 2000, "no heading outside Today");
        }

        function test_ctrl_down_moves_the_last_daytime_task_into_this_evening() {
            const day = eveningFixture("cross");
            list.selectedTaskId = day.taskId;
            list.forceActiveFocus();

            keyClick(Qt.Key_Down, Qt.ControlModifier);
            tryVerify(() => day.evening, 2000, "now scheduled for This Evening");
            compare(indexOf("cross-day"), indexOf("cross-e1") - 1, "at the top of This Evening");
            keyClick(Qt.Key_Down, Qt.ControlModifier);
            tryVerify(() => indexOf("cross-day") === indexOf("cross-e1") + 1, 2000, "and on down through it");

            keyClick(Qt.Key_Up, Qt.ControlModifier);
            keyClick(Qt.Key_Up, Qt.ControlModifier);
            tryVerify(() => !day.evening, 2000, "back among the daytime tasks");
            compare(list.selectedTaskId, day.taskId, "still selected");
            clearEvening("cross");
        }

        function test_dragging_into_this_evening_moves_the_task_there() {
            const day = eveningFixture("dnd");
            // Bring the evening tasks into view, as a user would scroll.
            list.positionViewAtIndex(indexOf("dnd-e2"), ListView.Contain);
            settle();
            // Drop on the lower half of the first evening task.
            const e1 = rowFor("dnd-e1");
            const to = e1.mapToItem(view, e1.width / 2, e1.height - 6);
            dragGripTo(findChild(day, "dragGrip"), to,
                       () => verify(findChild(view, "dropIndicator").visible, "a slot is offered"));
            tryVerify(() => day.evening && indexOf("dnd-day") === indexOf("dnd-e1") + 1, 2000,
                      "after dnd-e1, in This Evening");

            // And back: onto the lower half of the last daytime task.
            settle();
            const lastDay = rowFor("dnd-anchor");
            const back = lastDay.mapToItem(view, lastDay.width / 2, lastDay.height - 6);
            dragGripTo(findChild(day, "dragGrip"), back);
            tryVerify(() => !day.evening && indexOf("dnd-day") === indexOf("dnd-anchor") + 1, 2000,
                      "back among the daytime tasks, after dnd-anchor");
            compare(controller.errorMessage, "");
            clearEvening("dnd");
        }

        function test_no_grip_where_the_list_has_its_own_order() {
            controller.selectView("someday");
            view.captureTask("sd-grip");
            const row = waitForRow("sd-grip");
            verify(!findChild(row, "dragGrip").enabled, "Someday is not arranged by hand");
        }

        function test_dragging_a_project_in_the_sidebar_reorders_it() {
            controller.createProject("proj-a");
            controller.createProject("proj-b");
            const ids = () => JSON.parse(controller.projectsJson).map(p => p.title);
            tryVerify(() => ids().indexOf("proj-b") === ids().indexOf("proj-a") + 1, 2000, "new projects go last");

            const projects = findChild(view, "projectsList");
            // A user scrolls the sidebar to them first; other tests may have
            // filled it.
            projects.positionViewAtIndex(projects.model.findIndex(p => p.title === "proj-b"), ListView.Contain);
            wait(50);
            let a = null, b = null;
            tryVerify(() => {
                for (let i = 0; i < projects.count; ++i) {
                    const item = projects.itemAtIndex(i);
                    if (item && item.modelData.title === "proj-a") a = item;
                    if (item && item.modelData.title === "proj-b") b = item;
                }
                return a !== null && b !== null;
            }, 2000);
            const to = a.mapToItem(view, a.width / 2, 4);
            dragGripTo(findChild(b, "projectGrip"), to);
            tryVerify(() => ids().indexOf("proj-b") === ids().indexOf("proj-a") - 1, 2000, "proj-b moved above proj-a");
        }

        function test_no_new_tasks_in_the_logbook_or_the_trash() {
            const bar = findChild(view, "quickAddBar");
            const input = findChild(view, "newTaskInput");
            for (const name of ["logbook", "trash"]) {
                controller.selectView(name);
                verify(!bar.visible, "no quick-add field in " + name);
                focusList();
                keyClick(Qt.Key_N, Qt.ControlModifier);
                verify(!input.activeFocus, "Ctrl+N does nothing in " + name);
                compare(controller.addTask("not-here"), "", "nothing is captured in " + name);
            }
            controller.selectView("inbox");
            verify(bar.visible, "the field is back in the Inbox");
        }

        // Reorder the top task of a short, fresh list (issue report: with three
        // tasks, the one on top could not be reordered).
        function openFreshProject(name, titles) {
            controller.createProject(name);
            const id = JSON.parse(controller.projectsJson).find(p => p.title === name).id;
            controller.selectProject(id);
            for (const t of titles)
                view.captureTask(t);
            waitForRow(titles[titles.length - 1]);
            settle();
        }

        function titlesInOrder() {
            const out = [];
            for (let i = 0; i < list.count; ++i)
                out.push(list.model.titleAt(i));
            return out.join(",");
        }

        function test_top_task_moves_down_with_ctrl_down() {
            openFreshProject("top-keys", ["t1", "t2", "t3"]);
            focusList();
            keyClick(Qt.Key_Down);
            compare(selectedTitle(), "t1");
            keyClick(Qt.Key_Down, Qt.ControlModifier);
            tryVerify(() => titlesInOrder() === "t2,t1,t3", 2000, "t1 moved down: " + titlesInOrder());
        }

        function test_top_task_moves_down_by_dragging() {
            openFreshProject("top-drag", ["d1", "d2", "d3"]);
            const top = rowFor("d1");
            const last = rowFor("d3");
            // Drop below the middle of the last row: d1 goes to the end.
            const to = last.mapToItem(view, last.width / 2, last.height - 6);
            dragGripTo(findChild(top, "dragGrip"), to,
                       () => verify(findChild(view, "dropIndicator").visible, "a slot is offered"));
            tryVerify(() => titlesInOrder() === "d2,d3,d1", 2000, "d1 moved to the end: " + titlesInOrder());

            // And from the top to the middle.
            settle();
            const nowTop = rowFor("d2");
            const mid = rowFor("d3");
            const to2 = mid.mapToItem(view, mid.width / 2, mid.height - 6);
            dragGripTo(findChild(nowTop, "dragGrip"), to2);
            tryVerify(() => titlesInOrder() === "d3,d2,d1", 2000, "d2 moved below d3: " + titlesInOrder());
        }

        // ---- Markdown notes (#63) ---------------------------------------------

        // A Today task with `notes`, open.
        function openWithNotes(title, notes) {
            const row = addToToday(title);
            controller.updateTaskNotes(row.taskId, notes);
            expandRow(row);
            const view = findChild(row, "notesView");
            tryVerify(() => view.visible, 1000, "the notes are shown formatted");
            return row;
        }

        // Click at character `index` of the formatted notes.
        function clickShown(notesView, index) {
            const r = notesView.positionToRectangle(index);
            mouseClick(notesView, r.x + 1, r.y + r.height / 2);
        }

        function test_notes_are_shown_formatted_and_edited_as_markdown() {
            const row = openWithNotes("md-format", "**bold** and *slanted*");
            const notesView = findChild(row, "notesView");
            const notes = findChild(row, "notesInput");
            compare(notesView.getText(0, notesView.length), "bold and slanted", "no Markdown syntax shown");
            verify(!notes.activeFocus);

            // A click before "and" edits the source there.
            clickShown(notesView, 5);
            tryVerify(() => notes.activeFocus, 1000, "a click edits the notes");
            verify(!notesView.visible, "the source shows while editing");
            compare(notes.cursorPosition, 9, "the caret is where the click was, in the source");

            // Typing autosaves as before.
            typeText("x ");
            wait(900);
            compare(row.notes, "**bold** x and *slanted*");
            // Leaving the notes (here for the title) shows them formatted.
            mouseClick(findChild(row, "titleInput"));
            tryVerify(() => notesView.visible, 1000);
            compare(notesView.getText(0, notesView.length), "bold x and slanted");
        }

        function test_empty_notes_are_edited_directly() {
            const row = addToToday("md-empty");
            expandRow(row);
            verify(!findChild(row, "notesView").visible, "nothing to format");
            const notes = findChild(row, "notesInput");
            mouseClick(notes);
            verify(notes.activeFocus);
        }

        function test_web_and_mail_links_open_and_others_ask_first() {
            const opened = [];
            const original = view.openExternally;
            view.openExternally = url => opened.push(url);
            try {
                const row = openWithNotes("md-links",
                    "[site](https://example.com/a) [mail](mailto:me@example.com) [file](file:///tmp/x) and https://example.org/bare");
                const notesView = findChild(row, "notesView");
                const notes = findChild(row, "notesInput");
                const shown = notesView.getText(0, notesView.length);
                const at = word => shown.indexOf(word) + 1;

                clickShown(notesView, at("site"));
                compare(opened, ["https://example.com/a"]);
                verify(!notes.activeFocus, "a link opens; it doesn't edit");
                clickShown(notesView, at("mail"));
                compare(opened[1], "mailto:me@example.com");

                // A bare URL is a link too.
                clickShown(notesView, shown.indexOf("example.org") + 2);
                compare(opened[2], "https://example.org/bare");

                // Another scheme asks first.
                const dialog = findChild(view, "openLinkDialog");
                clickShown(notesView, at("file"));
                tryVerify(() => dialog.opened, 1000, "asks before opening");
                compare(findChild(dialog, "openLinkUrl").text, "file:///tmp/x");
                compare(opened.length, 3, "not opened yet");
                keyClick(Qt.Key_Escape);
                tryVerify(() => !dialog.visible, 2000);
                compare(opened.length, 3, "Cancel opens nothing");

                clickShown(notesView, at("file"));
                tryVerify(() => dialog.opened, 1000);
                mouseClick(findChild(dialog, "confirmOpenLink"));
                tryVerify(() => !dialog.visible, 2000);
                compare(opened[3], "file:///tmp/x", "opened once agreed");
            } finally {
                view.openExternally = original;
            }
        }

        function test_closed_rows_with_notes_show_a_marker() {
            const plain = addToToday("md-no-notes");
            const noted = addToToday("md-has-notes");
            controller.updateTaskNotes(noted.taskId, "something");
            tryVerify(() => findChild(rowFor("md-has-notes"), "notesMarker").visible, 1000);
            verify(!findChild(rowFor("md-no-notes"), "notesMarker").visible);
            expandRow(rowFor("md-has-notes"));
            verify(!findChild(rowFor("md-has-notes"), "notesMarker").visible, "not while open");
        }

        // ---- Titles (#27) ---------------------------------------------------

        function typeText(text) {
            for (const c of text)
                keyClick(c);
        }

        function test_title_is_edited_in_place() {
            const row = addToToday("old-title");
            expandRow(row);
            const input = findChild(row, "titleInput");
            verify(input.visible, "an open task's title is a field");
            mouseClick(input);
            verify(input.activeFocus);
            compare(list.selectedTaskId, row.taskId, "editing the title selects the task");
            keyClick(Qt.Key_A, Qt.ControlModifier);
            typeText("new title");
            keyClick(Qt.Key_Return);
            verify(findChild(row, "notesInput").activeFocus, "Enter goes on to the notes");
            tryVerify(() => row.title === "new title", 2000, "saved");
            compare(rowFor("new title"), row, "the same row, updated in place");
            verify(row.isExpanded, "and still open");
        }

        function test_escape_or_a_blank_title_keeps_the_old_name() {
            const row = addToToday("keep-me");
            expandRow(row);
            const input = findChild(row, "titleInput");

            mouseClick(input);
            keyClick(Qt.Key_A, Qt.ControlModifier);
            typeText("oops");
            keyClick(Qt.Key_Escape);
            compare(input.text, "keep-me", "Escape abandons the edit");
            verify(list.activeFocus, "focus is back on the list");
            verify(row.isExpanded, "the row stays open");

            mouseClick(input);
            keyClick(Qt.Key_A, Qt.ControlModifier);
            keyClick(Qt.Key_Delete);
            keyClick(Qt.Key_Return);
            compare(input.text, "keep-me", "a blank title snaps back");
            wait(100);
            compare(row.title, "keep-me", "and nothing was saved");
            compare(controller.errorMessage, "");
        }

        function test_f2_edits_the_selected_title() {
            const row = addToToday("f2-me");
            settle();
            focusList();
            keyClick(Qt.Key_Up);
            compare(selectedTitle(), "f2-me");
            keyClick(Qt.Key_F2);
            const input = findChild(row, "titleInput");
            tryVerify(() => input.activeFocus, 2000, "F2 opens the task with its title focused");
            verify(row.isExpanded);
            compare(input.selectedText, "f2-me", "the whole title is selected");
            typeText("renamed");
            keyClick(Qt.Key_Return);
            tryVerify(() => row.title === "renamed", 2000);
        }

        // ---- Enter: a new task below the selection --------------------------

        function newBelowField() {
            for (let i = 0; i < list.count; ++i) {
                const item = list.itemAtIndex(i);
                const field = item ? findChild(item, "newBelowInput") : null;
                if (field && field.visible)
                    return field;
            }
            return null;
        }

        function test_enter_types_new_tasks_below_the_selection() {
            openFreshProject("enter-proj", ["p-a", "p-b"]);
            focusList();
            keyClick(Qt.Key_Down);
            compare(selectedTitle(), "p-a");
            const before = list.count;

            keyClick(Qt.Key_Return);
            tryVerify(() => newBelowField() !== null && newBelowField().activeFocus, 2000, "a field opens under p-a");
            typeText("p-new1");
            keyClick(Qt.Key_Return);
            tryVerify(() => titlesInOrder() === "p-a,p-new1,p-b", 2000, "added right below: " + titlesInOrder());
            tryVerify(() => newBelowField() !== null && newBelowField().activeFocus, 2000, "the field moves on");
            typeText("p-new2");
            keyClick(Qt.Key_Return);
            tryVerify(() => titlesInOrder() === "p-a,p-new1,p-new2,p-b", 2000, titlesInOrder());

            // An empty Enter closes the field without creating anything.
            tryVerify(() => newBelowField() !== null && newBelowField().activeFocus, 2000);
            keyClick(Qt.Key_Return);
            tryVerify(() => newBelowField() === null, 2000, "closed");
            compare(list.count, before + 2, "no blank task");
            verify(list.activeFocus);
            compare(selectedTitle(), "p-new2");
        }

        function test_escape_closes_the_new_task_field_without_adding() {
            openFreshProject("esc-proj", ["q-a"]);
            focusList();
            keyClick(Qt.Key_Down);
            keyClick(Qt.Key_Return);
            tryVerify(() => newBelowField() !== null && newBelowField().activeFocus, 2000);
            typeText("never");
            keyClick(Qt.Key_Escape);
            tryVerify(() => newBelowField() === null, 2000);
            wait(100);
            compare(titlesInOrder(), "q-a", "nothing was added");
            verify(list.activeFocus);
        }

        function test_enter_elsewhere_goes_to_the_quick_add_field() {
            controller.selectView("someday");
            view.captureTask("sd-enter");
            waitForRow("sd-enter");
            settle();
            focusList();
            keyClick(Qt.Key_Down);
            keyClick(Qt.Key_Return);
            verify(findChild(view, "newTaskInput").activeFocus, "Someday is not arranged by hand");
            verify(newBelowField() === null);
            keyClick(Qt.Key_Escape);
        }

        // ---- When popover (#59) --------------------------------------------

        function isoInDays(days) {
            const d = new Date();
            d.setHours(0, 0, 0, 0);
            d.setDate(d.getDate() + days);
            return Qt.formatDate(d, "yyyy-MM-dd");
        }

        function selectLast(title) {
            settle();
            focusList();
            keyClick(Qt.Key_Up);
            compare(selectedTitle(), title);
        }

        function openWhen() {
            const popup = findChild(view, "whenPopup");
            keyClick(Qt.Key_S, Qt.ControlModifier);
            tryVerify(() => popup.opened, 1000, "Ctrl+S opens the When popover");
            waitForRendering(popup.contentItem); // laid out before clicking in it
            return popup;
        }

        function test_ctrl_s_opens_the_when_popover_for_the_selection() {
            addToToday("menu-me");
            selectLast("menu-me");
            const popup = openWhen();
            verify(findChild(popup.contentItem, "whenInput").activeFocus, "ready for a typed date");
            mouseClick(findChild(popup.contentItem, "when-someday"));
            waitForNoRow("menu-me");
            tryVerify(() => !popup.visible, 1000);
            tryVerify(() => list.activeFocus, 1000, "focus returns to the list");
            controller.selectView("someday");
            waitForRow("menu-me");
        }

        function test_typing_a_date_shows_what_it_means_and_schedules() {
            const row = addToToday("typed-when");
            selectLast("typed-when");
            const popup = openWhen();
            const preview = findChild(popup.contentItem, "whenPreview");
            typeText("tomorrow");
            verify(preview.visible && preview.text !== "Not a date", "a preview: " + preview.text);
            keyClick(Qt.Key_Return);
            tryVerify(() => !popup.visible, 1000);
            waitForNoRow("typed-when");
            controller.selectView("upcoming");
            compare(waitForRow("typed-when").startDate, isoInDays(1));
        }

        function test_a_refused_date_keeps_the_popover_open() {
            addToToday("not-a-date");
            selectLast("not-a-date");
            const popup = openWhen();
            typeText("banana");
            compare(findChild(popup.contentItem, "whenPreview").text, "Not a date");
            keyClick(Qt.Key_Return);
            verify(popup.visible, "still open");
            keyClick(Qt.Key_Escape);
            tryVerify(() => !popup.visible, 1000);
            verify(rowFor("not-a-date") !== null, "unchanged");
        }

        function test_the_calendar_works_from_the_keyboard() {
            addToToday("cal-keys");
            selectLast("cal-keys");
            const popup = openWhen();
            keyClick(Qt.Key_Down);
            verify(findChild(popup.contentItem, "whenCalendar").activeFocus, "Down moves into the calendar");
            keyClick(Qt.Key_Right);
            keyClick(Qt.Key_Right);
            keyClick(Qt.Key_Return);
            tryVerify(() => !popup.visible, 1000);
            controller.selectView("upcoming");
            compare(waitForRow("cal-keys").startDate, isoInDays(2));
        }

        // Every part of the popover stays inside it, whatever the style's
        // button sizes (native styles pad buttons more than Basic does).
        function test_the_when_popover_holds_all_its_controls() {
            const row = addToToday("fit-task");
            expandRow(row);
            for (const mode of ["when", "deadline"]) {
                mouseClick(findChild(row, mode === "when" ? "schedule-date" : "schedule-deadline"));
                const popup = findChild(view, "whenPopup");
                tryVerify(() => popup.opened, 1000);
                waitForRendering(popup.contentItem);
                const names = ["whenInput", "whenNextMonth", "whenCalendar", mode + "-today", mode + "-none"];
                if (mode === "when")
                    names.push("when-evening", "when-someday");
                for (const name of names) {
                    const item = findChild(popup.contentItem, name);
                    verify(item !== null && item.visible, name + " shown");
                    const right = item.mapToItem(popup.background, item.width, 0).x;
                    verify(right <= popup.background.width + 0.5,
                        mode + ": " + name + " ends at " + right + ", past the popover's " + popup.background.width);
                }
                keyClick(Qt.Key_Escape);
                tryVerify(() => !popup.visible, 2000);
            }
        }

        // Controls follow Matters' scheme, not the desktop's palette: in
        // dark mode a popover's buttons and field are dark too.
        function test_controls_take_matters_colours_in_dark_mode() {
            view.appearance = "dark";
            const row = addToToday("palette-task");
            expandRow(row);
            mouseClick(findChild(row, "schedule-date"));
            const popup = findChild(view, "whenPopup");
            tryVerify(() => popup.opened, 1000);
            const button = findChild(popup.contentItem, "when-today");
            const field = findChild(popup.contentItem, "whenInput");
            compare(String(button.palette.button), "#2c2e33");
            compare(String(button.palette.buttonText), "#f8f9fa");
            compare(String(field.palette.base), "#25262b");
            keyClick(Qt.Key_Escape);
            tryVerify(() => !popup.visible, 2000);
            view.appearance = "system";
        }

        function test_picking_a_day_next_month_from_the_date_button() {
            const row = addToToday("cal-mouse");
            expandRow(row);
            mouseClick(findChild(row, "schedule-date"));
            const popup = findChild(view, "whenPopup");
            tryVerify(() => popup.opened, 1000, "Date… opens the When popover");
            waitForRendering(popup.contentItem);
            mouseClick(findChild(popup.contentItem, "whenNextMonth"));
            const first = new Date();
            first.setDate(1);
            first.setMonth(first.getMonth() + 1);
            first.setDate(15);
            const iso = Qt.formatDate(first, "yyyy-MM-dd");
            // The grid's day cells are children of its content item, which
            // findChild does not search.
            const grid = findChild(popup.contentItem, "whenCalendar");
            const dayCell = () => {
                for (const cell of grid.contentItem.children) {
                    if (cell.objectName === "day-" + iso)
                        return cell;
                }
                return null;
            };
            tryVerify(() => dayCell() !== null, 1000, "day " + iso + " shown");
            waitForRendering(grid); // the new month's cells are laid out
            verify(dayCell().height > 20 && grid.height > 150, "the calendar has room on screen");
            mouseClick(dayCell());
            tryVerify(() => !popup.visible, 1000);
            controller.selectView("upcoming");
            compare(waitForRow("cal-mouse").startDate, iso);
        }

        // ---- Deadlines (#20) -------------------------------------------------

        // A task in the Inbox, selected, with the Deadline popover open.
        function deadlineFor(title) {
            controller.selectView("inbox");
            view.captureTask(title);
            const row = waitForRow(title);
            settle();
            list.selectedTaskId = row.taskId;
            list.forceActiveFocus();
            const popup = findChild(view, "whenPopup");
            keyClick(Qt.Key_D, Qt.ControlModifier);
            tryVerify(() => popup.opened, 1000, "Ctrl+D opens the Deadline popover");
            waitForRendering(popup.contentItem);
            return { row: row, popup: popup };
        }

        function test_ctrl_d_sets_a_deadline_by_typing() {
            const t = deadlineFor("due-typed");
            compare(findChild(t.popup.contentItem, "whenTitle").text, "Deadline");
            verify(!findChild(t.popup.contentItem, "deadline-evening").visible, "no This Evening for a deadline");
            verify(!findChild(t.popup.contentItem, "deadline-someday").visible, "no Someday for a deadline");
            typeText("in 3 days");
            verify(findChild(t.popup.contentItem, "whenPreview").text !== "Not a date");
            keyClick(Qt.Key_Return);
            tryVerify(() => !t.popup.visible, 1000);
            tryCompare(t.row, "deadline", isoInDays(3), 2000);
            compare(t.row.deadlineStatus, "set");
            verify(findChild(t.row, "deadlineBadge").visible, "a deadline badge");
            compare(t.row.startDate, "", "the When is unchanged");
            verify(rowFor("due-typed") !== null, "still in the Inbox");
        }

        function test_a_deadline_due_today_is_urgent_and_shows_in_today() {
            const t = deadlineFor("due-today");
            mouseClick(findChild(t.popup.contentItem, "deadline-today"));
            tryCompare(t.row, "deadlineStatus", "today", 2000);
            compare(findChild(t.row, "deadlineLabel").text, "Due today");
            verify(findChild(t.row, "deadlineBadge").urgent, "shown as urgent");
            controller.selectView("today");
            waitForRow("due-today");
        }

        function test_a_deadline_from_the_calendar_and_cleared_again() {
            const t = deadlineFor("due-cal");
            keyClick(Qt.Key_Down);
            keyClick(Qt.Key_Right);
            keyClick(Qt.Key_Return);
            tryVerify(() => !t.popup.visible, 1000);
            tryCompare(t.row, "deadline", isoInDays(1), 2000);

            // Clear it from the open task's Deadline… button.
            expandRow(t.row);
            mouseClick(findChild(t.row, "schedule-deadline"));
            tryVerify(() => t.popup.opened, 1000);
            waitForRendering(t.popup.contentItem);
            mouseClick(findChild(t.popup.contentItem, "deadline-none"));
            tryCompare(t.row, "deadline", "", 2000);
            verify(!findChild(t.row, "deadlineBadge").visible);
        }

        // ---- Sections (#25) ---------------------------------------------------

        // The visible heading titled `title`, if the list shows one.
        function headingTitled(title) {
            for (const child of list.contentItem.children) {
                if ((child.objectName === "sectionHeading" || child.objectName === "eveningHeading")
                        && child.visible && child.title === title)
                    return child;
            }
            return null;
        }

        // The nearest heading above the row of `task` is `title`: the task is
        // in that section, wherever it sits within it.
        function headedBy(task, title) {
            const row = waitForRow(task);
            tryVerify(() => headingTitled(title) !== null, 2000, "a heading '" + title + "'");
            let nearest = null;
            for (const child of list.contentItem.children) {
                if ((child.objectName === "sectionHeading" || child.objectName === "eveningHeading")
                        && child.visible && child.y < row.y && (nearest === null || child.y > nearest.y))
                    nearest = child;
            }
            verify(nearest !== null, "'" + task + "' has a heading above it");
            compare(nearest.title, title, "'" + task + "' sits under '" + title + "'");
        }

        function test_upcoming_is_grouped_by_day() {
            controller.selectView("upcoming");
            view.captureTask("up-tomorrow"); // captured in Upcoming: tomorrow
            waitForRow("up-tomorrow");
            headedBy("up-tomorrow", "Tomorrow");
        }

        function test_anytime_is_grouped_by_project() {
            controller.createProject("Grouped");
            const id = JSON.parse(controller.projectsJson).find(p => p.title === "Grouped").id;
            controller.selectProject(id);
            view.captureTask("in-grouped");
            waitForRow("in-grouped");
            controller.selectView("anytime");
            headedBy("in-grouped", "Grouped");
        }

        function test_the_logbook_is_grouped_by_day() {
            const row = addToToday("finish-me");
            controller.toggleTask(row.taskId);
            controller.selectView("logbook");
            headedBy("finish-me", "Today");
        }

        // ---- When completed tasks are logged (#67, #72) ---------------------

        // Choose a Logbook policy in Preferences, by keyboard: Ctrl+, opens
        // it on the current choice; Tab and Shift+Tab move through the
        // choices; Space chooses.
        function chooseLogPolicy(key) {
            const dialog = findChild(view, "preferencesDialog");
            focusList();
            keySequence("Ctrl+,");
            tryVerify(() => dialog.opened, 1000, "Ctrl+, opens Preferences");
            const current = findChild(dialog.contentItem, "logPolicy-" + controller.logPolicy);
            tryVerify(() => current.activeFocus, 1000, "the current choice has the focus");
            const order = ["when-leaving", "immediately", "daily", "manually"];
            const steps = order.indexOf(key) - order.indexOf(controller.logPolicy);
            for (let i = 0; i < Math.abs(steps); ++i)
                keyClick(Qt.Key_Tab, steps > 0 ? Qt.NoModifier : Qt.ShiftModifier);
            const wanted = findChild(dialog.contentItem, "logPolicy-" + key);
            if (steps !== 0) {
                verify(wanted.activeFocus, "Tab reaches " + key);
                keyClick(Qt.Key_Space);
            }
            tryCompare(controller, "logPolicy", key);
            verify(wanted.checked);
            keyClick(Qt.Key_Escape);
            tryVerify(() => !dialog.visible, 2000);
        }

        function test_completed_tasks_leave_at_once_when_chosen() {
            try {
                chooseLogPolicy("immediately");
                const row = addToToday("lg-now");
                const other = addToToday("lg-stays");
                controller.toggleTask(row.taskId);
                waitForNoRow("lg-now");
                verify(indexOf("lg-stays") >= 0, "the open task stays");
                verify(!findChild(view, "logCompletedButton").visible, "nothing waits to be logged");
            } finally {
                controller.chooseLogPolicy("when-leaving");
            }
        }

        function test_completed_tasks_wait_for_log_completed_when_chosen() {
            try {
                chooseLogPolicy("manually");
                const row = addToToday("lg-manual");
                controller.toggleTask(row.taskId);
                // Still listed, done, across navigation; not in the Logbook.
                controller.selectView("logbook");
                waitForNoRow("lg-manual");
                controller.selectView("today");
                verify(waitForRow("lg-manual").done, "shown done");
                const before = controller.todayCount;

                const button = findChild(view, "logCompletedButton");
                verify(button.visible);
                keySequence("Ctrl+Shift+L");
                waitForNoRow("lg-manual");
                compare(controller.todayCount, before, "the badge counted open tasks only");
                controller.selectView("logbook");
                waitForRow("lg-manual");

                // The button does the same.
                controller.selectView("today");
                const again = addToToday("lg-manual-2");
                controller.toggleTask(again.taskId);
                verify(waitForRow("lg-manual-2").done);
                mouseClick(button);
                waitForNoRow("lg-manual-2");
            } finally {
                controller.chooseLogPolicy("when-leaving");
            }
        }

        function test_completed_tasks_stay_until_the_day_ends_when_chosen() {
            try {
                chooseLogPolicy("daily");
                const row = addToToday("lg-daily");
                controller.toggleTask(row.taskId);
                controller.selectView("inbox");
                controller.selectView("today");
                verify(waitForRow("lg-daily").done, "still listed after leaving");
                keySequence("Ctrl+Shift+L");
                waitForNoRow("lg-daily");
            } finally {
                controller.chooseLogPolicy("when-leaving");
            }
        }

        function test_choosing_a_policy_never_brings_back_logged_tasks() {
            try {
                const row = addToToday("lg-already");
                controller.toggleTask(row.taskId);
                controller.selectView("inbox");
                controller.selectView("today");
                waitForNoRow("lg-already");
                chooseLogPolicy("manually");
                controller.selectView("today");
                settle();
                verify(rowFor("lg-already") === null, "finished before the choice: logged");

                // Back to the default, ticking keeps a task only until leaving.
                chooseLogPolicy("when-leaving");
                const kept = addToToday("lg-default");
                controller.toggleTask(kept.taskId);
                settle();
                verify(rowFor("lg-default") !== null, "kept while the list is open");
                verify(!findChild(view, "logCompletedButton").visible);
                controller.selectView("inbox");
                controller.selectView("today");
                waitForNoRow("lg-default");
            } finally {
                controller.chooseLogPolicy("when-leaving");
            }
        }

        // ---- Areas (#15) -----------------------------------------------------

        // Depth-first search of the visual tree under `item`.
        function findVisual(item, test) {
            if (!item)
                return null;
            if (test(item))
                return item;
            for (const child of item.children) {
                const found = findVisual(child, test);
                if (found)
                    return found;
            }
            return null;
        }

        function sidebarArea(title) {
            return findVisual(view, i => i.objectName === "areaItem" && i.areaTitle === title && i.visible);
        }

        function sidebarAreaProject(title) {
            return findVisual(view, i => i.objectName === "areaProjectItem" && i.projectTitle === title && i.visible);
        }

        function createArea(title) {
            const id = controller.createArea(title);
            verify(id !== "", "area created");
            tryVerify(() => sidebarArea(title) !== null, 2000, "'" + title + "' in the sidebar");
            return id;
        }

        function test_an_area_is_created_from_the_sidebar_and_opened() {
            const plus = findChild(view, "newContainerButton");
            mouseClick(plus);
            // Menu items live in the window's overlay, not under the view; the
            // menu belongs to the + button.
            const item = findChild(plus, "newAreaItem");
            verify(item !== null, "the + menu has New Area");
            tryVerify(() => item.visible, 1000, "the + menu is open");
            mouseClick(item);
            const dialog = findChild(view, "newAreaDialog");
            tryVerify(() => dialog.opened, 1000);
            typeText("Home Base");
            dialog.accept();
            tryVerify(() => !dialog.visible, 1000);
            compare(controller.currentView, "area");
            compare(controller.currentTitle, "Home Base");
            tryVerify(() => sidebarArea("Home Base") !== null && sidebarArea("Home Base").isSelected, 2000);

            view.captureTask("area-task");
            waitForRow("area-task");
            controller.selectView("inbox");
            waitForNoRow("area-task");
            mouseClick(shownArea("Home Base"));
            compare(controller.currentView, "area");
            waitForRow("area-task");
        }

        function test_a_project_is_filed_in_an_area() {
            createArea("Filing Area");
            controller.createProject("Filed Project");
            const project = JSON.parse(controller.projectsJson).find(p => p.title === "Filed Project");
            controller.selectProject(project.id);
            // The ⋯ menu's Area submenu files it.
            const button = findChild(view, "containerMenuButton");
            clickWhenShown(button);
            const menu = findChild(button, "projectMenu");
            tryVerify(() => menu.opened, 1000);
            const none = findChild(button, "areaChoice-none");
            verify(none.checked, "not in an area yet");
            mouseClick(menu.itemAt(0)); // the Area submenu
            const area = JSON.parse(controller.areasJson).find(a => a.title === "Filing Area");
            const choice = findChild(button, "areaChoice-" + area.id);
            tryVerify(() => choice.visible, 1000, "the Area submenu lists the area");
            waitForRendering(view);
            mouseClick(choice);
            tryVerify(() => sidebarAreaProject("Filed Project") !== null, 2000, "listed under its area");
            const top = findChild(view, "projectsList");
            verify(!top.model.some(p => p.title === "Filed Project"), "no longer among projects without an area");
        }

        function test_an_area_goes_to_the_trash_and_comes_back() {
            const id = createArea("Spare Area");
            controller.selectArea(id);
            menuAction("trashAreaItem");
            compare(controller.currentView, "inbox", "the Inbox shows instead");
            tryVerify(() => sidebarArea("Spare Area") === null, 2000, "gone from the sidebar");

            const toast = findChild(view, "undoToast");
            verify(toast.visible);
            mouseClick(findChild(toast, "undoButton"));
            compare(controller.currentView, "area");
            compare(controller.currentAreaId, id);
            tryVerify(() => sidebarArea("Spare Area") !== null, 2000, "Undo puts it back");

            menuAction("trashAreaItem");
            controller.selectView("trash");
            const trashed = findVisual(view, i => i.objectName === "trashedArea" && i.visible);
            verify(trashed !== null, "the Trash lists the area");
            verify(findChild(view, "emptyTrashButton").visible);
            mouseClick(findChild(trashed, "restoreAreaButton"));
            tryVerify(() => sidebarArea("Spare Area") !== null, 2000, "Put Back restores it");
        }

        // Click a control that has just appeared, once the window is laid out.
        function clickWhenShown(item) {
            tryVerify(() => item.visible && item.width > 0, 1000);
            waitForRendering(view);
            mouseClick(item);
        }

        function test_an_area_is_renamed() {
            const id = createArea("Old Name");
            controller.selectArea(id);
            menuAction("renameAreaItem");
            const dialog = findChild(view, "renameDialog");
            tryVerify(() => dialog.opened, 1000);
            const input = findChild(dialog, "renameInput");
            compare(input.text, "Old Name");
            input.text = "New Name";
            dialog.accept();
            tryCompare(controller, "currentTitle", "New Name", 2000);
            tryVerify(() => sidebarArea("New Name") !== null, 2000);

            // A blank name is refused.
            tryVerify(() => !dialog.visible, 1000);
            menuAction("renameAreaItem");
            tryVerify(() => dialog.opened, 1000);
            findChild(dialog, "renameInput").text = "   ";
            dialog.accept();
            tryVerify(() => !dialog.visible, 2000);
            compare(controller.currentTitle, "New Name");
            compare(controller.errorMessage, "");
        }

        // ---- Calmer rows -----------------------------------------------------

        function test_rows_show_only_dates_the_list_does_not_already_say() {
            const day = addToToday("plain-today");
            const eve = addToToday("evening-today");
            controller.scheduleTask(eve.taskId, "evening");
            settle();
            verify(!findChild(day, "startBadge").visible, "Today doesn't repeat today's date");
            tryVerify(() => eve.evening, 2000);
            verify(!findChild(eve, "eveningBadge").visible, "nor mark tasks under This Evening");
            // Leave Today without an evening task: later tests select its last row.
            controller.scheduleTask(eve.taskId, "someday");
            waitForNoRow("evening-today");

            // In a project, the same information is shown, for people.
            openFreshProject("badges-proj", ["b-tomorrow", "b-evening"]);
            controller.scheduleTaskTyped(rowFor("b-tomorrow").taskId, "tomorrow", true);
            controller.scheduleTask(rowFor("b-evening").taskId, "evening");
            tryCompare(findChild(rowFor("b-tomorrow"), "startLabel"), "text", "Tomorrow", 2000);
            tryVerify(() => findChild(rowFor("b-evening"), "eveningBadge").visible, 2000);
            compare(findChild(rowFor("b-evening"), "startLabel").text, "Today");
            // It is in Today's This Evening too; clear it so later tests that
            // select Today's last row aren't affected.
            controller.scheduleTask(rowFor("b-evening").taskId, "none");
        }

        function test_rows_line_up_in_every_list() {
            const today = addToToday("align-today");
            settle();
            const x = (row) => findChild(row, "checkCircle").mapToItem(list, 0, 0).x;
            const todayX = x(today);
            controller.selectView("someday");
            view.captureTask("align-someday");
            const someday = waitForRow("align-someday");
            settle();
            compare(x(someday), todayX, "the checkbox column doesn't move between lists");
        }

        // ---- An open task is a page ------------------------------------------

        function test_an_open_task_keeps_when_and_deadline_in_its_corner() {
            const row = addToToday("corner-task");
            expandRow(row);
            const when = findChild(row, "schedule-date");
            const deadline = findChild(row, "schedule-deadline");
            compare(when.label, "Today");
            compare(deadline.label, "Deadline");
            // The buttons carry no text of their own, so no style paints a
            // second label behind theirs.
            compare(when.text, "");
            compare(deadline.text, "");
            compare(when.Accessible.name, "Today");
            verify(when.mapToItem(row, 0, 0).x > row.width / 2, "tucked to the right");

            controller.setTaskDeadline(row.taskId, Qt.formatDate(new Date(), "yyyy-MM-dd"));
            tryCompare(deadline, "label", "Due today", 2000);
            controller.scheduleTask(row.taskId, "evening");
            tryCompare(when, "label", "This Evening", 2000);
            controller.scheduleTask(row.taskId, "someday");
            waitForNoRow("corner-task"); // left Today for Someday
        }

        // When and Deadline read as values to change, not as commands: a pill
        // with a chevron, a menu button to assistive tools, and an unset
        // value shown dimmer than a set one.
        function test_when_and_deadline_are_pills_to_change() {
            const row = addToToday("pill-task");
            expandRow(row);
            const when = findChild(row, "schedule-date");
            const deadline = findChild(row, "schedule-deadline");
            for (const pill of [when, deadline]) {
                verify(findChild(pill, "pillChevron").visible, pill.objectName + " has a chevron");
                compare(pill.Accessible.role, Accessible.ButtonMenu);
                verify(pill.background.border.width > 0, pill.objectName + " is outlined");
            }
            verify(when.hasValue, "Today is a set value");
            verify(!deadline.hasValue, "no deadline yet");
            verify(!Qt.colorEqual(findChild(when, "pillLabel").color, findChild(deadline, "pillLabel").color),
                "an unset value is dimmer than a set one");
            controller.setTaskDeadline(row.taskId, Qt.formatDate(new Date(), "yyyy-MM-dd"));
            tryVerify(() => deadline.hasValue, 2000);
            verify(Qt.colorEqual(findChild(when, "pillLabel").color, findChild(deadline, "pillLabel").color));
        }

        function test_notes_grow_with_what_is_written() {
            const row = addToToday("long-notes");
            expandRow(row);
            const notes = findChild(row, "notesInput");
            const before = notes.height;
            mouseClick(notes);
            for (let i = 0; i < 5; ++i) {
                typeText("line");
                keyClick(Qt.Key_Return);
            }
            tryVerify(() => notes.height > before + 40, 2000, "the notes grew: " + before + " -> " + notes.height);
            tryVerify(() => row.height === row.targetHeight, 2000, "and the card with them");
            keyClick(Qt.Key_Escape);
        }

        // ---- Project header ---------------------------------------------------

        // Open the ⋯ menu beside the title and choose `itemName`.
        function menuAction(itemName) {
            const button = findChild(view, "containerMenuButton");
            clickWhenShown(button);
            const item = findChild(button, itemName);
            verify(item !== null, itemName + " in the menu");
            tryVerify(() => item.visible, 1000, "the menu is open");
            waitForRendering(view);
            mouseClick(item);
            // Styles that animate menus (Material) keep a closing menu over
            // the window, catching clicks, until it has faded out.
            for (const name of ["projectMenu", "areaMenu"]) {
                const menu = findChild(button, name);
                tryVerify(() => !menu.visible, 2000, name + " has closed");
            }
        }

        function test_a_projects_progress_pie_fills_as_tasks_are_done() {
            openFreshProject("pie-proj", ["pie-1", "pie-2", "pie-3", "pie-4"]);
            const pie = findChild(view, "progressPie");
            verify(pie.visible);
            compare(pie.progress, 0);
            controller.toggleTask(rowFor("pie-1").taskId);
            tryCompare(pie, "progress", 0.25, 2000);
            controller.toggleTask(rowFor("pie-2").taskId);
            tryCompare(pie, "progress", 0.5, 2000);
            controller.selectView("inbox");
            verify(!pie.visible, "only projects have one");
        }

        function test_a_project_is_renamed_from_its_menu() {
            openFreshProject("old-proj-name", ["rn-1"]);
            menuAction("renameProjectItem");
            const dialog = findChild(view, "renameDialog");
            tryVerify(() => dialog.opened, 1000);
            const input = findChild(dialog, "renameInput");
            compare(input.text, "old-proj-name");
            input.text = "New project name";
            dialog.accept();
            tryCompare(controller, "currentTitle", "New project name", 2000);
            tryVerify(() => JSON.parse(controller.projectsJson).some(p => p.title === "New project name"), 2000);
            tryVerify(() => !dialog.visible, 2000);
        }

        // ---- Motion (#77) -----------------------------------------------------

        function test_motion_follows_the_reduced_motion_preference() {
            const preference = view.reduceMotion;
            view.reduceMotion = false;
            verify(list.add.enabled && list.remove.enabled && list.displaced.enabled && list.move.enabled,
                   "lists animate");
            view.reduceMotion = true;
            verify(!list.add.enabled && !list.remove.enabled && !list.displaced.enabled && !list.move.enabled,
                   "and stand still with reduced motion");

            // With reduced motion, an opened task is at full height at once.
            const row = addToToday("still-row");
            settle();
            clickRowBody(row);
            wait(0);
            waitForRendering(view);
            compare(row.height, row.targetHeight, "no height animation");
            verify(row.targetHeight > 48);
            view.reduceMotion = preference;
        }

        // ---- Details ----------------------------------------------------------

        function test_upcoming_days_lead_with_the_day_number() {
            controller.selectView("upcoming");
            view.captureTask("up-number"); // tomorrow
            waitForRow("up-number");
            tryVerify(() => headingTitled("Tomorrow") !== null, 2000);
            const tomorrow = new Date();
            tomorrow.setDate(tomorrow.getDate() + 1);
            compare(findChild(headingTitled("Tomorrow"), "headingDay").text, String(tomorrow.getDate()));
        }

        function test_f9_and_the_toggle_hide_and_show_the_sidebar() {
            const sidebar = findChild(view, "sidebar");
            verify(sidebar.visible && sidebar.width >= 200);
            focusList();
            keyClick(Qt.Key_F9);
            tryVerify(() => !sidebar.visible, 2000, "F9 hides the sidebar");

            // Everything still works without it.
            const find = findChild(view, "quickFindDialog");
            keyClick(Qt.Key_K, Qt.ControlModifier);
            tryVerify(() => find.opened, 1000, "Quick Find opens");
            keyClick(Qt.Key_Escape);
            tryVerify(() => !find.visible, 2000);

            clickWhenShown(findChild(view, "sidebarToggle"));
            tryVerify(() => sidebar.visible && sidebar.width >= 200, 2000, "the toggle shows it again");
        }

        function test_ctrl_n_adds_a_task_and_ctrl_shift_n_a_project() {
            focusList();
            keyClick(Qt.Key_N, Qt.ControlModifier);
            const input = findChild(view, "newTaskInput");
            verify(input.activeFocus, "Ctrl+N focuses the quick-add field");
            for (const c of "via-keys")
                keyClick(c);
            keyClick(Qt.Key_Return);
            waitForRow("via-keys");
            keyClick(Qt.Key_Escape);
            verify(list.activeFocus, "Escape returns to the list");

            const dialog = findChild(view, "newProjectDialog");
            keyClick(Qt.Key_N, Qt.ControlModifier | Qt.ShiftModifier);
            tryVerify(() => dialog.opened, 1000, "Ctrl+Shift+N opens New Project");
            dialog.reject();
            tryVerify(() => !dialog.visible, 1000);
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
            // The open task's When control opens the When popover.
            mouseClick(findChild(row, "schedule-date"));
            const popup = findChild(view, "whenPopup");
            tryVerify(() => popup.opened, 1000);
            waitForRendering(popup.contentItem);
            mouseClick(findChild(popup.contentItem, "when-someday"));
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

        // Open Quick Find and search for `text`, replacing the last search.
        function quickFind(text) {
            keySequence("Ctrl+K");
            const dialog = findChild(view, "quickFindDialog");
            tryVerify(() => dialog.opened, 1000, "Ctrl+K opens Quick Find");
            typeText(text);
            compare(findChild(view, "searchInput").text, text, "typing replaces the last search");
            return findChild(view, "searchResults");
        }

        function test_quick_find_opens_areas_projects_and_tasks() {
            const area = controller.createArea("qf-xylo-area");
            controller.createProject("qf-xylo-project");
            const project = JSON.parse(controller.projectsJson).find(p => p.title === "qf-xylo-project").id;
            controller.selectView("inbox");
            controller.addTask("qf-xylo-task");
            controller.selectView("today");

            let results = quickFind("xylo");
            tryVerify(() => results.count === 3, 1000, "an area, a project and a task");
            const kinds = [];
            for (let i = 0; i < results.count; ++i)
                kinds.push(results.itemAtIndex(i).kind);
            compare(kinds, ["area", "project", "task"], "areas and projects before tasks");
            compare(findChild(results.itemAtIndex(0), "searchResultKind").text, "Area");
            compare(findChild(results.itemAtIndex(1), "searchResultKind").text, "Project");

            // A click on the project opens it.
            mouseClick(results.itemAtIndex(1));
            tryCompare(controller, "currentView", "project");
            compare(controller.currentProjectId, project);
            const dialog = findChild(view, "quickFindDialog");
            tryVerify(() => !dialog.visible, 1000, "Quick Find closes");
            tryVerify(() => list.activeFocus, 1000, "focus goes back to the list");

            // Enter opens the first result, the area.
            results = quickFind("xylo");
            tryVerify(() => results.count === 3);
            keyClick(Qt.Key_Return);
            tryCompare(controller, "currentView", "area");
            compare(controller.currentAreaId, area);

            // Down moves the highlight: two steps to the task.
            results = quickFind("xylo");
            tryVerify(() => results.count === 3);
            keyClick(Qt.Key_Down);
            keyClick(Qt.Key_Down);
            compare(results.currentIndex, 2);
            keyClick(Qt.Key_Up);
            keyClick(Qt.Key_Down);
            keyClick(Qt.Key_Return);
            tryCompare(controller, "currentView", "inbox");
            const row = waitForRow("qf-xylo-task");
            verify(row.isExpanded, "the task is open");
        }

        // ---- Typing to find (#66) ---------------------------------------------

        // `/` opens Quick Find as Ctrl+K does: on the last search, selected,
        // not searching for "/".
        function test_slash_opens_quick_find() {
            const dialog = findChild(view, "quickFindDialog");
            const input = findChild(view, "searchInput");
            input.text = "tq-slash-last";
            focusList();
            keyClick("/");
            tryVerify(() => dialog.opened, 1000, "/ opens Quick Find");
            compare(input.text, "tq-slash-last", "the last search, not a slash");
            compare(input.selectedText, "tq-slash-last", "selected, so typing replaces it");
            keyClick(Qt.Key_Escape);
            tryVerify(() => !dialog.visible, 2000);
        }

        function test_typing_on_the_list_searches_for_what_was_typed() {
            const dialog = findChild(view, "quickFindDialog");
            const input = findChild(view, "searchInput");
            focusList();
            typeText("Logb");
            tryVerify(() => dialog.opened, 1000, "a typed letter opens Quick Find");
            compare(input.text, "Logb", "every key typed is in the search, the first too");
            const results = findChild(view, "searchResults");
            tryVerify(() => results.count > 0);
            compare(results.itemAtIndex(0).kind, "view");
            compare(findChild(results.itemAtIndex(0), "searchResultKind").text, "List");
            keyClick(Qt.Key_Return);
            tryCompare(controller, "currentView", "logbook");
            tryVerify(() => !dialog.visible, 1000, "Quick Find closes");
            tryVerify(() => list.activeFocus, 1000, "focus goes back to the list");

            // Ctrl+K still starts afresh, with the last search selected.
            keySequence("Ctrl+K");
            tryVerify(() => dialog.opened, 1000);
            compare(input.selectedText, "Logb");
            keyClick(Qt.Key_Escape);
            tryVerify(() => !dialog.visible, 2000);
        }

        function test_every_list_opens_by_typing_part_of_its_name_and_enter() {
            const area = controller.createArea("tq-plover-area");
            controller.createProject("tq-plover-project");
            const project = projectIdOf("tq-plover-project");
            const dialog = findChild(view, "quickFindDialog");
            const typed = [["nbo", "inbox"], ["TOD", "today"], ["pcom", "upcoming"],
                           ["nyti", "anytime"], ["omed", "someday"], ["ogbo", "logbook"],
                           ["rash", "trash"]];
            for (const [text, id] of typed) {
                controller.selectView(id === "today" ? "inbox" : "today");
                focusList();
                typeText(text);
                tryVerify(() => dialog.opened, 1000, text + " opens Quick Find");
                keyClick(Qt.Key_Return);
                tryCompare(controller, "currentView", id, 1000, text + " opens " + id);
                tryVerify(() => !dialog.visible, 2000);
            }

            focusList();
            typeText("plover-ar");
            tryVerify(() => dialog.opened, 1000);
            keyClick(Qt.Key_Return);
            tryCompare(controller, "currentView", "area");
            compare(controller.currentAreaId, area);
            tryVerify(() => !dialog.visible, 2000);

            focusList();
            typeText("plover-pr");
            tryVerify(() => dialog.opened, 1000);
            keyClick(Qt.Key_Return);
            tryCompare(controller, "currentView", "project");
            compare(controller.currentProjectId, project);
            tryVerify(() => !dialog.visible, 2000);
        }

        function test_a_tag_found_opens_anytime_narrowed_to_it() {
            // Anytime lists tasks filed in an area or project.
            const area = controller.createArea("tq-heron-area");
            const tag = controller.createTag("tq-heron", "");
            controller.selectArea(area);
            controller.addTask("tq-heron-task");
            controller.addTask("tq-untagged-task");
            controller.addTag("task", waitForRow("tq-heron-task").taskId, tag);
            controller.selectView("today");

            focusList();
            typeText("tq-her");
            const dialog = findChild(view, "quickFindDialog");
            tryVerify(() => dialog.opened, 1000);
            const results = findChild(view, "searchResults");
            tryVerify(() => results.count === 3, 1000, "the tag, the area and the task named like it");
            const kinds = [];
            for (let i = 0; i < results.count; ++i)
                kinds.push(results.itemAtIndex(i).kind);
            compare(kinds, ["area", "tag", "task"], "areas and projects, then tags, then tasks");
            compare(findChild(results.itemAtIndex(1), "searchResultKind").text, "Tag");
            keyClick(Qt.Key_Down);
            keyClick(Qt.Key_Return);

            tryCompare(controller, "currentView", "anytime");
            tryCompare(view, "tagFilter", tag);
            waitForRow("tq-heron-task");
            waitForNoRow("tq-untagged-task");
            verify(filterChip("tq-heron").chosen, "the tag bar shows the tag chosen");

            // Navigating clears it as usual.
            controller.selectView("anytime");
            waitForRow("tq-untagged-task");
            compare(view.tagFilter, "");
        }

        function test_a_chosen_tag_no_task_has_still_shows_in_the_tag_bar() {
            const tag = controller.createTag("tq-unused-egret", "");
            focusList();
            typeText("unused-egr");
            const dialog = findChild(view, "quickFindDialog");
            tryVerify(() => dialog.opened, 1000);
            keyClick(Qt.Key_Return);
            tryCompare(view, "tagFilter", tag);
            tryVerify(() => filterChip("tq-unused-egret") !== null, 1000);
            verify(filterChip("tq-unused-egret").chosen);
            compare(list.count, 0, "nothing has the tag");

            tryVerify(() => !dialog.visible, 2000, "Quick Find has closed");
            clickFilter("tq-unused-egret");
            tryCompare(view, "tagFilter", "");
        }

        // A closed task's editors give up the focus: the list, taking it
        // back, must not hand typed keys to an editor no one can see.
        function test_keys_typed_after_a_task_closes_never_reach_its_editors() {
            const dialog = findChild(view, "quickFindDialog");
            for (const editor of ["notesInput", "titleInput"]) {
                const row = addToToday("tq-closed-" + editor);
                expandRow(row);
                const input = findChild(row, editor);
                mouseClick(input);
                verify(input.activeFocus);
                const before = input.text;

                // Closed some other way than Escape, as by opening another.
                list.expandedTaskId = "";
                tryVerify(() => !input.visible, 1000);
                focusList();
                verify(!input.activeFocus, editor + " gave up the focus");
                keyClick("q");
                tryVerify(() => dialog.opened, 1000, "the key went to the list");
                compare(input.text, before, editor + " is unchanged");
                keyClick(Qt.Key_Escape);
                tryVerify(() => !dialog.visible, 2000);
            }
        }

        function test_typing_in_editors_never_opens_quick_find() {
            const dialog = findChild(view, "quickFindDialog");
            const row = addToToday("tq-editors");
            expandRow(row);

            const notes = findChild(row, "notesInput");
            mouseClick(notes);
            verify(notes.activeFocus);
            typeText("notes kept");
            compare(notes.text, "notes kept");

            const title = findChild(row, "titleInput");
            mouseClick(title);
            verify(title.activeFocus);
            typeText("x");
            compare(title.text.replace("x", ""), "tq-editors", "typed into the title");

            const tags = findChild(row, "tagInput");
            mouseClick(tags);
            verify(tags.activeFocus);
            typeText("zq");
            compare(tags.text, "zq");
            keyClick(Qt.Key_Escape);

            keySequence("Ctrl+N");
            const add = findChild(view, "newTaskInput");
            verify(add.activeFocus);
            typeText("tq-added");
            compare(add.text, "tq-added");
            keyClick(Qt.Key_Escape);

            wait(50);
            verify(!dialog.visible, "Quick Find stayed closed");
        }

        // ---- Headings (#22) -----------------------------------------------------

        // Each row's heading id ("" above the headings), as listed, with an
        // empty heading's row shown as "[<id>]".
        function headingLayout() {
            const out = [];
            for (let i = 0; i < list.count; ++i) {
                const id = list.model.idAt(i);
                const heading = list.model.headingAt(i);
                out.push(id === "" ? "[" + heading + "]" : list.model.titleAt(i) + "@" + heading);
            }
            return out.join(",");
        }

        // The section heading showing heading `title`.
        function headingSection(title) {
            for (const item of childrenNamed(list.contentItem, "sectionHeading")) {
                if (item.visible && item.kind === "heading" && item.title === title)
                    return item;
            }
            return null;
        }

        function headingMenuAction(title, itemName) {
            const section = headingSection(title);
            verify(section !== null, "heading " + title + " is shown");
            const button = findChild(section, "headingMenuButton");
            clickWhenShown(button);
            const item = findChild(button, itemName);
            tryVerify(() => item.visible, 1000, "the heading menu is open");
            waitForRendering(view);
            mouseClick(item);
            const menu = findChild(button, "headingMenu");
            tryVerify(() => !menu.visible, 2000, "the heading menu has closed");
        }

        function test_a_heading_is_added_from_the_project_menu_and_filled_by_moving_tasks() {
            openFreshProject("hd-add", ["hd-add-1", "hd-add-2"]);
            menuAction("addHeadingItem");
            const dialog = findChild(view, "renameDialog");
            tryVerify(() => dialog.opened, 1000);
            const input = findChild(dialog, "renameInput");
            compare(input.text, "", "a new heading starts blank");
            input.text = "Phase one";
            dialog.accept();
            tryVerify(() => !dialog.visible, 2000);

            // The empty heading shows, below the tasks.
            tryVerify(() => headingSection("Phase one") !== null, 2000, "the heading is shown");
            const heading = list.model.headingAt(list.count - 1);
            verify(heading !== "");
            compare(headingLayout(), "hd-add-1@,hd-add-2@,[" + heading + "]");

            // Ctrl+Down from the last task files it under the heading.
            const row = rowFor("hd-add-2");
            list.selectedTaskId = row.taskId;
            list.forceActiveFocus();
            keyClick(Qt.Key_Down, Qt.ControlModifier);
            tryVerify(() => headingLayout() === "hd-add-1@,hd-add-2@" + heading, 2000, headingLayout());
            verify(headingSection("Phase one") !== null, "the heading now heads its task");
            compare(rowFor("hd-add-2"), row, "the row was moved, not recreated");

            // Ctrl+Up takes it back above the headings.
            keyClick(Qt.Key_Up, Qt.ControlModifier);
            tryVerify(() => headingLayout() === "hd-add-1@,hd-add-2@,[" + heading + "]", 2000, headingLayout());
        }

        function test_a_heading_is_renamed_moved_and_deleted_with_undo() {
            openFreshProject("hd-edit", ["hd-edit-1", "hd-edit-2"]);
            const first = controller.addHeading("First");
            const second = controller.addHeading("Second");
            controller.placeProjectTask(rowFor("hd-edit-1").taskId, first, "");
            controller.placeProjectTask(rowFor("hd-edit-2").taskId, second, "");
            tryVerify(() => headingLayout() === "hd-edit-1@" + first + ",hd-edit-2@" + second, 2000, headingLayout());
            settle();

            headingMenuAction("First", "renameHeadingItem");
            const dialog = findChild(view, "renameDialog");
            tryVerify(() => dialog.opened, 1000);
            const input = findChild(dialog, "renameInput");
            compare(input.text, "First");
            input.text = "Planning";
            dialog.accept();
            tryVerify(() => headingSection("Planning") !== null, 2000, "renamed");
            tryVerify(() => !dialog.visible, 2000);

            headingMenuAction("Planning", "moveHeadingDownItem");
            tryVerify(() => headingLayout() === "hd-edit-2@" + second + ",hd-edit-1@" + first, 2000,
                      "the heading moved with its task: " + headingLayout());
            settle();

            // Delete keeps the task, above the headings; Undo brings it back.
            headingMenuAction("Planning", "deleteHeadingItem");
            tryVerify(() => headingLayout() === "hd-edit-1@,hd-edit-2@" + second, 2000,
                      "the task stays in the project: " + headingLayout());
            const toast = findChild(view, "undoToast");
            verify(toast.visible, "the undo notice shows");
            compare(findChild(view, "undoText").text, "Heading “Planning” deleted", "deleted, not trashed");
            mouseClick(findChild(toast, "undoButton"));
            tryVerify(() => headingLayout() === "hd-edit-2@" + second + ",hd-edit-1@" + first, 2000,
                      "undo puts it back: " + headingLayout());
            verify(headingSection("Planning") !== null);
        }

        function test_dragging_a_task_onto_an_empty_heading_files_it_there() {
            openFreshProject("hd-drag", ["hd-drag-1", "hd-drag-2"]);
            const heading = controller.addHeading("Later");
            tryVerify(() => headingLayout() === "hd-drag-1@,hd-drag-2@,[" + heading + "]", 2000, headingLayout());
            settle();
            const row = rowFor("hd-drag-1");
            const empty = childrenNamed(list.contentItem, "emptyHeading").find(item => item.headingId === heading);
            verify(empty !== undefined, "the empty heading has a row");
            const to = empty.mapToItem(view, empty.width / 2, empty.height / 2);
            dragGripTo(findChild(row, "dragGrip"), to);
            tryVerify(() => headingLayout() === "hd-drag-2@,hd-drag-1@" + heading, 2000,
                      "dropped under the heading: " + headingLayout());
        }

        function test_the_selection_steps_over_an_empty_heading() {
            openFreshProject("hd-sel", ["hd-sel-1", "hd-sel-2"]);
            controller.addHeading("Nothing yet");
            const full = controller.addHeading("Something");
            controller.placeProjectTask(rowFor("hd-sel-2").taskId, full, "");
            tryVerify(() => list.model.idAt(1) === "", 2000, "the empty heading's row is second: " + headingLayout());
            settle();
            focusList();
            keyClick(Qt.Key_Down);
            compare(selectedTitle(), "hd-sel-1");
            keyClick(Qt.Key_Down);
            compare(selectedTitle(), "hd-sel-2", "Down goes past the empty heading");
            keyClick(Qt.Key_Up);
            compare(selectedTitle(), "hd-sel-1", "and so does Up");
        }

        // ---- Checklists (#21) ---------------------------------------------------

        // Every visual descendant of `item` named `name`, in order.
        function childrenNamed(item, name) {
            const found = [];
            for (let i = 0; i < item.children.length; ++i) {
                const child = item.children[i];
                if (child.objectName === name)
                    found.push(child);
                for (const inner of childrenNamed(child, name))
                    found.push(inner);
            }
            return found;
        }

        // The open row's checklist items, top to bottom, that are shown.
        function checklistRows(row) {
            return childrenNamed(row, "checklistItem")
                .filter(item => item.visible)
                .sort((a, b) => a.mapToItem(row, 0, 0).y - b.mapToItem(row, 0, 0).y);
        }

        function checklistTitles(row) {
            return checklistRows(row).map(item => item.title);
        }

        function checklistItem(row, title) {
            return checklistRows(row).find(item => item.title === title) || null;
        }

        // A task in Today with a checklist made through the controller, open.
        function checklistFixture(name, titles) {
            const row = addToToday(name);
            for (const title of titles)
                verify(controller.addChecklistItem(row.taskId, title, "") !== "");
            expandRow(row);
            waitForChecklist(row, titles);
            return row;
        }

        function waitForChecklist(row, titles, message) {
            tryVerify(() => checklistTitles(row).join("|") === titles.join("|"), 2000,
                      (message || "checklist") + ": got " + checklistTitles(row).join("|"));
            settle();
        }

        function test_a_checklist_is_typed_into_the_open_task() {
            const row = addToToday("cl-typed");
            expandRow(row);
            mouseClick(findChild(row, "checklistButton"));
            const field = findChild(row, "checklistNewEnd");
            const input = findChild(field, "checklistNewInput");
            tryVerify(() => input.activeFocus, 1000, "the new-item field takes the cursor");

            typeText("passport");
            keyClick(Qt.Key_Return);
            typeText("  charger ");
            keyClick(Qt.Key_Return);
            waitForChecklist(row, ["passport", "charger"]);
            verify(input.activeFocus, "ready for the next item");
            compare(input.text, "");

            keyClick(Qt.Key_Return); // nothing typed: nothing added
            keyClick(Qt.Key_Escape);
            verify(!field.visible, "Escape closes the field");
            verify(list.activeFocus, "and goes back to the list");
            verify(row.isExpanded, "the task stays open");
            waitForChecklist(row, ["passport", "charger"]);

            // Closed, the row says how much is done.
            clickRowBody(row);
            tryVerify(() => !row.isExpanded);
            const badge = findChild(row, "checklistBadge");
            tryVerify(() => badge.visible, 1000, "the closed row shows the checklist");
            compare(findChild(row, "checklistProgress").text, "0/2");
        }

        function test_checklist_items_are_ticked_renamed_and_deleted() {
            const row = checklistFixture("cl-edit", ["milk", "bread", "eggs"]);

            mouseClick(findChild(checklistItem(row, "bread"), "checklistBox"));
            tryVerify(() => checklistItem(row, "bread").done, 1000, "bread is ticked");
            mouseClick(findChild(checklistItem(row, "bread"), "checklistBox"));
            tryVerify(() => !checklistItem(row, "bread").done, 1000, "and unticked");
            mouseClick(findChild(checklistItem(row, "milk"), "checklistBox"));
            tryVerify(() => checklistItem(row, "milk").done, 1000, "milk is ticked");

            // Rename: select the title, type, and leave the field.
            const title = findChild(checklistItem(row, "eggs"), "checklistTitle");
            mouseClick(title);
            verify(title.activeFocus);
            title.selectAll();
            typeText("a dozen eggs");
            keyClick(Qt.Key_Tab);
            waitForChecklist(row, ["milk", "bread", "a dozen eggs"]);

            // A blank title is refused and the old one comes back.
            const renamed = findChild(checklistItem(row, "a dozen eggs"), "checklistTitle");
            mouseClick(renamed);
            renamed.selectAll();
            keyClick(Qt.Key_Space);
            keyClick(Qt.Key_Tab);
            tryCompare(renamed, "text", "a dozen eggs");

            // The hover button deletes an item.
            const bread = checklistItem(row, "bread");
            mouseMove(bread, bread.width / 2, 10);
            const del = findChild(bread, "checklistDelete");
            tryVerify(() => del.visible, 1000, "delete shows on hover");
            mouseClick(del);
            waitForChecklist(row, ["milk", "a dozen eggs"]);

            clickRowBody(row);
            tryVerify(() => !row.isExpanded);
            tryCompare(findChild(row, "checklistProgress"), "text", "1/2");
        }

        function test_checklist_keys_act_on_the_checklist_not_the_task() {
            const row = checklistFixture("cl-keys", ["one", "two", "three"]);
            const before = indexOf("cl-keys");
            const one = findChild(checklistItem(row, "one"), "checklistTitle");
            mouseClick(one);
            verify(one.activeFocus);

            // Ctrl+Down moves the item, never the task.
            keyClick(Qt.Key_Down, Qt.ControlModifier);
            waitForChecklist(row, ["two", "one", "three"], "one moved down");
            verify(one.activeFocus, "the moved item keeps the cursor");
            compare(indexOf("cl-keys"), before, "the task stayed where it was");

            // Up and Down go between items.
            keyClick(Qt.Key_Down);
            tryVerify(() => findChild(checklistItem(row, "three"), "checklistTitle").activeFocus, 1000, "Down: next item");
            keyClick(Qt.Key_Up);
            keyClick(Qt.Key_Up);
            tryVerify(() => findChild(checklistItem(row, "two"), "checklistTitle").activeFocus, 1000, "Up twice: first item");
            keyClick(Qt.Key_Up);
            verify(findChild(checklistItem(row, "two"), "checklistTitle").activeFocus, "Up at the top stays");
            verify(row.isExpanded);

            // Backspace in an emptied item deletes it, not the task, and the
            // cursor goes to the item above.
            const three = findChild(checklistItem(row, "three"), "checklistTitle");
            mouseClick(three);
            three.selectAll();
            keyClick(Qt.Key_Backspace); // clears the text
            keyClick(Qt.Key_Backspace); // deletes the empty item
            waitForChecklist(row, ["two", "one"], "three deleted");
            verify(rowFor("cl-keys") !== null, "the task is not in the Trash");
            tryVerify(() => findChild(checklistItem(row, "one"), "checklistTitle").activeFocus, 1000,
                      "the cursor moves to the item above");
            keyClick(Qt.Key_Escape);
            verify(list.activeFocus);
            verify(row.isExpanded, "Escape in an item leaves the task open");
        }

        function test_enter_in_an_item_adds_the_next_one_below_it() {
            const row = checklistFixture("cl-enter", ["first", "last"]);
            mouseClick(findChild(checklistItem(row, "first"), "checklistTitle"));
            keyClick(Qt.Key_End);
            keyClick(Qt.Key_Return);
            typeText("second");
            keyClick(Qt.Key_Return);
            waitForChecklist(row, ["first", "second", "last"]);
            typeText("third");
            keyClick(Qt.Key_Return);
            waitForChecklist(row, ["first", "second", "third", "last"], "the next goes below the one just added");

            // Clicking away keeps what was typed.
            typeText("fourth");
            mouseClick(findChild(row, "notesInput"));
            waitForChecklist(row, ["first", "second", "third", "fourth", "last"], "fourth was kept");

            // So does closing the task while typing.
            mouseClick(findChild(row, "checklistButton"));
            const input = findChild(findChild(row, "checklistNewEnd"), "checklistNewInput");
            tryVerify(() => input.activeFocus, 1000, "the field at the end is open");
            typeText("sixth");
            clickRowBody(row);
            tryVerify(() => !row.isExpanded);
            tryCompare(findChild(row, "checklistProgress"), "text", "0/6");
        }

        function test_dragging_a_checklist_item_reorders_it() {
            const row = checklistFixture("cl-drag", ["a", "b", "c"]);
            const a = checklistItem(row, "a");
            const c = checklistItem(row, "c");
            const grip = findChild(a, "checklistGrip");
            const line = findChild(row, "checklistDropLine");
            // Onto the lower half of c: a goes after it.
            const to = c.mapToItem(view, c.width / 2, c.height - 6);
            dragGripTo(grip, to, () => verify(line.visible, "a line shows where it will land"));
            waitForChecklist(row, ["b", "c", "a"]);
            verify(!line.visible);
            compare(checklistItem(row, "a"), a, "the item was moved, not recreated");
            verify(rowFor("cl-drag") !== null && indexOf("cl-drag") >= 0, "the task didn't move");
        }

        function checklistEditor(row) {
            return findChild(row, "checklistLoader").editor;
        }

        function hoverDelete(item) {
            mouseMove(item, item.width / 2, 10);
            const del = findChild(item, "checklistDelete");
            tryVerify(() => del.visible, 1000, "delete shows on hover");
            mouseClick(del);
        }

        function test_review_text_below_an_item_is_kept_when_the_end_field_opens() {
            const row = checklistFixture("cl-handover", ["a", "b"]);
            mouseClick(findChild(checklistItem(row, "a"), "checklistTitle"));
            keyClick(Qt.Key_End);
            keyClick(Qt.Key_Return);
            typeText("buy stamps");
            // The Checklist button takes no focus: it only moves the open field.
            mouseClick(findChild(row, "checklistButton"));
            waitForChecklist(row, ["a", "buy stamps", "b"], "the typed text was kept below a");
            const end = findChild(findChild(row, "checklistNewEnd"), "checklistNewInput");
            tryVerify(() => end.activeFocus, 1000, "the field at the end took over");
            keyClick(Qt.Key_Escape);
            // Opening the field under a again starts empty.
            mouseClick(findChild(checklistItem(row, "a"), "checklistTitle"));
            keyClick(Qt.Key_Return);
            const below = childrenNamed(checklistItem(row, "a"), "checklistNewInput")[0];
            tryVerify(() => below.activeFocus, 1000, "the field under a is open");
            compare(below.text, "");
        }

        function test_review_hover_deleting_keeps_typed_text_and_drops_a_pending_rename() {
            const row = checklistFixture("cl-hover-delete", ["a", "b", "c"]);
            controller.errorMessage = "";
            // Typing a new item under a, then deleting a: the text is kept.
            mouseClick(findChild(checklistItem(row, "a"), "checklistTitle"));
            keyClick(Qt.Key_End);
            keyClick(Qt.Key_Return);
            typeText("x");
            hoverDelete(checklistItem(row, "a"));
            waitForChecklist(row, ["x", "b", "c"], "x replaced a");
            compare(controller.errorMessage, "", "no error");

            // Renaming b without saving, then deleting it: no stale rename.
            const title = findChild(checklistItem(row, "b"), "checklistTitle");
            mouseClick(title);
            title.selectAll();
            typeText("new text");
            hoverDelete(checklistItem(row, "b"));
            waitForChecklist(row, ["x", "c"], "b deleted");
            settle();
            compare(controller.errorMessage, "", "no error after the delete");
        }

        function test_review_an_item_deleted_elsewhere_keeps_typed_text_without_an_error() {
            const row = checklistFixture("cl-deleted-elsewhere", ["a", "b"]);
            controller.errorMessage = "";
            mouseClick(findChild(checklistItem(row, "a"), "checklistTitle"));
            keyClick(Qt.Key_End);
            keyClick(Qt.Key_Return);
            typeText("x");
            // As another window or device would: a goes while x is typed under it.
            controller.deleteChecklistItem(checklistItem(row, "a").itemId);
            waitForChecklist(row, ["b", "x"], "x was kept, at the end");
            settle();
            compare(controller.errorMessage, "", "no error");
        }

        function test_review_ctrl_period_ticks_an_item_and_f2_stays_in_it() {
            const row = checklistFixture("cl-tick", ["one", "two"]);
            const one = findChild(checklistItem(row, "one"), "checklistTitle");
            mouseClick(one);
            keyClick(Qt.Key_Period, Qt.ControlModifier);
            tryVerify(() => checklistItem(row, "one").done, 1000, "Ctrl+. ticks");
            verify(one.activeFocus, "and keeps the cursor");
            keyClick(Qt.Key_Period, Qt.ControlModifier);
            tryVerify(() => !checklistItem(row, "one").done, 1000, "and unticks");

            // F2 renames the selected task from the list; typed in an item it
            // does nothing.
            list.selectedTaskId = row.taskId;
            keyClick(Qt.Key_F2);
            settle();
            verify(one.activeFocus, "F2 did not leave the checklist");
            compare(view.titleEditId, "", "the task title is not being edited");
        }

        function test_review_a_stale_focus_request_does_not_stop_enter_focusing_the_field() {
            const row = checklistFixture("cl-stale-focus", ["a", "b"]);
            const editor = checklistEditor(row);
            const a = checklistItem(row, "a");
            // A request left over from a field that closed before taking it.
            editor.newFocusId = a.itemId;
            wait(30);
            mouseClick(findChild(a, "checklistTitle"));
            keyClick(Qt.Key_End);
            keyClick(Qt.Key_Return);
            const below = childrenNamed(a, "checklistNewInput")[0];
            tryVerify(() => below.visible && below.activeFocus, 1000, "the new field under a has the cursor");
            typeText("z");
            keyClick(Qt.Key_Return);
            waitForChecklist(row, ["a", "z", "b"]);
        }

        // ---- Several tasks at once (#64) -----------------------------------

        // Today tasks `titles`, the first selected, the list focused.
        function addSeveral(titles) {
            const rows = titles.map(t => addToToday(t));
            settle();
            list.positionViewAtIndex(indexOf(titles[0]), ListView.Contain);
            settle();
            view.selectOnly(rowFor(titles[0]).taskId);
            list.forceActiveFocus();
            return rows;
        }

        function selectedTitles() {
            return view.selection().map(id => list.model.titleAt(list.model.rowOf(id)));
        }

        function undoNoticeSays(text) {
            tryCompare(findChild(view, "undoText"), "text", text);
        }

        function test_shift_and_ctrl_select_several_tasks() {
            addSeveral(["ms-1", "ms-2", "ms-3", "ms-4"]);
            keyClick(Qt.Key_Down, Qt.ShiftModifier);
            keyClick(Qt.Key_Down, Qt.ShiftModifier);
            compare(selectedTitles(), ["ms-1", "ms-2", "ms-3"], "Shift+Down extends");
            compare(selectedTitle(), "ms-3", "the cursor is on the last");
            verify(rowFor("ms-2").isSelected, "shown selected");
            keyClick(Qt.Key_Up, Qt.ShiftModifier);
            compare(selectedTitles(), ["ms-1", "ms-2"], "Shift+Up shrinks it back");

            // Ctrl+click takes one out and puts one in; Shift+click extends.
            mouseClick(rowFor("ms-2"), rowFor("ms-2").width - 60, 16, Qt.LeftButton, Qt.ControlModifier);
            compare(selectedTitles(), ["ms-1"]);
            mouseClick(rowFor("ms-4"), rowFor("ms-4").width - 60, 16, Qt.LeftButton, Qt.ControlModifier);
            compare(selectedTitles(), ["ms-1", "ms-4"]);
            mouseClick(rowFor("ms-2"), rowFor("ms-2").width - 60, 16, Qt.LeftButton, Qt.ShiftModifier);
            compare(selectedTitles(), ["ms-2", "ms-3", "ms-4"], "from the last Ctrl+clicked");
            verify(!rowFor("ms-2").isExpanded, "a modified click opens nothing");

            // A plain arrow, or a plain click, selects one again.
            keyClick(Qt.Key_Down);
            compare(selectedTitles().length, 1);
            mouseClick(rowFor("ms-1"), rowFor("ms-1").width - 60, 16);
            compare(selectedTitles(), ["ms-1"]);
        }

        function test_several_tasks_are_completed_together_and_undone_at_once() {
            addSeveral(["mc-1", "mc-2", "mc-3"]);
            keyClick(Qt.Key_Down, Qt.ShiftModifier);
            keyClick(Qt.Key_Down, Qt.ShiftModifier);
            keySequence("Ctrl+.");
            for (const t of ["mc-1", "mc-2", "mc-3"])
                tryCompare(rowFor(t), "done", true);
            undoNoticeSays("3 tasks completed");
            keySequence(StandardKey.Undo);
            for (const t of ["mc-1", "mc-2", "mc-3"])
                tryCompare(rowFor(t), "done", false);
        }

        function test_several_tasks_are_trashed_together_and_undone_at_once() {
            addSeveral(["mt-1", "mt-2", "mt-3", "mt-4"]);
            const before = [];
            for (let i = 0; i < list.count; ++i)
                before.push(list.model.titleAt(i));
            keyClick(Qt.Key_Down);
            keyClick(Qt.Key_Down, Qt.ShiftModifier);
            compare(selectedTitles(), ["mt-2", "mt-3"]);
            keyClick(Qt.Key_Delete);
            waitForNoRow("mt-2");
            waitForNoRow("mt-3");
            compare(selectedTitle(), "mt-4", "the next task left is selected");
            undoNoticeSays("2 tasks moved to Trash");
            keySequence(StandardKey.Undo);
            waitForRow("mt-2");
            waitForRow("mt-3");
            const after = [];
            for (let i = 0; i < list.count; ++i)
                after.push(list.model.titleAt(i));
            compare(after, before, "back in their places");
        }

        function test_several_tasks_are_scheduled_together() {
            addSeveral(["mw-1", "mw-2", "mw-3"]);
            keyClick(Qt.Key_Down, Qt.ShiftModifier);
            const popup = openWhen();
            mouseClick(findChild(popup.contentItem, "when-someday"));
            waitForNoRow("mw-1");
            waitForNoRow("mw-2");
            verify(indexOf("mw-3") >= 0, "only the selected");
            undoNoticeSays("2 tasks scheduled");
            keySequence(StandardKey.Undo);
            waitForRow("mw-1");
            waitForRow("mw-2");

            // A typed date too.
            view.selectOnly(rowFor("mw-1").taskId);
            list.forceActiveFocus();
            keyClick(Qt.Key_Down, Qt.ShiftModifier);
            const again = openWhen();
            typeText("tomorrow");
            keyClick(Qt.Key_Return);
            waitForNoRow("mw-1");
            waitForNoRow("mw-2");
            controller.selectView("upcoming");
            waitForRow("mw-1");
            waitForRow("mw-2");
        }

        function test_several_tasks_get_a_deadline_together() {
            addSeveral(["md-1", "md-2"]);
            keyClick(Qt.Key_Down, Qt.ShiftModifier);
            const popup = findChild(view, "whenPopup");
            keyClick(Qt.Key_D, Qt.ControlModifier);
            tryVerify(() => popup.opened, 1000);
            waitForRendering(popup.contentItem);
            mouseClick(findChild(popup.contentItem, "deadline-today"));
            tryVerify(() => rowFor("md-1").deadline !== "" && rowFor("md-2").deadline !== "", 1000);
            undoNoticeSays("Deadline set for 2 tasks");
        }

        function test_several_tasks_are_moved_together() {
            controller.createArea("ms-area");
            addSeveral(["mm-1", "mm-2", "mm-3"]);
            keyClick(Qt.Key_Down, Qt.ShiftModifier);
            const popup = openMove();
            typeText("ms-area");
            keyClick(Qt.Key_Return);
            tryVerify(() => !popup.visible, 1000);
            undoNoticeSays("2 tasks moved");
            controller.selectView("anytime");
            waitForRow("mm-1");
            waitForRow("mm-2");
            waitForNoRow("mm-3");
        }

        function test_several_tasks_are_tagged_together_and_undone_at_once() {
            addSeveral(["mg-1", "mg-2", "mg-3"]);
            keyClick(Qt.Key_Down, Qt.ShiftModifier);
            keySequence("Ctrl+Shift+T");
            const popup = findChild(view, "batchTagPopup");
            tryVerify(() => popup.opened, 1000, "a tag popover for the selection");
            typeText("mg-tag");
            keyClick(Qt.Key_Return);
            tryVerify(() => !popup.visible, 1000);
            tryVerify(() => rowFor("mg-1").tagList.length === 1 && rowFor("mg-2").tagList.length === 1, 1000);
            compare(rowFor("mg-3").tagList.length, 0);
            undoNoticeSays("2 tasks tagged mg-tag");
            keySequence(StandardKey.Undo);
            tryVerify(() => rowFor("mg-1").tagList.length === 0 && rowFor("mg-2").tagList.length === 0, 1000);
        }

        function test_the_selection_survives_a_refresh_and_clears_on_navigation() {
            addSeveral(["mr-1", "mr-2"]);
            keyClick(Qt.Key_Down, Qt.ShiftModifier);
            controller.addTask("mr-new"); // the list reloads
            settle();
            compare(selectedTitles(), ["mr-1", "mr-2"]);
            controller.selectView("inbox");
            controller.selectView("today");
            settle();
            compare(view.selection(), []);
        }

        function test_dragging_a_selection_to_the_sidebar_moves_all_of_it() {
            controller.createArea("ms-drag-area");
            controller.selectView("today");
            addSeveral(["mv-d1", "mv-d2", "mv-d3"]);
            keyClick(Qt.Key_Down, Qt.ShiftModifier);
            const target = centre(shownArea("ms-drag-area"));
            dragGripToPoint(findChild(rowFor("mv-d1"), "dragGrip"), target);
            waitForNoRow("mv-d1");
            waitForNoRow("mv-d2");
            verify(indexOf("mv-d3") >= 0);
            undoNoticeSays("2 tasks moved");
        }

        // ---- Move (#54) -----------------------------------------------------

        function openMove() {
            const popup = findChild(view, "movePopup");
            keyClick(Qt.Key_M, Qt.ControlModifier | Qt.ShiftModifier);
            tryVerify(() => popup.opened, 1000, "Ctrl+Shift+M opens the Move popover");
            waitForRendering(popup.contentItem);
            return popup;
        }

        // The destinations the popover lists now, as "kind:title".
        function moveChoices(popup) {
            return popup.shown.map(d => d.kind + ":" + d.title);
        }

        function test_ctrl_shift_m_moves_the_selected_task_under_a_heading() {
            controller.createProject("mv-proj");
            const project = JSON.parse(controller.projectsJson).find(p => p.title === "mv-proj").id;
            controller.selectProject(project);
            const heading = controller.addHeading("mv-head");
            verify(heading !== "");
            controller.selectView("inbox");
            view.captureTask("mv-stay");
            view.captureTask("mv-go");
            waitForRow("mv-go");
            selectLast("mv-go");

            // Escape closes it without moving anything.
            let popup = openMove();
            verify(findChild(popup.contentItem, "moveFilter").activeFocus, "ready for typing");
            const inbox = popup.shown.find(d => d.kind === "inbox");
            verify(inbox.current, "the task's home is marked");
            keyClick(Qt.Key_Escape);
            tryVerify(() => !popup.visible, 1000);
            verify(indexOf("mv-go") >= 0);
            tryVerify(() => list.activeFocus, 1000, "focus returns to the list");

            // A project's name finds its headings too.
            popup = openMove();
            typeText("mv-proj");
            compare(moveChoices(popup), ["project:mv-proj", "heading:mv-head"]);
            keyClick(Qt.Key_Down);
            keyClick(Qt.Key_Return);

            waitForNoRow("mv-go");
            tryVerify(() => !popup.visible, 1000);
            compare(selectedTitle(), "mv-stay", "the selection moves to its neighbour");

            controller.selectProject(project);
            waitForRow("mv-go");
            compare(list.model.headingAt(indexOf("mv-go")), heading, "filed under the heading");
        }

        function test_the_move_popover_is_clicked_to_send_a_task_to_an_area() {
            createArea("mv-click-area");
            controller.selectView("inbox");
            view.captureTask("mv-click");
            waitForRow("mv-click");
            selectLast("mv-click");
            const popup = openMove();
            typeText("mv-click-area");
            const list2 = findChild(popup.contentItem, "moveDestinations");
            tryVerify(() => childrenNamed(list2.contentItem, "moveDestination").length === 1, 1000);
            mouseClick(childrenNamed(list2.contentItem, "moveDestination")[0]);
            waitForNoRow("mv-click");

            const area = JSON.parse(controller.areasJson).find(a => a.title === "mv-click-area").id;
            controller.selectArea(area);
            waitForRow("mv-click");
        }

        // Drag `grip` to `to` (a point in `view`) in both directions.
        function dragGripToPoint(grip, to, during) {
            const from = grip.mapToItem(view, grip.width / 2, grip.height / 2);
            mousePress(view, from.x, from.y);
            const steps = 16;
            for (let i = 1; i <= steps; ++i)
                mouseMove(view, from.x + (to.x - from.x) * i / steps,
                          from.y + (to.y - from.y) * i / steps, 10, Qt.LeftButton);
            if (during)
                during();
            mouseRelease(view, to.x, to.y);
        }

        function centre(item) {
            return item.mapToItem(view, item.width / 2, item.height / 2);
        }

        function sidebarProject(title) {
            const projects = findChild(view, "projectsList");
            const index = projects.model.findIndex(p => p.title === title);
            projects.positionViewAtIndex(index, ListView.Contain);
            let found = null;
            tryVerify(() => {
                found = findVisual(view, i => i.objectName === "projectItem" && i.visible
                                   && i.modelData.title === title);
                return found !== null;
            }, 2000, "'" + title + "' in the sidebar");
            return found;
        }

        // The area's row, scrolled into the visible part of the sidebar (the
        // areas are at the end; the list may still be growing).
        function shownArea(title) {
            const projects = findChild(view, "projectsList");
            let found = null;
            tryVerify(() => {
                projects.positionViewAtEnd();
                found = sidebarArea(title);
                if (found === null)
                    return false;
                const top = found.mapToItem(projects, 0, 0).y;
                return top >= 0 && top + found.height <= projects.height;
            }, 2000, "'" + title + "' in view");
            return found;
        }

        function dragTaskToSidebar(title, target) {
            controller.selectView("inbox");
            const row = waitForRow(title);
            const indicator = findChild(view, "dropIndicator");
            dragGripToPoint(findChild(row, "dragGrip"), centre(target), () => {
                compare(target.border.width, 2, "the entry under the pointer is marked");
                verify(!indicator.visible, "no drop line in the list");
            });
            waitForNoRow(title);
            // The entries are rebuilt as the counts change; nothing is marked.
            compare(view.sidebarDrop, null, "nothing is marked after the drop");
        }

        function test_dragging_a_task_onto_the_sidebar_moves_or_schedules_it() {
            controller.selectView("inbox");
            for (const t of ["sb-someday", "sb-today", "sb-project", "sb-area"])
                view.captureTask(t);
            controller.createProject("sb-proj");
            const project = JSON.parse(controller.projectsJson).find(p => p.title === "sb-proj").id;
            const area = createArea("sb-area-target");

            dragTaskToSidebar("sb-someday", findChild(view, "nav-someday"));
            dragTaskToSidebar("sb-today", findChild(view, "nav-today"));
            dragTaskToSidebar("sb-project", sidebarProject("sb-proj"));
            dragTaskToSidebar("sb-area", shownArea("sb-area-target"));

            controller.selectView("someday");
            waitForRow("sb-someday");
            controller.selectView("today");
            waitForRow("sb-today");
            controller.selectProject(project);
            waitForRow("sb-project");
            controller.selectArea(area);
            waitForRow("sb-area");
            controller.selectView("inbox");
            waitForNoRow("sb-someday");
            waitForNoRow("sb-area");
        }

        // With a long sidebar the area may be out of sight: held near the
        // bottom edge, the dragged project scrolls the sidebar to it.
        function test_dragging_a_project_onto_an_area_files_it_there() {
            controller.createProject("pa-proj");
            const area = createArea("pa-area");
            const projects = findChild(view, "projectsList");
            const grip = findChild(sidebarProject("pa-proj"), "projectGrip");
            const from = grip.mapToItem(view, grip.width / 2, grip.height / 2);
            const edge = projects.mapToItem(view, projects.width / 2, projects.height - 8);
            mousePress(view, from.x, from.y);
            for (let i = 1; i <= 12; ++i)
                mouseMove(view, from.x + (edge.x - from.x) * i / 12, from.y + (edge.y - from.y) * i / 12,
                          10, Qt.LeftButton);
            tryVerify(() => projects.atYEnd, 5000, "the sidebar scrolls to its end");
            let target = null;
            tryVerify(() => (target = sidebarArea("pa-area")) !== null, 2000);
            const to = centre(target);
            mouseMove(view, to.x, to.y, 10, Qt.LeftButton);
            mouseMove(view, to.x, to.y + 1, 10, Qt.LeftButton);
            compare(target.border.width, 2, "the area is marked");
            verify(!findChild(view, "projectDropIndicator").visible);
            mouseRelease(view, to.x, to.y + 1);
            tryVerify(() => JSON.parse(controller.projectsJson).find(p => p.title === "pa-proj").area_id === area,
                      2000, "the project is filed in the area");
            tryVerify(() => sidebarAreaProject("pa-proj") !== null, 2000, "listed under the area");
        }

        // ---- Finishing projects (#55) -----------------------------------------

        function projectIdOf(name) {
            return JSON.parse(controller.projectsJson).find(p => p.title === name).id;
        }

        function rowDone(title) {
            const row = rowFor(title);
            return row !== null && row.done;
        }

        function logbookProject(title) {
            for (const item of childrenNamed(list.contentItem, "projectRow")) {
                if (item.visible && item.title === title)
                    return item;
            }
            return null;
        }

        function test_completing_a_project_asks_about_its_open_tasks_and_reopening_restores_them() {
            openFreshProject("fin-proj", ["fin-1", "fin-2", "fin-3"]);
            const id = projectIdOf("fin-proj");
            controller.toggleTask(waitForRow("fin-1").taskId);
            tryCompare(controller, "currentProjectOpenTasks", 2);

            // Keep Open changes nothing.
            const dialog = findChild(view, "finishProjectDialog");
            menuAction("completeProjectItem");
            tryVerify(() => dialog.opened, 1000, "open tasks are asked about");
            verify(findChild(dialog, "finishProjectText").text.indexOf("2 open tasks") >= 0, "says how many");
            clickWhenShown(findChild(dialog, "keepProjectOpen"));
            tryVerify(() => !dialog.visible, 2000);
            compare(controller.currentProjectStatus, "open");

            menuAction("completeProjectItem");
            tryVerify(() => dialog.opened, 1000);
            clickWhenShown(findChild(dialog, "completeOpenTasks"));
            tryVerify(() => !dialog.visible, 2000);
            tryCompare(controller, "currentProjectStatus", "completed");
            const label = findChild(view, "projectStatusLabel");
            verify(label.visible);
            compare(label.text, "Completed");
            verify(!JSON.parse(controller.projectsJson).some(p => p.id === id), "it leaves the sidebar");

            // The Logbook lists the project, not its tasks.
            controller.selectView("logbook");
            let row = null;
            tryVerify(() => (row = logbookProject("fin-proj")) !== null, 2000, "listed in the Logbook");
            compare(findChild(row, "projectRowLabel").text, "Completed project");
            verify(indexOf("fin-1") < 0 && indexOf("fin-2") < 0);
            mouseClick(row);
            tryCompare(controller, "currentProjectId", id);

            // Reopening puts back what completing closed; fin-1 was done before.
            menuAction("completeProjectItem");
            tryCompare(controller, "currentProjectStatus", "open");
            verify(!label.visible);
            for (const t of ["fin-1", "fin-2", "fin-3"])
                waitForRow(t);
            tryVerify(() => !rowDone("fin-2") && !rowDone("fin-3") && rowDone("fin-1"), 2000,
                      "fin-2 and fin-3 open again, fin-1 still done");
            verify(JSON.parse(controller.projectsJson).some(p => p.id === id), "back in the sidebar");
        }

        function test_canceling_a_project_can_cancel_its_open_tasks() {
            openFreshProject("cancel-proj", ["cx-1"]);
            menuAction("cancelProjectItem");
            const dialog = findChild(view, "finishProjectDialog");
            tryVerify(() => dialog.opened, 1000);
            clickWhenShown(findChild(dialog, "cancelOpenTasks"));
            tryCompare(controller, "currentProjectStatus", "canceled");
            compare(findChild(view, "projectStatusLabel").text, "Canceled");
            waitForNoRow("cx-1"); // a project lists its canceled tasks nowhere
            controller.selectView("logbook");
            let row = null;
            tryVerify(() => (row = logbookProject("cancel-proj")) !== null, 2000);
            compare(findChild(row, "projectRowLabel").text, "Canceled project");
        }

        function test_a_project_with_every_task_done_offers_to_be_completed() {
            openFreshProject("all-done", ["ad-1"]);
            const button = findChild(view, "completeProjectButton");
            verify(!button.visible);
            controller.toggleTask(waitForRow("ad-1").taskId);
            tryVerify(() => button.visible, 2000, "offered once every task is done");
            clickWhenShown(button);
            // Nothing left open: nothing to ask.
            tryCompare(controller, "currentProjectStatus", "completed");
            verify(!findChild(view, "finishProjectDialog").visible);
            verify(!button.visible);
        }

        // ---- A project's When, deadline and notes (#56) -----------------------

        function projectRowNamed(title) {
            for (const item of childrenNamed(list.contentItem, "projectRow")) {
                if (item.visible && item.title === title)
                    return item;
            }
            return null;
        }

        function openProjectPill(name) {
            const popup = findChild(view, "whenPopup");
            clickWhenShown(findChild(view, name));
            tryVerify(() => popup.opened, 1000, name + " opens the popover");
            waitForRendering(popup.contentItem);
            verify(popup.forProject, "for the project, not a task");
            return popup;
        }

        function test_a_projects_when_and_deadline_are_set_from_its_header() {
            openFreshProject("pd-proj", ["pd-task"]);
            const id = projectIdOf("pd-proj");
            const when = findChild(view, "projectWhen");
            compare(findChild(when, "pillLabel").text, "When");

            // Someday: the project holds its task back and waits in Someday.
            let popup = openProjectPill("projectWhen");
            mouseClick(findChild(popup.contentItem, "when-someday"));
            tryVerify(() => !popup.visible, 2000);
            tryVerify(() => controller.currentProjectSomeday, 1000);
            compare(findChild(when, "pillLabel").text, "Someday");
            controller.selectView("anytime");
            waitForNoRow("pd-task");
            controller.selectView("someday");
            tryVerify(() => projectRowNamed("pd-proj") !== null, 2000, "listed in Someday");

            // A typed date: the project waits in Upcoming instead.
            controller.selectProject(id);
            popup = openProjectPill("projectWhen");
            typeText("in 5 days");
            keyClick(Qt.Key_Return);
            tryVerify(() => !popup.visible, 2000);
            tryVerify(() => controller.currentProjectStartDate === isoInDays(5), 1000);
            controller.selectView("upcoming");
            tryVerify(() => projectRowNamed("pd-proj") !== null, 2000, "listed in Upcoming");

            // Due today: Today lists it, with its deadline in red.
            controller.selectProject(id);
            popup = openProjectPill("projectDeadline");
            mouseClick(findChild(popup.contentItem, "deadline-today"));
            tryVerify(() => !popup.visible, 2000);
            tryCompare(controller, "currentProjectDeadlineStatus", "today");
            controller.selectView("today");
            let row = null;
            tryVerify(() => (row = projectRowNamed("pd-proj")) !== null, 2000, "listed in Today");
            verify(findChild(row, "projectRowLabel").urgent, "its deadline is shown as due");
            mouseClick(row);
            tryCompare(controller, "currentProjectId", id);

            // Cleared again: the task is back in Anytime, the project out
            // of Today.
            popup = openProjectPill("projectWhen");
            mouseClick(findChild(popup.contentItem, "when-none"));
            tryVerify(() => !popup.visible, 2000);
            tryCompare(controller, "currentProjectStartDate", "");
            popup = openProjectPill("projectDeadline");
            mouseClick(findChild(popup.contentItem, "deadline-none"));
            tryVerify(() => !popup.visible, 2000);
            tryCompare(controller, "currentProjectDeadline", "");
            controller.selectView("anytime");
            waitForRow("pd-task");
            controller.selectView("today");
            wait(0);
            tryVerify(() => projectRowNamed("pd-proj") === null, 2000, "no longer in Today");
        }

        function test_project_notes_save_as_they_are_typed_and_keep_focus() {
            openFreshProject("pn-one", ["pn-task"]);
            const one = projectIdOf("pn-one");
            controller.createProject("pn-two");
            const two = projectIdOf("pn-two");
            const notes = findChild(view, "projectNotes");
            compare(notes.text, "");

            mouseClick(notes);
            verify(notes.activeFocus);
            typeText("seed packets");
            tryCompare(controller, "currentProjectNotes", "seed packets", 3000, "saved after a pause");
            verify(notes.activeFocus, "saving doesn't take the focus");
            typeText(" and soil");

            // Leaving for another project saves what was typed there.
            controller.selectProject(two);
            tryCompare(notes, "text", "");
            controller.selectProject(one);
            tryCompare(notes, "text", "seed packets and soil");
            compare(controller.currentProjectNotes, "seed packets and soil");
        }

        // ---- Tags (#57) -----------------------------------------------------------

        function chipNames(item, name) {
            return childrenNamed(item, name).filter(c => c.visible).map(c => c.name);
        }

        function tagIdNamed(name) {
            const tag = JSON.parse(controller.tagsJson).find(t => t.name === name);
            return tag ? tag.id : "";
        }

        // Ctrl+Shift+T on the selected task: its tag field takes the keys.
        function openTagsOf(title) {
            selectLast(title);
            keyClick(Qt.Key_T, Qt.ControlModifier | Qt.ShiftModifier);
            const row = rowFor(title);
            tryVerify(() => row.isExpanded, 1000, "the task opens");
            const input = findChild(findChild(row, "taskTags"), "tagInput");
            tryVerify(() => input.activeFocus, 2000, "its tag field has the cursor");
            return row;
        }

        function test_a_task_is_tagged_and_untagged_by_keyboard() {
            addToToday("tg-task");
            const row = openTagsOf("tg-task");
            const field = findChild(row, "taskTags");

            typeText("tg-errands");
            keyClick(Qt.Key_Return);
            tryVerify(() => chipNames(field, "tagChip").indexOf("tg-errands") >= 0, 2000, "a new tag, given");
            verify(tagIdNamed("tg-errands") !== "", "and created");
            typeText("tg-home");
            keyClick(Qt.Key_Return);
            tryVerify(() => chipNames(field, "tagChip").length === 2, 2000);

            // Backspace in the empty field takes off the last tag.
            keyClick(Qt.Key_Backspace);
            tryVerify(() => compareArrays(chipNames(field, "tagChip"), ["tg-errands"]), 2000, "tg-home taken off");
            verify(tagIdNamed("tg-home") !== "", "the tag itself is kept");

            // Escape clears what was typed, then closes the task like any
            // editor in it; closed, the row shows its tags.
            typeText("tg-half");
            keyClick(Qt.Key_Escape);
            compare(findChild(field, "tagInput").text, "");
            verify(row.isExpanded);
            keyClick(Qt.Key_Escape);
            tryVerify(() => !row.isExpanded, 1000);
            tryVerify(() => list.activeFocus, 1000);
            verify(view.Window.activeFocusItem.visible, "the keys go to something on screen, not the hidden field");
            tryVerify(() => compareArrays(chipNames(row, "rowTag"), ["tg-errands"]), 2000, "shown on the row");
        }

        function compareArrays(a, b) {
            return JSON.stringify(a) === JSON.stringify(b);
        }

        function test_typing_suggests_existing_tags() {
            controller.createTag("tg-hardware", "");
            controller.createTag("tg-hardly", "");
            const count = JSON.parse(controller.tagsJson).length;
            addToToday("tg-suggest");
            const row = openTagsOf("tg-suggest");
            const field = findChild(row, "taskTags");

            typeText("tg-hard");
            tryVerify(() => chipNames(field, "tagSuggestion").length === 2, 1000, "both suggested");
            keyClick(Qt.Key_Down); // the second
            keyClick(Qt.Key_Return);
            tryVerify(() => compareArrays(chipNames(field, "tagChip"), ["tg-hardly"]), 2000);
            compare(JSON.parse(controller.tagsJson).length, count, "no new tag");
            // What the task has is no longer suggested.
            typeText("tg-hard");
            tryVerify(() => compareArrays(chipNames(field, "tagSuggestion"), ["tg-hardware"]), 1000);
            keyClick(Qt.Key_Escape);
        }

        function test_projects_and_areas_take_tags_in_their_headers() {
            openFreshProject("tg-proj", ["tg-in-proj"]);
            const projectField = findChild(view, "projectTags");
            mouseClick(findChild(projectField, "tagInput"));
            typeText("tg-project-tag");
            keyClick(Qt.Key_Return);
            tryVerify(() => compareArrays(chipNames(projectField, "tagChip"), ["tg-project-tag"]), 2000);

            const area = createArea("tg-area");
            controller.selectArea(area);
            const areaField = findChild(view, "areaTags");
            tryVerify(() => areaField.visible, 1000);
            compare(chipNames(areaField, "tagChip").length, 0, "the area's own tags");
            mouseClick(findChild(areaField, "tagInput"));
            typeText("tg-area-tag");
            keyClick(Qt.Key_Return);
            tryVerify(() => compareArrays(chipNames(areaField, "tagChip"), ["tg-area-tag"]), 2000);
            mouseClick(childrenNamed(areaField, "removeTag")[0]);
            tryVerify(() => chipNames(areaField, "tagChip").length === 0, 2000, "taken off by its button");
        }

        function tagRowNamed(name) {
            const dialog = findChild(view, "tagsDialog");
            return childrenNamed(dialog.contentItem, "tagRow").find(r => r.visible && r.name === name) || null;
        }

        function test_the_tags_dialog_renames_nests_and_deletes() {
            const parent = controller.createTag("tg-dlg-parent", "");
            const child = controller.createTag("tg-dlg-child", "");
            addToToday("tg-dlg-task");
            controller.addTag("task", rowFor("tg-dlg-task").taskId, child);

            clickWhenShown(findChild(view, "tagsButton"));
            const dialog = findChild(view, "tagsDialog");
            tryVerify(() => dialog.opened, 1000);

            // Nest the child under the parent.
            let row = null;
            tryVerify(() => (row = tagRowNamed("tg-dlg-child")) !== null, 1000);
            mouseClick(findChild(row, "nestTag"));
            const under = findChild(row, "nestUnder-" + parent);
            tryVerify(() => under.visible, 1000);
            mouseClick(under);
            tryVerify(() => JSON.parse(controller.tagsJson).find(t => t.id === child).parent_id === parent, 2000);

            // Rename it in place.
            tryVerify(() => (row = tagRowNamed("tg-dlg-child")) !== null, 1000);
            const name = findChild(row, "tagName");
            mouseClick(name);
            keyClick(Qt.Key_A, Qt.ControlModifier);
            typeText("tg-dlg-renamed");
            keyClick(Qt.Key_Return);
            tryVerify(() => tagIdNamed("tg-dlg-renamed") === child, 2000);

            // Delete the parent: the child moves up, the task keeps its tag.
            tryVerify(() => (row = tagRowNamed("tg-dlg-parent")) !== null, 1000);
            mouseClick(findChild(row, "deleteTag"));
            tryVerify(() => tagIdNamed("tg-dlg-parent") === "", 2000);
            compare(JSON.parse(controller.tagsJson).find(t => t.id === child).parent_id, null);
            dialog.close();
            tryVerify(() => !dialog.visible, 2000);
            const task = rowFor("tg-dlg-task");
            tryVerify(() => compareArrays(chipNames(task, "rowTag"), ["tg-dlg-renamed"]), 2000);
        }

        // ---- Filtering by tag (#58) ---------------------------------------------

        function filterChip(name) {
            return childrenNamed(findChild(view, "tagBar"), "filterTag").find(c => c.visible && c.name === name) || null;
        }

        function clickFilter(name) {
            let chip = null;
            tryVerify(() => (chip = filterChip(name)) !== null, 2000, name + " offered in the tag bar");
            mouseClick(chip);
        }

        function test_a_list_is_narrowed_to_a_tag_and_cleared_on_navigation() {
            const parent = controller.createTag("tf-errands", "");
            const child = controller.createTag("tf-hardware", parent);
            const a = addToToday("tf-a").taskId;
            const b = addToToday("tf-b").taskId;
            addToToday("tf-c");
            controller.addTag("task", a, child);
            controller.addTag("task", b, parent);
            settle();
            const before = indexOf("tf-a") < indexOf("tf-b");

            // A parent narrows to its children too, in the same order.
            clickFilter("tf-errands");
            waitForNoRow("tf-c");
            verify(indexOf("tf-a") >= 0 && indexOf("tf-b") >= 0);
            compare(indexOf("tf-a") < indexOf("tf-b"), before, "the order is kept");
            verify(filterChip("tf-errands").chosen);

            clickFilter("tf-hardware");
            waitForNoRow("tf-b");
            verify(indexOf("tf-a") >= 0);

            // Choosing it again shows everything.
            clickFilter("tf-hardware");
            waitForRow("tf-c");
            compare(view.tagFilter, "");

            // Leaving the view clears the filter.
            clickFilter("tf-errands");
            waitForNoRow("tf-c");
            controller.selectView("inbox");
            controller.selectView("today");
            waitForRow("tf-c");
            compare(view.tagFilter, "");
            verify(filterChip("All").chosen);
        }

        function test_a_projects_tag_narrows_lists_to_its_tasks() {
            openFreshProject("tf-proj", ["tf-in-proj"]);
            const project = projectIdOf("tf-proj");
            const tag = controller.createTag("tf-project-tag", "");
            controller.addTag("project", project, tag);
            controller.selectView("anytime");
            waitForRow("tf-in-proj");
            clickFilter("tf-project-tag");
            tryVerify(() => {
                for (let i = 0; i < list.count; ++i) {
                    const title = list.model.titleAt(i);
                    if (title !== "" && title !== "tf-in-proj")
                        return false;
                }
                return indexOf("tf-in-proj") >= 0;
            }, 2000, "only the project's task, which inherits its tag");
        }

        // ---- Arranging the sidebar (#164) ----------------------------------------

        function areaOrder() {
            return JSON.parse(controller.areasJson).map(a => a.title);
        }

        function areaProjects(areaId) {
            return JSON.parse(controller.projectsJson).filter(p => p.area_id === areaId).map(p => p.title);
        }

        function test_areas_are_reordered_by_dragging() {
            createArea("sd-first");
            createArea("sd-second");
            tryVerify(() => areaOrder().indexOf("sd-second") === areaOrder().indexOf("sd-first") + 1, 2000);
            shownArea("sd-second");
            wait(50); // laid out before aiming
            const first = sidebarArea("sd-first");
            const second = sidebarArea("sd-second");
            // Just above the middle of sd-first: before it.
            const to = first.mapToItem(view, first.width / 2, first.height / 2 - 4);
            dragGripTo(findChild(second, "areaGrip"), to,
                       () => verify(findChild(view, "projectDropIndicator").visible, "a line shows where it lands"));
            tryVerify(() => areaOrder().indexOf("sd-second") === areaOrder().indexOf("sd-first") - 1, 2000,
                      "sd-second moved above sd-first");
        }

        function test_a_project_is_dragged_within_its_area_and_into_another() {
            const one = createArea("sd-one");
            const two = createArea("sd-two");
            for (const [title, area] of [["sd-p1", one], ["sd-p2", one], ["sd-p3", two]]) {
                controller.createProject(title);
                controller.setProjectArea(projectIdOf(title), area);
            }
            tryVerify(() => areaProjects(one).join() === "sd-p1,sd-p2", 2000);

            // Within its area: p2 above p1.
            shownArea("sd-two");
            let p1 = null, p2 = null;
            tryVerify(() => (p1 = sidebarAreaProject("sd-p1")) !== null && (p2 = sidebarAreaProject("sd-p2")) !== null, 2000);
            wait(50);
            dragGripTo(findChild(p2, "projectGrip"), p1.mapToItem(view, p1.width / 2, p1.height / 2 - 4));
            tryVerify(() => areaProjects(one).join() === "sd-p2,sd-p1", 2000, "reordered within sd-one");

            // Into another area, at a place: p3 (sd-two) below p2 (sd-one).
            shownArea("sd-two");
            let p3 = null;
            tryVerify(() => (p2 = sidebarAreaProject("sd-p2")) !== null && (p3 = sidebarAreaProject("sd-p3")) !== null, 2000);
            wait(50);
            dragGripTo(findChild(p3, "projectGrip"), p2.mapToItem(view, p2.width / 2, p2.height / 2 + 4));
            tryVerify(() => areaProjects(one).join() === "sd-p2,sd-p3,sd-p1", 2000, "filed in sd-one after sd-p2");
            compare(areaProjects(two).length, 0);
        }

        // ---- Completing the hierarchy (#169) ----------------------------------

        function test_a_heading_moves_to_another_project_with_its_tasks() {
            controller.createProject("hm-target");
            const target = projectIdOf("hm-target");
            openFreshProject("hm-source", ["hm-under", "hm-above"]);
            const heading = controller.addHeading("hm-heading");
            controller.placeProjectTask(rowFor("hm-under").taskId, heading, "");
            tryVerify(() => headingSection("hm-heading") !== null, 2000);

            const section = headingSection("hm-heading");
            const button = findChild(section, "headingMenuButton");
            clickWhenShown(button);
            const menu = findChild(button, "headingMenu");
            tryVerify(() => menu.opened, 1000);
            const submenu = findChild(button, "moveHeadingToProjectMenu");
            submenu.open();
            const item = findChild(button, "moveHeadingTo-" + target);
            tryVerify(() => item.visible, 1000, "the projects are offered");
            waitForRendering(view);
            mouseClick(item);
            tryVerify(() => !menu.visible, 2000);

            waitForNoRow("hm-under");
            verify(indexOf("hm-above") >= 0, "the task above the headings stays");
            controller.selectProject(target);
            waitForRow("hm-under");
            tryVerify(() => headingSection("hm-heading") !== null, 2000, "the heading came along");
        }

        function test_a_deleted_checklist_item_comes_back_with_undo() {
            const row = checklistFixture("cl-undo", ["keep", "drop", "last"]);
            controller.setChecklistItemDone(checklistItem(row, "drop").itemId, true);
            hoverDelete(checklistItem(row, "drop"));
            waitForChecklist(row, ["keep", "last"]);
            tryCompare(view, "undoKind", "checklist");
            compare(findChild(view, "undoText").text, "Checklist item “drop” deleted");
            const notice = findChild(view, "undoButton");
            mouseClick(notice);
            waitForChecklist(row, ["keep", "drop", "last"], "back in its place");
            verify(checklistItem(row, "drop").done, "still ticked");
            compare(view.undoKind, "");
        }
    }
}
