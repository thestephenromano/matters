// Entry point into Qt Quick Test for the `ui-tests` feature. Built only when
// that feature is enabled, so release binaries do not link QtQuickTest.
#include <QtCore/QCoreApplication>
#include <QtQuickTest/quicktest.h>

// The tests run in English with its plural forms ("1 open task"), as the
// app does for a language without a catalog (#75): cpp/translations.cpp.
extern "C" bool matters_use_catalog(const char *name);

static void english_by_default()
{
    matters_use_catalog("en");
}
Q_COREAPP_STARTUP_FUNCTION(english_by_default)

extern "C" int matters_quick_test_main(int argc, char **argv, const char *source_dir)
{
    return quick_test_main(argc, argv, "matters", source_dir);
}
