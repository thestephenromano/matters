import QtQuick

// A ring around its parent while the parent has the keyboard focus (#74),
// for the items Matters draws itself; Qt's controls draw their own.
// Clicking these items doesn't focus them, so the ring shows only for the
// keyboard.
Rectangle {
    objectName: "focusRing"
    anchors.fill: parent
    anchors.margins: -2
    // Two pixels outside the sidebar entries' 6.
    radius: 8
    color: "transparent"
    border.width: 2
    visible: parent !== null && parent.activeFocus
}
