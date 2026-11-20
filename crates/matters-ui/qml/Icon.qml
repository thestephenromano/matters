pragma ComponentBehavior: Bound
import QtQuick
import QtQuick.Shapes

// One of Matters' own symbolic icons. Each is a path on a 24×24 grid, drawn
// as a line in `color`, so it stays crisp at any size and follows the palette
// (light, dark, a view's color). Dot icons (more, grip) are filled instead.
Item {
    id: icon

    property string name: ""
    property color color: "black"
    property real size: 16
    // Line width on the 24×24 grid; scaled with the icon.
    property real strokeWidth: 1.8

    implicitWidth: size
    implicitHeight: size

    readonly property var paths: ({
        "inbox": "M3 13h5l1.5 2.5h5L16 13h5 M5.5 5h13L21 13v6H3v-6z",
        "today": "M12 7.5a4.5 4.5 0 1 0 0 9a4.5 4.5 0 1 0 0-9z M12 2v2 M12 20v2 M4.9 4.9l1.4 1.4 M17.7 17.7l1.4 1.4 M2 12h2 M20 12h2 M4.9 19.1l1.4-1.4 M17.7 6.3l1.4-1.4",
        "upcoming": "M4 6h16v14H4z M4 10h16 M8 3v4 M16 3v4",
        "calendar": "M4 6h16v14H4z M4 10h16 M8 3v4 M16 3v4",
        "anytime": "M12 3l9 5-9 5-9-5z M3 13l9 5 9-5",
        "someday": "M3 4h18v4H3z M5 8v12h14V8 M10 12h4",
        "logbook": "M6 3h10a3 3 0 0 1 3 3v15H8a3 3 0 0 1-3-3V4 M9 11.5l2 2 4-4",
        "trash": "M4 7h16 M9 7V4h6v3 M6 7l1 13h10l1-13 M10 11v6 M14 11v6",
        "area": "M3 6.5h6l2 2h10v11H3z",
        "project": "M12 4a8 8 0 1 0 0 16a8 8 0 1 0 0-16z",
        "search": "M11 4a7 7 0 1 0 0 14a7 7 0 1 0 0-14z M16.5 16.5L21 21",
        "evening": "M20 14.5A8.5 8.5 0 1 1 9.5 4a7 7 0 0 0 10.5 10.5z",
        "deadline": "M6 21V4 M6 4h11l-2.5 4 2.5 4H6",
        "plus": "M12 5v14 M5 12h14",
        "settings": "M4 6h9 M17 6h3 M15 4v4 M4 12h3 M11 12h9 M9 10v4 M4 18h11 M19 18h1 M17 16v4",
        "sidebar": "M4 5h16v14H4z M9.5 5v14",
        "close": "M6 6l12 12 M18 6L6 18",
        "check": "M5 12.5l4.5 4.5L19 7",
        "tag": "M3.5 3.5h8l9 9-8 8-9-9z M8 7a1 1 0 1 0 0.01 0z",
        "checklist": "M4 5h3.5v3.5H4z M4 10.25h3.5v3.5H4z M4 15.5h3.5V19H4z M11 6.75h9 M11 12h9 M11 17.25h9",
        "chevron-left": "M15 5l-7 7 7 7",
        "chevron-right": "M9 5l7 7-7 7",
        "chevron-down": "M7 10l5 5 5-5",
        "more": "M3.6 12a1.4 1.4 0 1 0 2.8 0a1.4 1.4 0 1 0-2.8 0z M10.6 12a1.4 1.4 0 1 0 2.8 0a1.4 1.4 0 1 0-2.8 0z M17.6 12a1.4 1.4 0 1 0 2.8 0a1.4 1.4 0 1 0-2.8 0z",
        "grip": "M7.8 6a1.2 1.2 0 1 0 2.4 0a1.2 1.2 0 1 0-2.4 0z M13.8 6a1.2 1.2 0 1 0 2.4 0a1.2 1.2 0 1 0-2.4 0z M7.8 12a1.2 1.2 0 1 0 2.4 0a1.2 1.2 0 1 0-2.4 0z M13.8 12a1.2 1.2 0 1 0 2.4 0a1.2 1.2 0 1 0-2.4 0z M7.8 18a1.2 1.2 0 1 0 2.4 0a1.2 1.2 0 1 0-2.4 0z M13.8 18a1.2 1.2 0 1 0 2.4 0a1.2 1.2 0 1 0-2.4 0z"
    })

    // Dot icons are filled shapes rather than lines.
    readonly property bool filled: name === "more" || name === "grip"

    Shape {
        anchors.centerIn: parent
        width: 24
        height: 24
        scale: icon.size / 24
        preferredRendererType: Shape.CurveRenderer

        ShapePath {
            strokeColor: icon.filled ? "transparent" : icon.color
            strokeWidth: icon.strokeWidth
            fillColor: icon.filled ? icon.color : "transparent"
            capStyle: ShapePath.RoundCap
            joinStyle: ShapePath.RoundJoin

            PathSvg {
                path: icon.paths[icon.name] || ""
            }
        }
    }
}
