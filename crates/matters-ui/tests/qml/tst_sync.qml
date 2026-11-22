import QtQuick
import QtTest
import matters

// Sync (#42) as a person uses it, with the keyboard: the ways in for a device
// that does not sync, a damaged invitation, starting a collection with no
// relay to reach, the recovery kit shown once, and stopping with changes not
// yet on the relay. The sync session keeps its keys in memory (tests/ui.rs)
// and is shared with the other tests, so this ends where it began: not
// syncing.
Item {
    width: 960
    height: 680

    MainView {
        id: view
        anchors.fill: parent
    }

    TestCase {
        id: testCase
        name: "Sync"
        when: windowShown

        readonly property var controller: view.appController

        function dialog(name) {
            return findChild(view, name);
        }

        function open() {
            const d = dialog("syncDialog");
            mouseClick(findChild(view, "syncButton"));
            tryVerify(() => d.opened, 2000, "the Sync dialog opens");
            return d;
        }

        function close(d) {
            d.close();
            tryVerify(() => !d.visible, 2000, "it closes");
        }

        function type(text) {
            for (const c of text)
                keyClick(c);
        }

        function test_from_the_ways_in_to_stopping() {
            compare(controller.syncAvailable, true);
            compare(controller.syncPhase, "off");
            // The sidebar says where things stand, to a screen reader too.
            const button = findChild(view, "syncButton");
            verify(button.Accessible.name.indexOf("on this device only") >= 0, button.Accessible.name);

            // Not syncing: the keyboard starts in the relay's address.
            let d = open();
            const endpoint = findChild(d, "syncEndpoint");
            tryVerify(() => endpoint.activeFocus, 2000, "focus starts in the address");
            compare(endpoint.Accessible.name, "Relay address");
            compare(findChild(d, "syncCreate").enabled, false, "nothing to start without an address");
            for (const name of ["syncInvitationField", "syncCollectionField", "syncWordsField"])
                verify(findChild(d, name).Accessible.name !== "", name + " is named");

            // A damaged invitation is refused, in plain words, and nothing changes.
            type("http://127.0.0.1:9");
            compare(findChild(d, "syncCreate").enabled, true);
            const invitation = findChild(d, "syncInvitationField");
            invitation.forceActiveFocus();
            type("not an invitation");
            mouseClick(findChild(d, "syncJoin"));
            tryCompare(controller, "syncError", "this is not a Matters invitation");
            compare(controller.syncPhase, "off");
            verify(findChild(d, "syncError").visible);

            // Starting a collection: the recovery kit, once, then a relay
            // that can't be reached. Changes are saved here, never "backed up".
            mouseClick(findChild(d, "syncCreate"));
            const kit = dialog("recoveryKitDialog");
            tryVerify(() => kit.opened, 3000, "the recovery kit is shown");
            compare(findChild(kit, "recoveryKitWords").text.trim().split(/\s+/).length, 24);
            compare(findChild(kit, "recoveryKitEndpoint").text, "http://127.0.0.1:9");
            verify(findChild(kit, "recoveryKitCollection").text.length > 0);
            const kept = findChild(kit, "recoveryKitKept");
            tryVerify(() => kept.activeFocus, 2000, "the keyboard is on the kit's button");
            keyClick(Qt.Key_Space);
            tryVerify(() => !kit.visible, 2000, "the kit is put away");
            compare(controller.syncKitJson, "");
            tryCompare(controller, "syncPhase", "on");
            tryCompare(controller, "syncHealth", "retrying", 10000);
            const status = findChild(d, "syncStatus");
            verify(status.text.indexOf("saved on this device") >= 0, status.text);
            verify(status.text.indexOf("backed up") < 0);
            compare(controller.syncAdmin, true);
            compare(JSON.parse(controller.syncDevicesJson).length, 1);
            verify(controller.syncUnsent > 0, "the existing tasks wait for the relay");
            verify(findChild(d, "syncUnsent").visible);

            // Stopping with changes not on the relay: said plainly, and only
            // the explicit choice does it. Every task stays.
            const tasksBefore = controller.inboxCount + controller.todayCount + controller.anytimeCount;
            mouseClick(findChild(d, "syncLeave"));
            const leave = dialog("syncLeaveDialog");
            tryVerify(() => leave.opened, 2000, "the stop dialog opens");
            const text = findChild(leave, "syncLeaveText").text;
            verify(text.indexOf("not yet on the relay") >= 0, text);
            verify(text.indexOf("never get them") >= 0, text);
            tryVerify(() => findChild(leave, "syncLeaveCancel").activeFocus, 2000,
                      "the safe choice has the keyboard");
            keyClick(Qt.Key_Space);
            tryVerify(() => !leave.visible, 2000);
            compare(controller.syncPhase, "on", "keeping sync changed nothing");
            mouseClick(findChild(d, "syncLeave"));
            tryVerify(() => leave.opened, 2000);
            mouseClick(findChild(leave, "syncLeaveConfirm"));
            tryCompare(controller, "syncPhase", "off");
            compare(controller.inboxCount + controller.todayCount + controller.anytimeCount, tasksBefore);
            close(d);
        }
    }
}
