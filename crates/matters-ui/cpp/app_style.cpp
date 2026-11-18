// Qt Quick Controls style selection, called from Rust (src/lib.rs).
#include <QtQuickControls2/QQuickStyle>

// Set the style. Call after the application exists, so the desktop's
// platform theme has already made its own choice, and before any QML that
// imports QtQuick.Controls is loaded.
extern "C" void matters_set_quick_style(const char *name)
{
    QQuickStyle::setStyle(QString::fromUtf8(name));
}

// Write the style in effect, NUL-terminated and truncated to `capacity`, into
// `out`. Used by the style tests.
extern "C" void matters_quick_style_name(char *out, int capacity)
{
    if (capacity <= 0)
        return;
    const QByteArray name = QQuickStyle::name().toUtf8();
    const int length = qMin(int(name.size()), capacity - 1);
    memcpy(out, name.constData(), size_t(length));
    out[length] = '\0';
}
