// Dates, times and day names in the user's locale (#75), for
// src/locale_format.rs. `locale` names a locale ("de_DE"); empty means the
// system's.
#include <QtCore/QByteArray>
#include <QtCore/QDate>
#include <QtCore/QLocale>
#include <QtCore/QString>
#include <QtCore/QTime>

#include <cstdlib>
#include <cstring>

namespace {

QLocale locale(const char *name)
{
    return name[0] == '\0' ? QLocale::system() : QLocale(QString::fromUtf8(name));
}

char *copy(const QString &text)
{
    const QByteArray utf8 = text.toUtf8();
    auto *out = static_cast<char *>(std::malloc(utf8.size() + 1));
    std::memcpy(out, utf8.constData(), utf8.size());
    out[utf8.size()] = '\0';
    return out;
}

} // namespace

// The locale's long date format, as QLocale writes it ("dddd, MMMM d, yyyy").
extern "C" char *matters_long_date_format(const char *name)
{
    return copy(locale(name).dateFormat(QLocale::LongFormat));
}

// The day `year`-`month`-`day` in `pattern` (QLocale's format letters),
// with the locale's names.
extern "C" char *matters_format_date(const char *name, int year, int month, int day,
                                     const char *pattern)
{
    return copy(locale(name).toString(QDate(year, month, day), QString::fromUtf8(pattern)));
}

// `hour`:`minute` in the locale's short time format (09:00, 9:00 AM).
extern "C" char *matters_format_time(const char *name, int hour, int minute)
{
    return copy(locale(name).toString(QTime(hour, minute), QLocale::ShortFormat));
}

// Weekday `day` (1 = Monday) by name, long or short.
extern "C" char *matters_day_name(const char *name, int day, bool long_name)
{
    return copy(locale(name).dayName(day, long_name ? QLocale::LongFormat : QLocale::ShortFormat));
}

// Whether the locale writes right to left.
extern "C" bool matters_locale_is_rtl(const char *name)
{
    return locale(name).textDirection() == Qt::RightToLeft;
}

extern "C" void matters_free_locale_text(char *text)
{
    std::free(text);
}
