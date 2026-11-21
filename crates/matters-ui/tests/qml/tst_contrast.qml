import QtQuick
import QtTest
import matters

// The palette is readable (#74, #76): in light and in dark, with Matters'
// own accent or any the desktop offers, every text colour meets WCAG 2.2
// AA (4.5:1) on every surface it is drawn on, and the accent, which also
// draws focus rings and ticked boxes, meets the 3:1 of non-text contrast
// against every surface. When the desktop asks for higher contrast, text
// is held to AAA (7:1), the accent's marks to 4.5:1 and lines to 3:1.
// Every palette token is in a pair here, or named as a line nothing needs
// to read, so a new colour can't skip the check. qml_lint keeps every
// colour in the palette.
Item {
    width: 960
    height: 680

    MainView {
        id: view
        anchors.fill: parent
    }

    FakeDesktop {
        id: desktop
    }

    TestCase {
        name: "Contrast"
        when: windowShown

        readonly property var controller: view.appController

        // Every surface text is drawn on.
        readonly property var surfaces: ["colBg", "colSidebarBg", "colSidebarHover", "colSidebarActive",
            "colCardBg", "colRowHover", "colRowSelected", "colChip", "colBadge", "colField",
            "colControl", "colInputBar", "colDangerBg", "colEveningBg"]
        // Lines that only separate or frame: nothing is read from them
        // (unless the desktop asks for higher contrast).
        readonly property var decorative: ["colBorder", "colRowSelectedBorder"]

        // [text, surfaces it is drawn on, the ratio it needs]
        function pairs(high) {
            const text = high ? 7 : 4.5;
            const list = [
                ["colTextPrimary", surfaces, text],
                ["colTextSecondary", surfaces, text],
                ["colDanger", surfaces, text],
                // The accent as text: selected sidebar entries, headings,
                // links and previews in popovers.
                ["colAccent", ["colBg", "colSidebarBg", "colSidebarHover", "colSidebarActive", "colCardBg",
                    "colField", "colControl", "colInputBar", "colChip"], text],
                // The accent as a focus ring, a ticked box or a selection mark.
                ["colAccent", surfaces, high ? 4.5 : 3],
                ["colOnAccent", ["colAccent"], 4.5],
                // This Evening's heading, and its pill in a row.
                ["colEvening", ["colBg", "colCardBg", "colEveningBg", "colRowHover", "colRowSelected"], text],
                ["colTooltipText", ["colTooltipBg"], 4.5],
                ["colInverseText", ["colInverseBg"], 4.5]
            ];
            if (high)
                list.push(["colBorder", ["colBg", "colCardBg", "colSidebarBg"], 3]);
            return list;
        }

        // Accents desktops offer, the hard ones included: this desktop's
        // pale beige, yellow, white, black.
        readonly property var accents: ["", "#cabab4", "#3584e4", "#e01b24", "#f6d32d", "#2ec27e",
            "#ffffff", "#000000"]

        function luminance(c) {
            const linear = v => v <= 0.04045 ? v / 12.92 : Math.pow((v + 0.055) / 1.055, 2.4);
            return 0.2126 * linear(c.r) + 0.7152 * linear(c.g) + 0.0722 * linear(c.b);
        }

        function ratio(a, b) {
            const la = luminance(a), lb = luminance(b);
            return (Math.max(la, lb) + 0.05) / (Math.min(la, lb) + 0.05);
        }

        function cleanup() {
            desktop.set("", false);
            controller.chooseFollowAccent(true);
            controller.chooseColorScheme("system");
        }

        function test_the_formula() {
            // Black on white is 21:1; a colour on itself 1:1.
            fuzzyCompare(ratio(Qt.color("#000000"), Qt.color("#FFFFFF")), 21, 0.001);
            fuzzyCompare(ratio(Qt.color("#777777"), Qt.color("#777777")), 1, 0.001);
            // A published example: #767676 on white is 4.54:1.
            fuzzyCompare(ratio(Qt.color("#767676"), Qt.color("#FFFFFF")), 4.54, 0.01);
        }

        function test_every_token_is_checked() {
            const tokens = Object.keys(view).filter(k => /^col[A-Z]/.test(k) && !k.endsWith("Changed"));
            verify(tokens.length > 20, "the palette's tokens are found: " + tokens.length);
            const checked = new Set(decorative);
            for (const [text, on] of pairs(false)) {
                checked.add(text);
                for (const s of on)
                    checked.add(s);
            }
            const unchecked = tokens.filter(t => !checked.has(t));
            compare(unchecked.length, 0, "tokens in no pair: " + unchecked.join(", "));
            for (const t of checked)
                verify(tokens.indexOf(t) >= 0, t + " is a token");
        }

        // Without a desktop accent or a contrast preference, the palette is
        // Matters' own, unchanged (#76).
        function test_without_the_desktops_choices_the_palette_is_matters_own() {
            controller.chooseColorScheme("light");
            compare(String(view.colAccent), "#1864ab");
            compare(String(view.colOnAccent), "#ffffff");
            compare(String(view.colRowSelected), "#d0e4f7");
            compare(String(view.colTextSecondary), "#5e656c");
            compare(String(view.colBorder), "#e9ecef");
            controller.chooseColorScheme("dark");
            compare(String(view.colAccent), "#4dabf7");
            compare(String(view.colOnAccent), "#101113");
            compare(String(view.colRowSelected), "#1c3b5a");
            compare(String(view.colTextSecondary), "#a0a2a5");
        }

        // The open window follows the desktop's accent and contrast as they
        // change, and Preferences can keep Matters' own accent (#76).
        function test_the_window_follows_the_desktops_accent() {
            controller.chooseColorScheme("light");
            const ring = findChild(findChild(view, "nav-inbox"), "focusRing");
            compare(String(ring.color), "#1864ab");

            desktop.set("#e01b24", false);
            tryCompare(controller, "systemAccent", "#e01b24");
            verify(view.colAccent.hslHue < 0.05 || view.colAccent.hslHue > 0.95, "red: " + view.colAccent);
            compare(String(ring.color), String(view.colAccent), "what is drawn follows");
            verify(String(view.colRowSelected) !== "#d0e4f7", "the selection is tinted with it");

            const dialog = findChild(view, "preferencesDialog");
            dialog.open();
            tryVerify(() => dialog.opened, 2000);
            try {
                const box = findChild(dialog.contentItem, "followAccentBox");
                verify(box.enabled && box.checked);
                compare(findChild(dialog.contentItem, "noAccent").visible, false);
                waitForRendering(dialog.contentItem);
                mouseClick(box);
                tryCompare(controller, "followAccent", false);
                compare(String(view.colAccent), "#1864ab", "Matters' own again");
                compare(String(ring.color), "#1864ab");
                mouseClick(box);
                tryCompare(controller, "followAccent", true);
                verify(String(view.colAccent) !== "#1864ab");

                // A desktop without an accent: nothing to follow.
                desktop.set("", false);
                tryCompare(controller, "systemAccent", "");
                compare(String(view.colAccent), "#1864ab");
                verify(!box.enabled);
                verify(findChild(dialog.contentItem, "noAccent").visible);
            } finally {
                dialog.close();
                tryVerify(() => !dialog.visible, 2000);
            }

            // Higher contrast: secondary text darkens, lines show.
            desktop.set("", true);
            tryCompare(controller, "highContrast", true);
            verify(luminance(view.colTextSecondary) < luminance(Qt.color("#5e656c")));
            verify(ratio(view.colBorder, view.colBg) >= 3);
        }

        // Every combination in one test (ci/ui-lane.sh counts test
        // functions, so no data rows): light and dark, with and without
        // higher contrast, Matters' accent and each desktop's.
        function test_text_is_readable() {
            const low = [];
            for (const scheme of ["light", "dark"]) {
                for (const high of [false, true]) {
                    for (const accent of accents) {
                        const where = scheme + (high ? ", high contrast" : "") + ", accent " + (accent || "none");
                        controller.chooseColorScheme(scheme);
                        desktop.set(accent, high);
                        tryCompare(controller, "systemAccent", accent);
                        tryCompare(controller, "highContrast", high);
                        compare(view.isDark, scheme === "dark");
                        for (const [text, on, need] of pairs(high)) {
                            for (const s of on) {
                                const r = ratio(view[text], view[s]);
                                if (r < need)
                                    low.push(where + ": " + text + " on " + s + ": " + r.toFixed(2) + ":1, needs "
                                        + need + " (" + view[text] + " on " + view[s] + ")");
                            }
                        }
                    }
                }
            }
            // QtTest shortens long failure messages, so each pair is also
            // reported on its own.
            for (const line of low)
                warn(line);
            verify(low.length === 0, low.length + " pairs too faint, listed above; the first: " + low[0]);
        }
    }
}
