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
                                "emptyTrashDialog", "quickFindDialog"]) {
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
            compare(list.selectedTaskId, list.model.idAt(0), "Down selects the first task");
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
            mouseClick(sidebarArea("Home Base"));
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
    }
}
