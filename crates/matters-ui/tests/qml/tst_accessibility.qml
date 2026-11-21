import QtQuick
import QtTest
import matters
import "sweep.js" as Sweep

// What a screen reader is told (#74). Qt's accessibility tree, the one Orca
// reads over AT-SPI, is checked in each view and with each dialog and
// popover open: every control a screen reader can act on has a name, the
// keyboard can reach it, and nothing that can be clicked is hidden from
// the screen reader.
Item {
    width: 960
    height: 680

    MainView {
        id: view
        anchors.fill: parent
    }

    AccessibilityProbe {
        id: probe
    }

    TestCase {
        id: testCase
        name: "Accessibility"
        when: windowShown

        readonly property var controller: view.appController
        readonly property var list: view.taskList

        // QAccessible::Role values a screen reader can act on.
        readonly property var interactiveRoles: ({
            0x0C: "menu item", 0x1E: "link", 0x22: "list item", 0x25: "tab",
            0x2A: "text field", 0x2B: "button", 0x2C: "check box", 0x2D: "radio button",
            0x2E: "combo box", 0x33: "slider", 0x34: "spin box", 0x38: "drop-down button",
            0x39: "menu button", 0x87: "switch"
        })

        function report() {
            return JSON.parse(probe.report());
        }

        // What is wrong in the current state: nameless controls and
        // mouse-only areas, each described once.
        function problems() {
            const r = report();
            const found = [];
            for (const e of r.elements) {
                const kind = interactiveRoles[e.role];
                // A combo box's or spin box's own text field is part of it.
                const part = e.role === 0x2A && (e.inside === 0x2E || e.inside === 0x34);
                if (kind !== undefined && e.name.trim() === "" && !part)
                    found.push("a " + kind + " with no name (" + e.type.replace(/_QML.*/, "")
                        + (e.objectName !== "" ? " " + e.objectName : "") + ", in " + e.within + ")");
            }
            // The keyboard reaches every other control. A list's items and a
            // menu's are reached through the list or menu, with the arrow
            // keys, rather than one by one.
            for (const e of r.elements) {
                const kind = interactiveRoles[e.role];
                if (kind !== undefined && !e.focusable && !e.disabled && e.role !== 0x22 && e.role !== 0x0C)
                    found.push("the keyboard can't reach the " + kind + " " + e.name + " ("
                        + e.type.replace(/_QML.*/, "") + " " + e.objectName + ", in " + e.within + ")");
            }
            for (const w of r.mouseOnly)
                found.push("only a mouse can use a control in " + w);
            return found;
        }

        function settle() {
            wait(view.reduceMotion ? 30 : 260);
        }

        function check(where) {
            settle();
            const found = problems(where);
            verify(found.length === 0, "\n" + found.join("\n"));
        }

        function initTestCase() {
            Sweep.setup(testCase, view, s => "a11y " + s);
            settle();
        }

        function cleanupTestCase() {
            Sweep.teardown(testCase, view, s => "a11y " + s, ["cursor first", "cursor second"]);
        }

        // Every state is checked before the test fails, so one run lists
        // everything that is wrong: each problem with where it was met.
        property var found: ({})

        function look(where) {
            settle();
            const now = found;
            for (const problem of problems()) {
                if (!now[problem])
                    now[problem] = [];
                if (now[problem].indexOf(where) < 0)
                    now[problem].push(where);
            }
            found = now;
        }

        function ring(item) {
            for (let i = 0; i < item.children.length; ++i) {
                const c = item.children[i];
                if (c.objectName === "focusRing")
                    return c;
            }
            return null;
        }

        // Tab reaches the sidebar's entries, the focused one has a ring, and
        // Space or Enter opens it.
        function test_the_sidebar_works_from_the_keyboard() {
            controller.selectView("inbox");
            settle();
            const quickFind = findChild(view, "quickFindButton");
            quickFind.forceActiveFocus(Qt.TabFocusReason);
            verify(ring(quickFind).visible, "the focused entry has a ring");
            keyClick(Qt.Key_Tab);
            const inbox = findChild(view, "nav-inbox");
            verify(inbox.activeFocus, "Tab goes on to the Inbox");
            verify(!ring(quickFind).visible && ring(inbox).visible, "the ring moves with the focus");
            keyClick(Qt.Key_Tab);
            verify(findChild(view, "nav-today").activeFocus, "and on to Today");
            keyClick(Qt.Key_Space);
            tryCompare(controller, "currentView", "today");
            keyClick(Qt.Key_Tab);
            keyClick(Qt.Key_Return);
            tryCompare(controller, "currentView", "upcoming");

            const area = findChild(view, "areaItem");
            area.forceActiveFocus(Qt.TabFocusReason);
            keyClick(Qt.Key_Enter);
            tryCompare(controller, "currentView", "area");

            const quick = findChild(view, "quickFindDialog");
            quickFind.forceActiveFocus(Qt.TabFocusReason);
            keyClick(Qt.Key_Space);
            tryVerify(() => quick.opened, 2000, "Space on Quick Find opens it");
            quick.close();
            tryVerify(() => !quick.visible, 2000);
        }

        function focusedRows() {
            return report().elements.filter(e => e.role === 0x22 && e.focused && e.within === "taskRow").map(e => e.name);
        }

        // The task list keeps the keyboard focus and moves its own cursor;
        // the row under it is the one a screen reader is told has the
        // focus, so Orca reads each task as Up and Down reach it.
        function test_a_screen_reader_follows_the_list_cursor() {
            controller.selectView("today");
            view.captureTask("a11y cursor first");
            view.captureTask("a11y cursor second");
            settle();
            const list = view.taskList;
            list.forceActiveFocus();
            list.selectedTaskId = "";
            const titles = [];
            for (let i = 0; i < list.count; ++i)
                titles.push(list.model.titleAt(i));
            const first = titles.indexOf("a11y cursor first");
            list.selectedTaskId = list.itemAtIndex(first).taskId;
            settle();
            compare(focusedRows(), ["a11y cursor first"]);
            keyClick(Qt.Key_Down);
            settle();
            compare(focusedRows(), [titles[first + 1]], "Down moves it");
            keyClick(Qt.Key_Up);
            settle();
            compare(focusedRows(), ["a11y cursor first"], "Up moves it back");
            // Focus elsewhere: no row claims it.
            findChild(view, "quickFindButton").forceActiveFocus(Qt.TabFocusReason);
            settle();
            compare(focusedRows(), []);
        }

        function test_everything_a_screen_reader_meets() {
            found = {};
            Sweep.run(testCase, view, look, s => "a11y " + s);

            // QtTest shortens long failure messages, so each problem is
            // also reported on its own.
            const lines = Object.keys(found).map(p => p + " [" + found[p].join(", ") + "]");
            for (const line of lines)
                warn(line);
            verify(lines.length === 0, lines.length + " problems, listed above; the first: " + lines[0]);
        }
    }
}
