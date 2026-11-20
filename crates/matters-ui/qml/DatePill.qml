pragma ComponentBehavior: Bound
import QtQuick
import QtQuick.Controls
import QtQuick.Layouts
import matters

// A When or Deadline pill: shows the current value, or what can be set, and
// opens its popover when clicked. Used in an open task and in a project's
// header (#56).
ToolButton {
    id: pill

    // What the pill says, and whether it names a value (shown stronger).
    required property string label
    required property bool hasValue
    // "calendar" for When, "deadline" for a deadline.
    required property string iconName
    // Colours from the view's palette.
    required property color textPrimary
    required property color textSecondary
    required property color lineColor
    required property color hoverColor
    required property color downColor

    focusPolicy: Qt.NoFocus
    font.pixelSize: 12
    Accessible.name: label
    Accessible.role: Accessible.ButtonMenu
    leftPadding: 10
    rightPadding: 8
    topPadding: 4
    bottomPadding: 4

    background: Rectangle {
        implicitHeight: 26
        radius: height / 2
        color: pill.down ? pill.downColor : pill.hovered ? pill.hoverColor : "transparent"
        border.width: 1
        border.color: pill.lineColor
    }

    contentItem: RowLayout {
        spacing: 6
        Icon {
            name: pill.iconName
            size: 14
            color: pill.textSecondary
        }
        Text {
            objectName: "pillLabel"
            text: pill.label
            font: pill.font
            color: pill.hasValue ? pill.textPrimary : pill.textSecondary
        }
        Icon {
            objectName: "pillChevron"
            name: "chevron-down"
            size: 12
            color: pill.textSecondary
        }
    }
}
