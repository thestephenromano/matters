import QtQuick
import QtTest
import matters

// The palette is readable (#74): in light and in dark, every text colour
// meets WCAG 2.2 AA (4.5:1) on every surface it is drawn on, and the
// accent, which also draws focus rings and ticked boxes, meets the 3:1 of
// non-text contrast against every surface. Every palette token is in a
// pair here, or named as a line nothing needs to read, so a new colour
// can't skip the check. qml_lint keeps every colour in the palette.
Item {
    width: 960
    height: 680

    MainView {
        id: view
        anchors.fill: parent
    }

    TestCase {
        name: "Contrast"
        when: windowShown

        readonly property var controller: view.appController

        // Every surface text is drawn on.
        readonly property var surfaces: ["colBg", "colSidebarBg", "colSidebarHover", "colSidebarActive",
            "colCardBg", "colRowHover", "colRowSelected", "colChip", "colBadge", "colField",
            "colControl", "colInputBar", "colDangerBg", "colEveningBg"]
        // [text, surfaces it is drawn on, the ratio it needs]
        readonly property var pairs: [
            ["colTextPrimary", surfaces, 4.5],
            ["colTextSecondary", surfaces, 4.5],
            ["colDanger", surfaces, 4.5],
            // The accent as text: selected sidebar entries, headings, links
            // and previews in popovers.
            ["colAccent", ["colBg", "colSidebarBg", "colSidebarHover", "colSidebarActive", "colCardBg",
                "colField", "colControl", "colInputBar", "colChip"], 4.5],
            // The accent as a focus ring, a ticked box or a selection mark.
            ["colAccent", surfaces, 3],
            ["colOnAccent", ["colAccent"], 4.5],
            // This Evening's heading, and its pill in a row.
            ["colEvening", ["colBg", "colCardBg", "colEveningBg", "colRowHover", "colRowSelected"], 4.5],
            ["colTooltipText", ["colTooltipBg"], 4.5],
            ["colInverseText", ["colInverseBg"], 4.5]
        ]
        // Lines that only separate or frame: nothing is read from them.
        readonly property var decorative: ["colBorder", "colRowSelectedBorder"]

        function luminance(c) {
            const linear = v => v <= 0.04045 ? v / 12.92 : Math.pow((v + 0.055) / 1.055, 2.4);
            return 0.2126 * linear(c.r) + 0.7152 * linear(c.g) + 0.0722 * linear(c.b);
        }

        function ratio(a, b) {
            const la = luminance(a), lb = luminance(b);
            return (Math.max(la, lb) + 0.05) / (Math.min(la, lb) + 0.05);
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
            for (const [text, on] of pairs) {
                checked.add(text);
                for (const s of on)
                    checked.add(s);
            }
            const unchecked = tokens.filter(t => !checked.has(t));
            compare(unchecked.length, 0, "tokens in no pair: " + unchecked.join(", "));
            for (const t of checked)
                verify(tokens.indexOf(t) >= 0, t + " is a token");
        }

        function test_text_is_readable_in_light_and_dark_data() {
            return [{ tag: "light", scheme: "light" }, { tag: "dark", scheme: "dark" }];
        }

        function test_text_is_readable_in_light_and_dark(data) {
            controller.chooseColorScheme(data.scheme);
            try {
                compare(view.isDark, data.scheme === "dark");
                const low = [];
                for (const [text, on, need] of pairs) {
                    for (const s of on) {
                        const r = ratio(view[text], view[s]);
                        if (r < need)
                            low.push(text + " on " + s + ": " + r.toFixed(2) + ":1, needs " + need
                                + " (" + view[text] + " on " + view[s] + ")");
                    }
                }
                compare(low.length, 0, "\n" + low.join("\n"));
            } finally {
                controller.chooseColorScheme("system");
            }
        }
    }
}
