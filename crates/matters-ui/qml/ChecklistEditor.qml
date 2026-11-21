pragma ComponentBehavior: Bound
import QtQuick
import QtQuick.Controls
import matters

// The checklist of one open task (#21): tick, rename, reorder and delete its
// items, and add new ones. Items are saved as they change; a blank one is
// never stored.
//
// Keys in an item: Enter opens a new item below it, Ctrl+. ticks or unticks
// it, Backspace in an empty item deletes it, Up and Down go to the item above
// or below, Ctrl+Up and Ctrl+Down move it, and Escape leaves the checklist
// (`leave`). Every key typed here stays here (Tab aside, which moves focus):
// none reaches the task list underneath.
Item {
    id: editor

    required property string taskId
    required property AppController controller

    // MainView's colours.
    property color textPrimary: "black"
    property color textSecondary: "gray"
    property color accent: "blue"
    // A tick on the accent.
    property color tickColor: "white"
    property color border: "lightgray"
    property color hover: "whitesmoke"

    // The field for a new item is open, under `newAfterId` ("" for the end).
    property bool adding: false
    property string newAfterId: ""

    // A row to put the cursor in: the item `focusId`'s title, or the new-item
    // field under `newFocusId`. Rows that exist, or are created, take it.
    property string focusId: ""
    property string newFocusId: ""

    // An item being dragged by its grip, and the row it would drop before.
    property string dragId: ""
    property int dropIndex: -1

    readonly property int count: repeater.count

    // Escape: the user is done with the checklist.
    signal leave()
    // An item was deleted; the host offers Undo (#169).
    signal itemDeleted(string itemId, string title)

    // Whether there is anything to show. Hosts bind to this, not to
    // `visible`, which also reads false while the host itself is hidden.
    readonly property bool shown: count > 0 || adding

    implicitHeight: column.implicitHeight
    visible: shown

    // Keys the fields leave alone (F2, say) would otherwise bubble up to the
    // task list and act on the task.
    Keys.onPressed: event => {
        if (event.key !== Qt.Key_Tab && event.key !== Qt.Key_Backtab)
            event.accepted = true;
    }
    // A focus request is only for the field open now.
    onAddingChanged: if (!adding) newFocusId = ""

    ChecklistModel {
        id: items
    }

    function reload() {
        items.load(editor.taskId);
    }
    Component.onCompleted: reload()
    Connections {
        target: editor.controller
        function onRevisionChanged() {
            Qt.callLater(editor.reload);
        }
    }

    // Open the new-item field under `afterId` ("" for the end) and focus it.
    function startAdding(afterId) {
        newAfterId = afterId;
        adding = true;
        Qt.callLater(focusNewField);
    }

    function focusNewField() {
        if (!adding)
            return;
        if (newAfterId === "") {
            endField.focusInput();
        } else {
            // Ask afresh, even if an unanswered request names the same row.
            newFocusId = "";
            newFocusId = newAfterId;
        }
    }

    function hasItem(id) {
        for (let i = 0; i < items.rowCount(); ++i) {
            if (items.idAt(i) === id)
                return true;
        }
        return false;
    }

    function focusItem(index) {
        if (index >= 0 && index < repeater.count)
            focusId = items.idAt(index);
    }

    // Add what was typed in a new-item field. The field then sits under the
    // new item, ready for the next one.
    function commitNew(field) {
        const title = field.text;
        field.text = "";
        if (title.trim() === "")
            return;
        const id = controller.addChecklistItem(taskId, title, newAfterId);
        if (id !== "" && newAfterId !== "") {
            newAfterId = id;
            Qt.callLater(focusNewField);
        }
    }

    // A new-item field lost focus: keep what was typed. It closes the editor's
    // new item only if it is still the open field; otherwise it has handed
    // over (to the field under an item just added, or elsewhere) and keeps
    // what was typed all the same. An item deleted meanwhile (elsewhere) is
    // no place to add below: the text goes at the end instead.
    function newFieldLeft(field, afterId) {
        const title = field.text;
        field.text = "";
        if (adding && newAfterId === afterId)
            adding = false;
        if (title.trim() === "")
            return;
        controller.addChecklistItem(taskId, title, afterId === "" || hasItem(afterId) ? afterId : "");
    }

    function cancelNew(field) {
        field.text = "";
        adding = false;
        editor.leave();
    }

    function dropIndexAt(scenePoint) {
        const y = column.mapFromItem(null, scenePoint.x, scenePoint.y).y;
        for (let i = 0; i < repeater.count; ++i) {
            const row = repeater.itemAt(i);
            if (row && y < row.y + row.height / 2)
                return i;
        }
        return repeater.count;
    }

    function finishDrag() {
        const id = dragId;
        const at = dropIndex;
        dragId = "";
        dropIndex = -1;
        if (id === "" || at < 0)
            return;
        let from = -1;
        for (let i = 0; i < repeater.count; ++i) {
            if (items.idAt(i) === id)
                from = i;
        }
        if (from < 0 || at === from || at === from + 1)
            return; // dropped where it already is
        let after = "";
        for (let i = at - 1; i >= 0; --i) {
            if (items.idAt(i) !== id) {
                after = items.idAt(i);
                break;
            }
        }
        controller.placeChecklistItem(id, after);
    }

    // A field for a new item. It says what happens; the editor decides.
    component NewItemField: Rectangle {
        id: newField
        objectName: "checklistNewItem"
        property string afterId: ""
        property alias input: newInput
        property color textColor: "black"
        property color placeholderColor: "gray"
        property color focusColor: "blue"
        signal commit()
        signal cancel()
        signal focusLost()
        height: visible ? 30 : 0
        radius: 4
        color: "transparent"
        border.color: newInput.activeFocus ? focusColor : "transparent"

        TextInput {
            id: newInput
            objectName: "checklistNewInput"
            anchors.fill: parent
            anchors.leftMargin: 22 // in line with the item titles
            anchors.rightMargin: 8
            verticalAlignment: TextInput.AlignVCenter
            font.pixelSize: 13
            color: newField.textColor
            clip: true

            Text {
                anchors.fill: parent
                verticalAlignment: Text.AlignVCenter
                text: qsTr("New checklist item")
                color: newField.placeholderColor
                font.pixelSize: 13
                visible: !newInput.text
            }

            Keys.onReturnPressed: newField.commit()
            Keys.onEnterPressed: newField.commit()
            Keys.onEscapePressed: newField.cancel()
            Keys.onUpPressed: event => event.accepted = true
            Keys.onDownPressed: event => event.accepted = true
            Keys.onPressed: event => {
                // Backspace in an empty field closes it, like an empty item.
                if (event.key === Qt.Key_Backspace && newInput.text === "") {
                    event.accepted = true;
                    newField.cancel();
                }
            }
            onActiveFocusChanged: if (!activeFocus) newField.focusLost()
        }

        function focusInput() {
            newInput.forceActiveFocus();
        }
        // Closing the task while typing keeps what was typed.
        Component.onDestruction: if (visible) focusLost()
    }

    Column {
        id: column
        width: parent.width

        Repeater {
            id: repeater
            model: items

            delegate: Column {
                id: itemRow
                objectName: "checklistItem"
                required property string itemId
                required property string title
                required property bool done
                required property int index

                width: column.width

                // Take the cursor when the editor asks for this row, now or
                // once the row exists (an item just added).
                readonly property bool titleFocusRequested: editor.focusId === itemId
                readonly property bool newFocusRequested: editor.newFocusId === itemId
                onTitleFocusRequestedChanged: if (titleFocusRequested) Qt.callLater(itemRow.takeFocus)
                onNewFocusRequestedChanged: if (newFocusRequested) Qt.callLater(itemRow.takeFocus)
                Component.onCompleted: if (titleFocusRequested || newFocusRequested) Qt.callLater(itemRow.takeFocus)
                // Delete this item. What is being typed under it is kept, in
                // its place; an unsaved rename of it is dropped.
                function remove() {
                    if (newBelow.visible)
                        editor.newFieldLeft(newBelow.input, itemId);
                    titleInput.text = itemRow.title;
                    editor.controller.deleteChecklistItem(itemId);
                    editor.itemDeleted(itemId, itemRow.title);
                }
                function toggleDone() {
                    editor.controller.setChecklistItemDone(itemId, !done);
                }
                function takeFocus() {
                    if (titleFocusRequested) {
                        editor.focusId = "";
                        titleInput.forceActiveFocus();
                    }
                    if (newFocusRequested && newBelow.visible) {
                        editor.newFocusId = "";
                        newBelow.focusInput();
                    }
                }

                Item {
                    width: parent.width
                    height: 30

                    HoverHandler { id: itemHover }

                    Rectangle {
                        anchors.fill: parent
                        radius: 4
                        color: itemHover.hovered ? editor.hover : "transparent"
                    }

                    // Drag grip: drop the item between two others. It sits in
                    // the margin, so the checkboxes line up with the notes.
                    Item {
                        id: grip
                        objectName: "checklistGrip"
                        width: 12
                        height: parent.height
                        anchors.right: box.left
                        anchors.rightMargin: 6
                        opacity: itemHover.hovered || gripHover.hovered || gripDrag.active ? 1 : 0

                        Icon {
                            anchors.centerIn: parent
                            name: "grip"
                            size: 14
                            color: editor.textSecondary
                        }
                        HoverHandler {
                            id: gripHover
                            cursorShape: Qt.OpenHandCursor
                        }
                        DragHandler {
                            id: gripDrag
                            target: null
                            onActiveChanged: {
                                if (active) {
                                    editor.dragId = itemRow.itemId;
                                } else {
                                    editor.finishDrag();
                                }
                            }
                            onCentroidChanged: if (active) editor.dropIndex = editor.dropIndexAt(centroid.scenePosition)
                        }
                    }

                    // Checkbox: a rounded square, unlike a task's circle.
                    Rectangle {
                        id: box
                        objectName: "checklistBox"
                        anchors.left: parent.left
                        anchors.verticalCenter: parent.verticalCenter
                        width: 14
                        height: 14
                        radius: 3
                        color: itemRow.done ? editor.accent : "transparent"
                        border.width: 1.5
                        border.color: itemRow.done || boxMouse.containsMouse ? editor.accent : editor.textSecondary
                        Accessible.role: Accessible.CheckBox
                        Accessible.name: itemRow.title
                        Accessible.checkable: true
                        Accessible.checked: itemRow.done

                        Icon {
                            anchors.centerIn: parent
                            name: "check"
                            size: 11
                            strokeWidth: 3
                            color: editor.tickColor
                            visible: itemRow.done
                        }
                        MouseArea {
                            id: boxMouse
                            anchors.fill: parent
                            anchors.margins: -4
                            hoverEnabled: true
                            cursorShape: Qt.PointingHandCursor
                            onClicked: itemRow.toggleDone()
                        }
                    }

                    TextInput {
                        id: titleInput
                        objectName: "checklistTitle"
                        Accessible.name: qsTr("Checklist item")
                        anchors.left: box.right
                        anchors.leftMargin: 8
                        anchors.right: deleteButton.left
                        anchors.rightMargin: 4
                        anchors.verticalCenter: parent.verticalCenter
                        text: itemRow.title
                        font.pixelSize: 13
                        font.strikeout: itemRow.done
                        color: itemRow.done ? editor.textSecondary : editor.textPrimary
                        selectByMouse: true
                        clip: true

                        // Saved when editing ends. A blank title is refused and
                        // the old one shown; Backspace deletes an empty item.
                        // A row deleted elsewhere ends editing as it goes: there
                        // is nothing left to rename.
                        onEditingFinished: {
                            if (text === itemRow.title || !editor.hasItem(itemRow.itemId))
                                return;
                            if (!editor.controller.renameChecklistItem(itemRow.itemId, text))
                                text = itemRow.title;
                        }
                        // Show changes made elsewhere, but never while typing.
                        Connections {
                            target: itemRow
                            function onTitleChanged() {
                                if (!titleInput.activeFocus)
                                    titleInput.text = itemRow.title;
                            }
                        }

                        Keys.onReturnPressed: editor.startAdding(itemRow.itemId)
                        Keys.onEnterPressed: editor.startAdding(itemRow.itemId)
                        Keys.onEscapePressed: {
                            text = itemRow.title;
                            editor.leave();
                        }
                        Keys.onUpPressed: event => {
                            event.accepted = true;
                            if (event.modifiers & Qt.ControlModifier)
                                editor.controller.moveChecklistItem(itemRow.itemId, -1);
                            else
                                editor.focusItem(itemRow.index - 1);
                        }
                        Keys.onDownPressed: event => {
                            event.accepted = true;
                            if (event.modifiers & Qt.ControlModifier)
                                editor.controller.moveChecklistItem(itemRow.itemId, 1);
                            else
                                editor.focusItem(itemRow.index + 1);
                        }
                        Keys.onPressed: event => {
                            if (event.key === Qt.Key_Period && (event.modifiers & Qt.ControlModifier)) {
                                event.accepted = true;
                                itemRow.toggleDone();
                                return;
                            }
                            if (event.key !== Qt.Key_Backspace || titleInput.text !== "")
                                return;
                            event.accepted = true;
                            const before = itemRow.index - 1;
                            editor.controller.deleteChecklistItem(itemRow.itemId);
                            editor.itemDeleted(itemRow.itemId, itemRow.title);
                            Qt.callLater(editor.focusItem, Math.max(before, 0));
                        }
                    }

                    ToolButton {
                        id: deleteButton
                        objectName: "checklistDelete"
                        anchors.right: parent.right
                        anchors.verticalCenter: parent.verticalCenter
                        focusPolicy: Qt.NoFocus
                        visible: itemHover.hovered
                        width: visible ? implicitWidth : 0
                        Accessible.name: qsTr("Delete checklist item")
                        contentItem: Icon {
                            name: "close"
                            size: 12
                            color: editor.textSecondary
                        }
                        ToolTip.visible: hovered
                        ToolTip.text: qsTr("Delete")
                        onClicked: itemRow.remove()
                    }
                }

                NewItemField {
                    id: newBelow
                    afterId: itemRow.itemId
                    width: itemRow.width
                    visible: editor.adding && editor.newAfterId === itemRow.itemId
                    textColor: editor.textPrimary
                    placeholderColor: editor.textSecondary
                    focusColor: editor.accent
                    onCommit: editor.commitNew(newBelow.input)
                    onCancel: editor.cancelNew(newBelow.input)
                    onFocusLost: editor.newFieldLeft(newBelow.input, newBelow.afterId)
                }
            }
        }

        NewItemField {
            id: endField
            objectName: "checklistNewEnd"
            afterId: ""
            width: column.width
            visible: editor.adding && editor.newAfterId === ""
            textColor: editor.textPrimary
            placeholderColor: editor.textSecondary
            focusColor: editor.accent
            onCommit: editor.commitNew(endField.input)
            onCancel: editor.cancelNew(endField.input)
            onFocusLost: editor.newFieldLeft(endField.input, endField.afterId)
        }
    }

    // Where a dragged item would land.
    Rectangle {
        objectName: "checklistDropLine"
        visible: editor.dragId !== "" && editor.dropIndex >= 0
        x: 0
        width: parent.width
        height: 2
        radius: 1
        color: editor.accent
        y: {
            if (editor.dropIndex < 0)
                return 0;
            const row = repeater.itemAt(Math.min(editor.dropIndex, repeater.count - 1));
            if (!row)
                return 0;
            return editor.dropIndex < repeater.count ? row.y - 1 : row.y + row.height - 1;
        }
    }
}
