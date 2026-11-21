// The application's identity for the desktop, called from Rust (src/lib.rs).
#include <QtGui/QGuiApplication>

// Name the desktop entry (without ".desktop") the desktop matches the window
// to, for its icon and name (the Wayland app ID).
// Static: call before or after the application exists.
extern "C" void matters_set_desktop_file_name(const char *name)
{
    QGuiApplication::setDesktopFileName(QString::fromUtf8(name));
}
