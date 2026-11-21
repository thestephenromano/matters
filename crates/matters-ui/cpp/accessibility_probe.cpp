// The accessibility tree as a screen reader sees it (#74), for the QML
// tests (src/accessibility_probe.rs). Built only with the `ui-tests`
// feature.
#include <QtCore/QByteArray>
#include <QtCore/QJsonArray>
#include <QtCore/QJsonDocument>
#include <QtCore/QJsonObject>
#include <QtGui/QAccessible>
#include <QtGui/QGuiApplication>
#include <QtGui/QWindow>
#include <QtQuick/QQuickItem>
#include <QtQuick/QQuickWindow>

#include <cstdlib>
#include <cstring>

namespace {

// The nearest object name at or around `object`, through visual parents
// (a delegate's items are parented to the view, not to the delegate).
QString within(QObject *object)
{
    for (QObject *o = object; o != nullptr;) {
        if (!o->objectName().isEmpty())
            return o->objectName();
        auto *item = qobject_cast<QQuickItem *>(o);
        o = item != nullptr && item->parentItem() != nullptr ? item->parentItem() : o->parent();
    }
    return QString();
}

// Whether a screen reader can act on `role`.
bool interactive(QAccessible::Role role)
{
    switch (role) {
    case QAccessible::Button:
    case QAccessible::CheckBox:
    case QAccessible::RadioButton:
    case QAccessible::ComboBox:
    case QAccessible::EditableText:
    case QAccessible::ListItem:
    case QAccessible::MenuItem:
    case QAccessible::PageTab:
    case QAccessible::Link:
    case QAccessible::ButtonMenu:
    case QAccessible::ButtonDropDown:
    case QAccessible::Switch:
    case QAccessible::SpinBox:
    case QAccessible::Slider:
        return true;
    default:
        return false;
    }
}

// `inside` is the role of the nearest element above that a screen reader
// can act on (a combo box's own text field is part of the combo box).
void walk(QAccessibleInterface *iface, QJsonArray &out, int depth, int inside)
{
    if (iface == nullptr || !iface->isValid() || depth > 256)
        return;
    const QAccessible::State state = iface->state();
    // Hidden, so hidden with everything in it.
    if (state.invisible)
        return;
    QObject *object = iface->object();
    QJsonObject element;
    element[QStringLiteral("role")] = int(iface->role());
    element[QStringLiteral("name")] = iface->text(QAccessible::Name);
    element[QStringLiteral("focusable")] = bool(state.focusable);
    element[QStringLiteral("disabled")] = bool(state.disabled);
    element[QStringLiteral("focused")] = bool(state.focused);
    element[QStringLiteral("objectName")] = object != nullptr ? object->objectName() : QString();
    element[QStringLiteral("type")] =
        object != nullptr ? QString::fromLatin1(object->metaObject()->className()) : QString();
    element[QStringLiteral("within")] = within(object);
    element[QStringLiteral("inside")] = inside;
    out.append(element);
    const int role = interactive(iface->role()) ? int(iface->role()) : inside;
    for (int i = 0; i < iface->childCount(); ++i)
        walk(iface->child(i), out, depth + 1, role);
}

// Whether `item` is shown: visible, with every ancestor, and not fully
// transparent.
bool shown(QQuickItem *item)
{
    for (QQuickItem *i = item; i != nullptr; i = i->parentItem()) {
        if (!i->isVisible() || i->opacity() == 0.0)
            return false;
    }
    return true;
}

// Whether anything listens to one of `object`'s click signals. A derived
// class may name QObject's protected isSignalConnected, and the pointer it
// gives works on any QObject.
struct SignalPeek : QObject {
    static bool clickable(const QObject *object)
    {
        const QMetaObject *meta = object->metaObject();
        for (const char *signal : { "clicked(QQuickMouseEvent*)", "pressed(QQuickMouseEvent*)",
                                    "released(QQuickMouseEvent*)", "doubleClicked(QQuickMouseEvent*)" }) {
            const int index = meta->indexOfSignal(signal);
            auto connected = &SignalPeek::isSignalConnected;
            if (index >= 0 && (object->*connected)(meta->method(index)))
                return true;
        }
        return false;
    }
};

// Mouse areas that act on a click, which no element a screen reader can
// act on covers: a control only a mouse can use.
void clickables(QQuickItem *item, QJsonArray &out)
{
    if (item == nullptr || !item->isVisible())
        return;
    if (qstrcmp(item->metaObject()->className(), "QQuickMouseArea") == 0 && item->isEnabled()
        && SignalPeek::clickable(item) && shown(item)) {
        // Covered when the nearest element above it with a role of its own
        // (not a plain item, which every QQuickItem is) is one a screen
        // reader can act on. A list or pane around it doesn't count.
        bool covered = false;
        for (QQuickItem *i = item; i != nullptr; i = i->parentItem()) {
            QAccessibleInterface *iface = QAccessible::queryAccessibleInterface(i);
            if (iface == nullptr || iface->role() == QAccessible::Client
                || iface->role() == QAccessible::NoRole)
                continue;
            covered = interactive(iface->role());
            break;
        }
        if (!covered)
            out.append(within(item));
    }
    for (QQuickItem *child : item->childItems())
        clickables(child, out);
}

} // namespace

extern "C" char *matters_accessibility_report()
{
    // On for the walk, as a screen reader would have it, then as it was,
    // so the tests after these don't run with it.
    const bool wasActive = QAccessible::isActive();
    QAccessible::setActive(true);
    QJsonArray elements;
    QJsonArray uncovered;
    for (QWindow *window : QGuiApplication::topLevelWindows()) {
        if (!window->isVisible())
            continue;
        walk(QAccessible::queryAccessibleInterface(window), elements, 0, 0);
        if (auto *quick = qobject_cast<QQuickWindow *>(window))
            clickables(quick->contentItem(), uncovered);
    }
    QJsonObject report;
    report[QStringLiteral("elements")] = elements;
    report[QStringLiteral("mouseOnly")] = uncovered;
    QAccessible::setActive(wasActive);
    const QByteArray json = QJsonDocument(report).toJson(QJsonDocument::Compact);
    auto *text = static_cast<char *>(std::malloc(json.size() + 1));
    std::memcpy(text, json.constData(), json.size());
    text[json.size()] = '\0';
    return text;
}

extern "C" void matters_accessibility_free(char *report)
{
    std::free(report);
}
