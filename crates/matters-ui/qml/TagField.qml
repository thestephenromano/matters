pragma ComponentBehavior: Bound
import QtQuick
import QtQuick.Controls
import QtQuick.Layouts
import matters

// The tags of one task, project or area (#57): a chip per tag, each with a
// button to take it off, and a field that adds a tag by name, suggesting
// existing tags as you type.
//
// Keys in the field: Enter adds the highlighted suggestion or, with none,
// the typed name (creating the tag if it is new); Up and Down move through
// the suggestions (Up past the first means "none"); Backspace in the empty
// field takes off the last tag. Escape clears what was typed; in the empty
// field it is left to the host (in an open task, the list closes the task,
// SPEC §4) and `leave` is signalled.
ColumnLayout {
    id: field

    // `task`, `project` or `area`, and its id.
    required property string kind
    required property string itemId
    // The item's tags, `[{id, name}]`, and every tag (`tagsJson`).
    required property var tags
    required property var allTags
    required property AppController controller
    required property color textPrimary
    required property color textSecondary
    required property color chipColor
    required property color highlightColor

    // Escape in the empty field: the host may take the focus back.
    signal leave()

    readonly property alias input: input
    // The tags that match what is typed and that the item hasn't got yet.
    readonly property var suggestions: {
        const typed = input.text.trim().toLowerCase();
        if (typed === "")
            return [];
        const mine = tags.map(t => t.id);
        return allTags.filter(t => mine.indexOf(t.id) < 0 && t.name.toLowerCase().indexOf(typed) >= 0).slice(0, 6);
    }
    // The suggestion Enter would add; -1 for the typed name itself. The
    // first suggestion is offered when it starts with what was typed.
    property int highlighted: -1

    function focusInput() {
        input.forceActiveFocus();
    }

    // Hidden (its task closed), the field gives up the focus: otherwise its
    // focus scope would hand the keys back to it, unseen.
    onVisibleChanged: if (!visible) input.focus = false

    function commit() {
        const chosen = highlighted >= 0 ? suggestions[highlighted] : null;
        if (chosen)
            controller.addTag(kind, itemId, chosen.id);
        else if (input.text.trim() !== "")
            controller.tagByName(kind, itemId, input.text);
        input.text = "";
    }

    spacing: 4

    Flow {
        Layout.fillWidth: true
        spacing: 6

        Repeater {
            model: field.tags
            delegate: Rectangle {
                id: chip
                objectName: "tagChip"
                required property var modelData
                readonly property string name: modelData.name
                height: 22
                width: chipRow.implicitWidth + 16
                radius: 11
                color: field.chipColor
                Accessible.role: Accessible.StaticText
                Accessible.name: "Tag: " + name

                RowLayout {
                    id: chipRow
                    anchors.centerIn: parent
                    spacing: 4
                    Text {
                        text: chip.name
                        font.pixelSize: 12
                        color: field.textPrimary
                    }
                    Item {
                        objectName: "removeTag"
                        Layout.preferredWidth: 14
                        Layout.preferredHeight: 14
                        Accessible.role: Accessible.Button
                        Accessible.name: "Remove the tag " + chip.name
                        Icon {
                            anchors.centerIn: parent
                            name: "close"
                            size: 10
                            color: field.textSecondary
                        }
                        MouseArea {
                            anchors.fill: parent
                            cursorShape: Qt.PointingHandCursor
                            onClicked: field.controller.removeTag(field.kind, field.itemId, chip.modelData.id)
                        }
                    }
                }
            }
        }

        TextField {
            id: input
            objectName: "tagInput"
            width: 160
            height: 24
            placeholderText: field.tags.length === 0 ? "Add tags" : "Add a tag"
            font.pixelSize: 12
            leftPadding: 2
            topPadding: 2
            bottomPadding: 2
            background: null
            color: field.textPrimary
            onTextChanged: {
                const first = field.suggestions[0];
                field.highlighted = first && first.name.toLowerCase().indexOf(text.trim().toLowerCase()) === 0 ? 0 : -1;
            }
            Keys.onReturnPressed: field.commit()
            Keys.onEnterPressed: field.commit()
            Keys.onDownPressed: field.highlighted = Math.min(field.suggestions.length - 1, field.highlighted + 1)
            Keys.onUpPressed: field.highlighted = Math.max(-1, field.highlighted - 1)
            Keys.onEscapePressed: event => {
                if (text !== "") {
                    text = "";
                } else {
                    event.accepted = false;
                    field.leave();
                }
            }
            Keys.onPressed: event => {
                if (event.key === Qt.Key_Backspace && text === "" && field.tags.length > 0) {
                    const last = field.tags[field.tags.length - 1];
                    field.controller.removeTag(field.kind, field.itemId, last.id);
                    event.accepted = true;
                }
            }
        }
    }

    // Suggestions, under the field while something is typed.
    Repeater {
        model: field.suggestions
        delegate: Rectangle {
            id: suggestion
            objectName: "tagSuggestion"
            required property var modelData
            required property int index
            readonly property string name: modelData.name
            Layout.preferredWidth: 220
            Layout.preferredHeight: 24
            radius: 4
            color: index === field.highlighted ? field.highlightColor : "transparent"
            Text {
                anchors.verticalCenter: parent.verticalCenter
                x: 8 + 12 * suggestion.modelData.depth
                text: suggestion.name
                font.pixelSize: 12
                color: field.textPrimary
            }
            MouseArea {
                anchors.fill: parent
                cursorShape: Qt.PointingHandCursor
                onClicked: {
                    field.controller.addTag(field.kind, field.itemId, suggestion.modelData.id);
                    input.text = "";
                }
            }
        }
    }
}
