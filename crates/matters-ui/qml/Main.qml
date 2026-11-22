import QtQuick
import QtQuick.Controls
import matters

ApplicationWindow {
    id: window
    width: 960
    height: 680
    minimumWidth: 720
    minimumHeight: 500
    // Started by a D-Bus call (#203): no window until asked for one.
    visible: !mainView.controller.startedInBackground()
    title: qsTr("%1 — Matters", "the window's title: the open list, then the app").arg(mainView.title)

    MainView {
        id: mainView
        anchors.fill: parent
    }

    Connections {
        target: mainView.controller
        function onActivateRequested() {
            window.show()
            window.raise()
            window.requestActivate()
        }
    }

    // An instance with no window quits once idle (#203); closing a shown
    // window quits as before.
    Timer {
        interval: 30000
        repeat: true
        running: !window.visible
        onTriggered: if (mainView.controller.idleExpired()) Qt.quit()
    }
}
