import QtQuick
import QtTest
import matters
import "sweep.js" as Sweep

// Every string Matters shows can be translated (#75). With the pseudo
// catalog `build.rs` makes from translations/matters.ts, every translated
// string reads ⟦…⟧, so a string still in plain English was never marked
// for translation, or is missing from the catalog. In every view and with
// every dialog and popover open, each name and description a screen
// reader is given (which covers the window's text) and each placeholder
// must be translated, a date or name from the locale, or the test's own
// data. In the right-to-left catalog the window is mirrored.
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

    TestTranslations {
        id: translations
    }

    TestCase {
        id: testCase
        name: "Translation"
        when: windowShown

        readonly property var controller: view.appController

        // The test's own data, and the fake calendar's (tests/ui.rs): not
        // Matters' strings.
        readonly property var testData: ["Team standup", "Public holiday", "Dentist", "Conference", "Work", "Home",
            "file:///etc/hosts"]
        // Qt's own words, translated by Qt's catalog (qtbase), which Matters
        // installs for the user's language; and the test window's title.
        readonly property var qtWords: ["OK", "Cancel", "Close", "tst_translation"]

        function name(text) {
            return "⟦" + text + "⟧";
        }

        // What the locale writes itself: month and day names, AM and PM.
        function localeWords() {
            const locale = Qt.locale();
            const words = [locale.amText, locale.pmText];
            for (let m = 0; m < 12; ++m) {
                for (const format of [Locale.LongFormat, Locale.ShortFormat, Locale.NarrowFormat]) {
                    words.push(locale.monthName(m, format), locale.standaloneMonthName(m, format));
                }
            }
            for (let d = 0; d < 7; ++d) {
                for (const format of [Locale.LongFormat, Locale.ShortFormat, Locale.NarrowFormat]) {
                    words.push(locale.dayName(d, format), locale.standaloneDayName(d, format));
                }
            }
            // Longest first, so "November" goes before "Nov".
            return words.filter(w => w !== "").sort((a, b) => b.length - a.length);
        }

        // Every name the user (here, any test) gave something: the lists'
        // rows, projects, areas, tags, headings and what is in the Trash.
        // The test files share one database, so other files' data shows.
        function userTitles() {
            const out = [];
            const parse = json => {
                try {
                    return JSON.parse(json);
                } catch (e) {
                    return [];
                }
            };
            for (const json of [controller.projectsJson, controller.areasJson, controller.trashedProjectsJson,
                                controller.trashedAreasJson])
                for (const item of parse(json))
                    out.push(item.title);
            for (const tag of parse(controller.tagsJson))
                out.push(tag.name);
            const list = view.taskList;
            // Each row's own title too: a finished project in the Logbook
            // is a row, not an open project.
            for (let i = 0; i < list.count; ++i) {
                out.push(list.model.titleAt(i));
                const row = list.itemAtIndex(i);
                if (row && row.title !== undefined)
                    out.push(row.title);
            }
            if (list.count > 0 && list.model.idAt(0) !== "") {
                for (const d of parse(controller.moveDestinations(list.model.idAt(0))))
                    out.push(d.title);
            }
            return out.filter(t => t !== undefined && t !== "");
        }

        // `text` with what is translated, the locale's and the data taken
        // out: empty when nothing is left that should have been translated.
        function untranslated(text, words) {
            // Translations can hold translations (a phrase with a day in
            // it): take out the innermost first.
            let rest = String(text);
            for (let before = ""; before !== rest;) {
                before = rest;
                rest = rest.replace(/⟦[^⟦⟧]*⟧/g, " ");
            }
            if (qtWords.indexOf(rest.trim()) >= 0)
                return "";
            for (const w of testData.concat(words).sort((a, b) => b.length - a.length))
                rest = rest.split(w).join(" ");
            // Latin, Greek, Cyrillic, Hebrew, Arabic, kana and Han letters
            // (QML's JavaScript has no \p{L}).
            return /[A-Za-z\u00C0-\u024F\u0370-\u03FF\u0400-\u04FF\u0590-\u06FF\u3040-\u30FF\u4E00-\u9FFF]/.test(rest)
                ? String(text) : "";
        }

        property var found: ({})

        function note(text, where) {
            const now = found;
            if (!now[text])
                now[text] = [];
            if (now[text].indexOf(where) < 0)
                now[text].push(where);
            found = now;
        }

        // Placeholders, which the accessibility tree doesn't carry.
        function placeholders(item, out) {
            if (!item || !item.visible)
                return;
            if (item.placeholderText !== undefined && item.placeholderText !== "")
                out.push(item.placeholderText);
            for (let i = 0; i < item.children.length; ++i)
                placeholders(item.children[i], out);
        }

        function look(where) {
            wait(view.reduceMotion ? 30 : 260);
            const words = localeWords().concat(userTitles());
            const texts = [];
            for (const e of JSON.parse(probe.report()).elements)
                texts.push(e.name, e.description);
            placeholders(view.Window.window.contentItem, texts);
            for (const text of texts) {
                const plain = untranslated(text, words);
                if (plain !== "")
                    note(plain, where);
            }
        }

        function initTestCase() {
            verify(translations.useCatalog("pseudo"), "the pseudo catalog is built in");
            Sweep.setup(testCase, view, name);
        }

        function cleanupTestCase() {
            translations.useCatalog("en");
            Sweep.teardown(testCase, view, name, ["fit"]);
        }

        function test_the_check_sees_what_isnt_translated() {
            const words = localeWords();
            compare(untranslated("⟦Ťóďáý···⟧", words), "");
            compare(untranslated("⟦Řéḿíñďéř ƒóř···⟧ 09:00", words), "");
            compare(untranslated("Today", words), "Today");
            compare(untranslated(Qt.locale().monthName(10, Locale.ShortFormat) + " 21", words), "");
            compare(untranslated("Team standup", words), "");
            // The pseudo catalog is in use: a view's name reads ⟦…⟧.
            compare(findChild(view, "nav-inbox").Accessible.name.indexOf("⟦"), 0);
        }

        function test_every_string_is_translated() {
            found = {};
            Sweep.run(testCase, view, look, name);
            // QtTest shortens long failure messages, so each string is
            // also reported on its own.
            const lines = Object.keys(found).map(t => JSON.stringify(t) + " [" + found[t].join(", ") + "]");
            for (const line of lines)
                warn(line);
            verify(lines.length === 0, lines.length + " untranslated, listed above; the first: " + lines[0]);
        }

        // What sticks out of `box`: each visible item with text in `item`
        // (not in a scrolling list, whose rows may go on past the edge)
        // that starts before its left edge or ends past its right.
        function overflowing(item, box, out) {
            if (!item || !item.visible || item.opacity === 0)
                return out;
            if (item.contentHeight !== undefined && item.flickableDirection !== undefined)
                return out;
            if (item.text !== undefined && item.width > 0) {
                const left = item.mapToItem(box, 0, 0).x;
                const right = left + item.width;
                if (left < -0.5 || right > box.width + 0.5)
                    out.push((item.objectName || String(item.text)) + " spans " + Math.round(left) + " to " + Math.round(right)
                        + " of " + Math.round(box.width));
            }
            for (let i = 0; i < item.children.length; ++i)
                overflowing(item.children[i], box, out);
            return out;
        }

        function test_the_overflow_check_sees_overflow() {
            const box = Qt.createQmlObject('import QtQuick; Item { width: 50; height: 20; Text { objectName: "wide"; x: 10; width: 60; text: "long" } }',
                view, "overflowBox");
            try {
                compare(overflowing(box, box, []).length, 1, "a text past the edge is found");
                box.children[0].width = 30;
                compare(overflowing(box, box, []).length, 0, "one inside isn't");
                box.children[0].x = -5;
                compare(overflowing(box, box, []).length, 1, "one that starts before it is");
            } finally {
                box.destroy();
            }
        }

        // Translations a third longer still fit: nothing in the sidebar or
        // the popovers and dialogs runs past their edges, left to right or
        // right to left.
        // Both directions in one test (ci/ui-lane.sh counts test functions,
        // so no data rows).
        function test_long_translations_fit() {
            const found = [];
            for (const [direction, catalog] of [["left to right", "pseudo"], ["right to left", "pseudo_rtl"]])
                fitting(catalog, direction, found);
            for (const line of found)
                warn(line);
            verify(found.length === 0, found.length + " overflow, listed above; the first: " + found[0]);
        }

        function fitting(catalog, direction, found) {
            verify(translations.useCatalog(catalog));
            try {
                controller.selectView("inbox");
                view.captureTask(name("fit"));
                wait(30);
                const list = view.taskList;
                let id = "";
                for (let i = 0; i < list.count && id === ""; ++i) {
                    if (list.model.titleAt(i) === name("fit"))
                        id = list.model.idAt(i);
                }
                verify(id !== "", "the task is listed");
                const sidebar = findChild(view, "sidebar");
                for (const s of overflowing(sidebar, sidebar, []))
                    found.push(direction + ", sidebar: " + s);
                const popups = [
                    ["whenPopup", () => view.openWhenFor(id), "When"],
                    ["whenPopup", () => view.openDeadlineFor(id), "Deadline"],
                    ["preferencesDialog", p => p.open(), "Preferences"],
                    ["repeatDialog", p => p.edit([id], JSON.stringify({
                        every: 2, frequency: { kind: "weekly", days: [1, 3] }, after_completion: false,
                        start: "2026-01-05", until: "2027-01-01"
                    }), "2026-01-05"), "Repeat"],
                    ["newAreaDialog", p => p.open(), "New Area"],
                    ["emptyTrashDialog", p => p.open(), "Empty Trash"],
                    ["movePopup", () => view.openMoveFor(id), "Move"],
                    ["taskMenu", () => view.openTaskMenu(id, Qt.point(40, 40), list), "the task menu"]
                ];
                for (const [objectName, open, what] of popups) {
                    const popup = findChild(view, objectName);
                    open(popup);
                    tryVerify(() => popup.opened, 2000, what + " opens");
                    waitForRendering(popup.contentItem);
                    const box = popup.background || popup.contentItem;
                    for (const s of overflowing(popup.contentItem, box, []))
                        found.push(direction + ", " + what + ": " + s);
                    popup.close();
                    tryVerify(() => !popup.visible, 2000);
                }
            } finally {
                translations.useCatalog("pseudo");
            }
        }

        // Right to left: the window is mirrored, the sidebar on the right
        // and a row's circle at its right end.
        function test_right_to_left_mirrors_the_window() {
            verify(translations.useCatalog("pseudo_rtl"));
            try {
                tryCompare(Qt.application, "layoutDirection", Qt.RightToLeft);
                controller.selectView("inbox");
                wait(30);
                const sidebar = findChild(view, "sidebar");
                const list = view.taskList;
                verify(sidebar.mapToItem(view, 0, 0).x > list.mapToItem(view, 0, 0).x,
                    "the sidebar is on the right");
                let row = null;
                for (let i = 0; i < list.count && !row; ++i)
                    row = list.itemAtIndex(i);
                verify(row, "a row is shown");
                const circle = findChild(row, "checkCircle");
                verify(circle.mapToItem(row, 0, 0).x > row.width / 2, "the circle is at the row's right end");
            } finally {
                translations.useCatalog("pseudo");
                tryCompare(Qt.application, "layoutDirection", Qt.LeftToRight);
            }
        }
    }
}
