import QtQuick
import QtQuick.Controls
import matters

ApplicationWindow {
    width: 960
    height: 680
    minimumWidth: 720
    minimumHeight: 500
    visible: true
    title: mainView.title + " — Matters"

    MainView {
        id: mainView
        anchors.fill: parent
    }
}
