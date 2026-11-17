// Entry point into Qt Quick Test for the `ui-tests` feature. Built only when
// that feature is enabled, so release binaries do not link QtQuickTest.
#include <QtQuickTest/quicktest.h>

extern "C" int matters_quick_test_main(int argc, char **argv, const char *source_dir)
{
    return quick_test_main(argc, argv, "matters", source_dir);
}
