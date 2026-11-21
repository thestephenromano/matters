import QtQuick
import QtQuick.Controls
import matters

ApplicationWindow {
    width: 960
    height: 680
    minimumWidth: 720
    minimumHeight: 500
    visible: true
    title: qsTr("%1 — Matters", "the window's title: the open list, then the app").arg(mainView.title)

    MainView {
        id: mainView
        anchors.fill: parent
    }
}
