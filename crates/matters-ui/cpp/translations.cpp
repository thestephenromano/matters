// Translations (#75): the catalogs built into Matters, the one for the
// system's languages installed at start, and translation for Rust
// (src/i18n.rs).
#include <QtCore/QByteArray>
#include <QtCore/QCoreApplication>
#include <QtCore/QLibraryInfo>
#include <QtCore/QList>
#include <QtCore/QLocale>
#include <QtCore/QString>
#include <QtCore/QTranslator>
#include <QtGui/QGuiApplication>
#include <QtGui/QWindow>
#include <QtQml/QQmlEngine>
#include <QtQuick/QQuickItem>
#include <QtQuick/QQuickWindow>

#include <cstdlib>
#include <cstring>
#include <memory>

namespace {

struct Catalog {
    QString name;
    const unsigned char *data;
    int size;
};

QList<Catalog> &catalogs()
{
    static QList<Catalog> list;
    return list;
}

std::unique_ptr<QTranslator> &installed()
{
    static std::unique_ptr<QTranslator> translator;
    return translator;
}

char *copy(const QString &text)
{
    const QByteArray utf8 = text.toUtf8();
    auto *out = static_cast<char *>(std::malloc(utf8.size() + 1));
    std::memcpy(out, utf8.constData(), utf8.size());
    out[utf8.size()] = '\0';
    return out;
}

// Install catalog `name`, or none for an empty name, in place of the one
// installed before. True when a catalog was installed.
bool install(const QString &name)
{
    if (installed()) {
        QCoreApplication::removeTranslator(installed().get());
        installed().reset();
    }
    for (const Catalog &catalog : catalogs()) {
        if (catalog.name != name || name.isEmpty())
            continue;
        auto translator = std::make_unique<QTranslator>();
        if (!translator->load(catalog.data, catalog.size))
            return false;
        QCoreApplication::installTranslator(translator.get());
        installed() = std::move(translator);
        return true;
    }
    return false;
}

} // namespace

// A catalog built into Matters (`matters_<name>.qm`), kept for the life of
// the program.
extern "C" void matters_add_catalog(const char *name, const unsigned char *data, int size)
{
    catalogs().append({ QString::fromUtf8(name), data, size });
}

// The catalog for the first of the system's languages that has one
// (`de_DE`, then `de`, as QLocale::uiLanguages lists them), or else the
// English one, which holds the English plural forms. And Qt's own catalog
// for the language, for the words Qt's controls show (OK, Cancel). Call
// once the application exists.
extern "C" void matters_install_system_translation()
{
    static QTranslator qt;
    if (qt.load(QLocale::system(), QStringLiteral("qtbase"), QStringLiteral("_"),
                QLibraryInfo::path(QLibraryInfo::TranslationsPath)))
        QCoreApplication::installTranslator(&qt);
    for (const QString &language : QLocale::system().uiLanguages(QLocale::TagSeparator::Underscore)) {
        if (install(language))
            return;
    }
    install(QStringLiteral("en"));
}

// Use catalog `name` (an empty name: none), and translate every QML
// window again. For the tests' pseudo-translations.
extern "C" bool matters_use_catalog(const char *name)
{
    const bool found = install(QString::fromUtf8(name));
    for (QWindow *window : QGuiApplication::topLevelWindows()) {
        // The window itself when QML made it, or else the item QML put in it.
        QQmlEngine *engine = qmlEngine(window);
        if (auto *quick = qobject_cast<QQuickWindow *>(window); engine == nullptr && quick != nullptr) {
            for (QQuickItem *child : quick->contentItem()->childItems()) {
                if ((engine = qmlEngine(child)) != nullptr)
                    break;
            }
        }
        if (engine != nullptr)
            engine->retranslate();
    }
    return found || name[0] == '\0';
}

// `source` in context `context` (with `disambiguation`, and `n` for
// plurals, -1 for none) in the installed catalog, or `source` itself.
extern "C" char *matters_translate(const char *context, const char *source,
                                   const char *disambiguation, int n)
{
    return copy(QCoreApplication::translate(context, source,
                                            disambiguation[0] == '\0' ? nullptr : disambiguation, n));
}

extern "C" void matters_free_text(char *text)
{
    std::free(text);
}
