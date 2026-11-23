import QtQuick
import QtTest
import matters

// Sync (#42) as a person uses it, with the keyboard: the ways in for a device
// that does not sync, a damaged invitation, starting a collection with no
// relay to reach, the recovery kit shown once, and stopping with changes not
// yet on the relay. Then adding a device with no relay to reach, and
// reloading a database opened elsewhere (#45). The sync session keeps its
// keys in memory (tests/ui.rs) and is shared with the other tests, so each
// ends where it began: not syncing.
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

        // A failed test leaves no sync, and no dialog, to the next one.
        function cleanup() {
            // The recovery kit's dialog follows the session: leaving puts it away.
            for (const name of ["syncLeaveDialog", "syncDialog"]) {
                const p = dialog(name);
                if (p.visible)
                    p.close();
            }
            if (controller.syncPhase !== "off")
                controller.syncLeave(true);
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
            // From the safe choice to the explicit one, with the keyboard: a
            // click here could land before the reopened dialog is placed, and
            // close it without choosing (#45).
            tryVerify(() => findChild(leave, "syncLeaveCancel").activeFocus, 2000);
            keyClick(Qt.Key_Tab);
            tryVerify(() => findChild(leave, "syncLeaveConfirm").activeFocus, 2000,
                      "Tab reaches the explicit choice");
            keyClick(Qt.Key_Space);
            tryCompare(controller, "syncPhase", "off", 5000, "not stopped: error '" + controller.syncError
                       + "', " + controller.syncUnsent + " unsent, dialog open: " + leave.visible);
            compare(controller.inboxCount + controller.todayCount + controller.anytimeCount, tasksBefore);
            close(d);
        }

        // Adding a device: the relay registers the invitation first, off the
        // window's thread (#45). A relay that can't be reached is said in
        // plain words, and no invitation is shown.
        function test_adding_a_device_waits_for_the_relay() {
            compare(controller.syncPhase, "off");
            controller.syncCreate("http://127.0.0.1:9");
            tryCompare(controller, "syncPhase", "on");
            controller.syncKitKept();
            tryVerify(() => !dialog("recoveryKitDialog").visible, 2000, "the kit is put away");
            const d = open();
            const invite = findChild(d, "syncInvite");
            // The dialog shows its syncing controls for the first time here:
            // click only once they are laid out, not where they start (0, 0).
            tryVerify(() => invite.visible && invite.y > 0, 2000, "the admin can add a device");
            mouseClick(invite);
            const settled = () => !controller.syncInviting
                  && (controller.syncError !== "" || controller.syncInvitation !== "");
            for (let i = 0; i < 100 && !settled(); ++i)
                wait(100);
            verify(controller.syncError !== "", "the failure is said; inviting: " + controller.syncInviting
                   + ", invitation: '" + controller.syncInvitation + "', leave dialog open: "
                   + dialog("syncLeaveDialog").visible);
            compare(controller.syncInvitation, "");
            verify(!findChild(d, "syncInviting").visible);
            verify(invite.visible, "it can be tried again");
            compare(controller.syncLeave(true), 0);
            compare(controller.syncPhase, "off");
            close(d);
        }

        // Another open of the database (matters-syncd, or another Matters)
        // took over as this device's writer, so changes here are refused
        // (#45): the window says so and offers to reload, which loses
        // nothing.
        function test_a_database_opened_elsewhere_offers_to_reload() {
            const banner = findChild(view, "supersededBanner");
            verify(!banner.visible);
            // As a refused change sets it (bridge.rs, `note_superseded`).
            controller.databaseSuperseded = true;
            tryVerify(() => banner.visible, 2000, "the banner shows");
            const text = findChild(view, "supersededText").text;
            verify(text.indexOf("opened again elsewhere") >= 0, text);
            const reload = findChild(view, "reloadDatabase");
            compare(reload.Accessible.name, "Reload the database");
            tryVerify(() => reload.x > 0, 2000, "laid out beside the text");
            const tasks = controller.inboxCount + controller.todayCount + controller.anytimeCount;
            mouseClick(reload);
            compare(controller.databaseSuperseded, false);
            tryVerify(() => !banner.visible, 2000, "the banner goes");
            compare(controller.errorMessage, "");
            compare(controller.inboxCount + controller.todayCount + controller.anytimeCount, tasks,
                    "every task is still here");
        }
    }
}
