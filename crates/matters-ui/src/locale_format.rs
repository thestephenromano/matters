//! Dates and times as the user's locale writes them (#75).
//!
//! Qt gives each locale a long date format ("dddd, MMMM d, yyyy" in
//! American English, "dddd d MMMM yyyy" in French, "yyyy年M月d日dddd" in
//! Japanese) but none for the shorter forms Matters shows: a day and month,
//! with or without the weekday and year. [`pattern`] derives them from the
//! long format, keeping its order, its separators and the words that join
//! its parts ("de" in Spanish, 年 and 月 in Japanese), and dropping the parts
//! not wanted with the separator that went with them. QLocale then writes
//! the date with the locale's names (`cpp/locale_format.cpp`).

use std::ffi::{c_char, c_int, CStr, CString};

use jiff::civil::{Date, Weekday};

extern "C" {
    fn matters_long_date_format(locale: *const c_char) -> *mut c_char;
    fn matters_format_date(
        locale: *const c_char,
        year: c_int,
        month: c_int,
        day: c_int,
        pattern: *const c_char,
    ) -> *mut c_char;
    fn matters_format_time(locale: *const c_char, hour: c_int, minute: c_int) -> *mut c_char;
    fn matters_day_name(locale: *const c_char, day: c_int, long_name: bool) -> *mut c_char;
    fn matters_locale_is_rtl(locale: *const c_char) -> bool;
    fn matters_free_locale_text(text: *mut c_char);
}

/// Read and free a string the C++ side allocated.
///
/// # Safety
/// `text` is a NUL-terminated string from `cpp/locale_format.cpp`, not yet
/// freed.
unsafe fn take(text: *mut c_char) -> String {
    let out = CStr::from_ptr(text).to_string_lossy().into_owned();
    matters_free_locale_text(text);
    out
}

fn cstr(text: &str) -> CString {
    CString::new(text).expect("text without NUL")
}

thread_local! {
    /// The locale `""` stands for on this thread: the system's, unless a
    /// test chose one ([`with_locale`]).
    static LOCALE: std::cell::RefCell<String> = const { std::cell::RefCell::new(String::new()) };
}

/// While it lives, `""` means `locale` on this thread, as if it were the
/// system's: for tests of what is written in "the user's locale".
pub struct LocaleGuard {
    before: String,
}

impl LocaleGuard {
    pub fn new(locale: &str) -> LocaleGuard {
        LocaleGuard {
            before: LOCALE.with(|l| l.replace(locale.to_string())),
        }
    }
}

impl Drop for LocaleGuard {
    fn drop(&mut self) {
        LOCALE.with(|l| *l.borrow_mut() = std::mem::take(&mut self.before));
    }
}

/// Run `f` with `""` meaning `locale` on this thread ([`LocaleGuard`]).
pub fn with_locale<T>(locale: &str, f: impl FnOnce() -> T) -> T {
    let _guard = LocaleGuard::new(locale);
    f()
}

/// `locale`, or for `""` the one in effect on this thread.
fn chosen(locale: &str) -> CString {
    if locale.is_empty() {
        LOCALE.with(|l| cstr(&l.borrow()))
    } else {
        cstr(locale)
    }
}

/// Which parts of a date to write.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Parts {
    pub weekday: bool,
    pub day: bool,
    pub year: bool,
    /// The month's and weekday's short names (Nov, Sat) rather than their
    /// full ones (November, Saturday).
    pub short_month: bool,
}

impl Parts {
    /// "Nov 21", "21 nov.", "11月21日".
    pub const DAY_MONTH: Parts = Parts {
        weekday: false,
        day: true,
        year: false,
        short_month: true,
    };
    /// "Nov 21, 2027".
    pub const DAY_MONTH_YEAR: Parts = Parts {
        weekday: false,
        day: true,
        year: true,
        short_month: true,
    };
    /// "Sat, Nov 21": names short.
    pub const SHORT_WEEKDAY_DAY_MONTH: Parts = Parts {
        weekday: true,
        day: true,
        year: false,
        short_month: true,
    };
    /// "Saturday, November 21".
    pub const WEEKDAY_DAY_MONTH: Parts = Parts {
        weekday: true,
        day: true,
        year: false,
        short_month: false,
    };
    /// "Saturday, November 21, 2027".
    pub const WEEKDAY_DAY_MONTH_YEAR: Parts = Parts {
        weekday: true,
        day: true,
        year: true,
        short_month: false,
    };
    /// "November 2026".
    pub const MONTH_YEAR: Parts = Parts {
        weekday: false,
        day: false,
        year: true,
        short_month: false,
    };
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Field {
    Weekday,
    Day,
    Month,
    Year,
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum Piece {
    /// A field's letters ("MMMM"), with the words that belong to it on
    /// either side (年 after a year, ב before a Hebrew month, г. after a
    /// Russian year), as pattern text.
    Field(Field, String),
    /// What separates fields, as written (not quoted).
    Between(String),
}

fn is_cjk(c: char) -> bool {
    matches!(c as u32,
        0x3040..=0x30FF // Hiragana, Katakana
        | 0x3400..=0x4DBF | 0x4E00..=0x9FFF // Han
        | 0xAC00..=0xD7AF) // Hangul
}

/// A literal as pattern text: each word with letters QLocale would read as
/// fields (or a quote) quoted, the spaces between them left as they are.
fn quoted(text: &str) -> String {
    fn flush(word: &mut String, out: &mut String) {
        if word.chars().any(|c| c.is_ascii_alphabetic() || c == '\'') {
            out.push('\'');
            out.push_str(&word.replace('\'', "''"));
            out.push('\'');
        } else {
            out.push_str(word);
        }
        word.clear();
    }
    let mut out = String::new();
    let mut word = String::new();
    for c in text.chars() {
        if c.is_whitespace() {
            flush(&mut word, &mut out);
            out.push(c);
        } else {
            word.push(c);
        }
    }
    flush(&mut word, &mut out);
    out
}

/// The long format's fields and the literals between them.
fn tokens(format: &str) -> Vec<Piece> {
    let mut pieces = Vec::new();
    let mut literal = String::new();
    let mut chars = format.chars().peekable();
    while let Some(c) = chars.next() {
        match c {
            '\'' => {
                // A quoted literal; '' is a quote.
                if chars.peek() == Some(&'\'') {
                    chars.next();
                    literal.push('\'');
                    continue;
                }
                while let Some(q) = chars.next() {
                    if q == '\'' {
                        if chars.peek() == Some(&'\'') {
                            chars.next();
                            literal.push('\'');
                            continue;
                        }
                        break;
                    }
                    literal.push(q);
                }
            }
            'd' | 'M' | 'y' => {
                let mut run = String::from(c);
                while chars.peek() == Some(&c) {
                    run.push(c);
                    chars.next();
                }
                if !literal.is_empty() {
                    pieces.push(Piece::Between(std::mem::take(&mut literal)));
                }
                let field = match (c, run.len()) {
                    ('d', 3..) => Field::Weekday,
                    ('d', _) => Field::Day,
                    ('M', _) => Field::Month,
                    _ => Field::Year,
                };
                pieces.push(Piece::Field(field, run));
            }
            other => literal.push(other),
        }
    }
    if !literal.is_empty() {
        pieces.push(Piece::Between(literal));
    }
    attach_words(pieces)
}

/// Words that belong to a field join it: CJK characters right after one
/// (年, 월), letters right before one with no space (Hebrew ב), and
/// letters after the last one (Russian " г.").
fn attach_words(pieces: Vec<Piece>) -> Vec<Piece> {
    let mut out: Vec<Piece> = Vec::new();
    let last_field = pieces.iter().rposition(|p| matches!(p, Piece::Field(..)));
    for (i, piece) in pieces.into_iter().enumerate() {
        match piece {
            Piece::Between(text) => {
                let after_field = matches!(out.last(), Some(Piece::Field(..)));
                // CJK right after a field: its suffix.
                let suffix: String = if after_field {
                    text.chars().take_while(|c| is_cjk(*c)).collect()
                } else {
                    String::new()
                };
                let mut rest: String = text[suffix.len()..].to_string();
                if !suffix.is_empty() {
                    if let Some(Piece::Field(_, pattern)) = out.last_mut() {
                        pattern.push_str(&quoted(&suffix));
                    }
                }
                // Letters after the last field: its suffix too.
                if after_field
                    && last_field.is_some_and(|l| i > l)
                    && rest.chars().any(char::is_alphabetic)
                {
                    if let Some(Piece::Field(_, pattern)) = out.last_mut() {
                        pattern.push_str(&quoted(&rest));
                    }
                    rest.clear();
                }
                if !rest.is_empty() {
                    out.push(Piece::Between(rest));
                }
            }
            Piece::Field(field, pattern) => {
                // Letters right before a field, with no space: its prefix.
                let prefix = match out.last_mut() {
                    Some(Piece::Between(text)) => {
                        let start = text
                            .char_indices()
                            .rev()
                            .take_while(|(_, c)| c.is_alphabetic() && !is_cjk(*c))
                            .last()
                            .map(|(at, _)| at);
                        start.map(|at| text.split_off(at)).unwrap_or_default()
                    }
                    _ => String::new(),
                };
                if matches!(out.last(), Some(Piece::Between(t)) if t.is_empty()) {
                    out.pop();
                }
                out.push(Piece::Field(field, format!("{}{pattern}", quoted(&prefix))));
            }
        }
    }
    out
}

/// The pattern for `parts`, from the locale's long date format.
pub fn pattern(long_format: &str, parts: Parts) -> String {
    let wanted = |f: Field| match f {
        Field::Weekday => parts.weekday,
        Field::Day => parts.day,
        Field::Month => true,
        Field::Year => parts.year,
    };
    let mut pieces = tokens(long_format);
    // Drop the fields not wanted, each with the separator after it, or
    // before it when no field follows.
    while let Some(at) = pieces
        .iter()
        .position(|p| matches!(p, Piece::Field(f, _) if !wanted(*f)))
    {
        let field_after = pieces[at + 1..]
            .iter()
            .any(|p| matches!(p, Piece::Field(..)));
        if field_after && matches!(pieces.get(at + 1), Some(Piece::Between(_))) {
            pieces.remove(at + 1);
        } else if !field_after && at > 0 && matches!(pieces[at - 1], Piece::Between(_)) {
            pieces.remove(at - 1);
            pieces.remove(at - 1);
            continue;
        }
        pieces.remove(at);
    }
    // No separator at either end.
    while matches!(pieces.first(), Some(Piece::Between(_))) {
        pieces.remove(0);
    }
    while matches!(pieces.last(), Some(Piece::Between(_))) {
        pieces.pop();
    }
    let mut out = String::new();
    for piece in pieces {
        match piece {
            Piece::Field(Field::Month, pattern) if parts.short_month => {
                out.push_str(&pattern.replace("MMMM", "MMM"));
            }
            Piece::Field(Field::Weekday, pattern) if parts.short_month => {
                out.push_str(&pattern.replace("dddd", "ddd"));
            }
            Piece::Field(_, pattern) => out.push_str(&pattern),
            Piece::Between(text) => out.push_str(&quoted(&text)),
        }
    }
    out
}

/// `date` written in `locale` ("" for the system's) with `parts`.
pub fn date(locale: &str, date: Date, parts: Parts) -> String {
    let locale = chosen(locale);
    // SAFETY: NUL-terminated arguments that outlive the calls; results
    // are taken once.
    unsafe {
        let long = take(matters_long_date_format(locale.as_ptr()));
        let pattern = cstr(&pattern(&long, parts));
        take(matters_format_date(
            locale.as_ptr(),
            c_int::from(date.year()),
            c_int::from(date.month()),
            c_int::from(date.day()),
            pattern.as_ptr(),
        ))
    }
}

/// `hour`:`minute` in `locale`'s short time format.
pub fn time(locale: &str, hour: i8, minute: i8) -> String {
    let locale = chosen(locale);
    // SAFETY: as in `date`.
    unsafe {
        take(matters_format_time(
            locale.as_ptr(),
            c_int::from(hour),
            c_int::from(minute),
        ))
    }
}

/// The weekday's name in `locale`, long ("Monday") or short ("Mon").
pub fn day_name(locale: &str, day: Weekday, long: bool) -> String {
    let locale = chosen(locale);
    // SAFETY: as in `date`.
    unsafe {
        take(matters_day_name(
            locale.as_ptr(),
            c_int::from(day.to_monday_one_offset()),
            long,
        ))
    }
}

/// Whether `locale` writes right to left.
pub fn is_rtl(locale: &str) -> bool {
    let locale = chosen(locale);
    // SAFETY: a NUL-terminated argument for the call.
    unsafe { matters_locale_is_rtl(locale.as_ptr()) }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_short_forms_keep_the_locales_order_and_words() {
        let cases = [
            (
                "dddd, MMMM d, yyyy",
                ["MMM d", "MMM d, yyyy", "dddd, MMMM d", "MMMM yyyy"],
            ),
            (
                "dddd d MMMM yyyy",
                ["d MMM", "d MMM yyyy", "dddd d MMMM", "MMMM yyyy"],
            ),
            (
                "dddd, d. MMMM yyyy",
                ["d. MMM", "d. MMM yyyy", "dddd, d. MMMM", "MMMM yyyy"],
            ),
            (
                "dddd, d 'de' MMMM 'de' yyyy",
                [
                    "d 'de' MMM",
                    "d 'de' MMM 'de' yyyy",
                    "dddd, d 'de' MMMM",
                    "MMMM 'de' yyyy",
                ],
            ),
            (
                "yyyy年M月d日dddd",
                ["M月d日", "yyyy年M月d日", "M月d日dddd", "yyyy年M月"],
            ),
            (
                "yyyy년 M월 d일 dddd",
                ["M월 d일", "yyyy년 M월 d일", "M월 d일 dddd", "yyyy년 M월"],
            ),
            (
                "dddd، d MMMM yyyy",
                ["d MMM", "d MMM yyyy", "dddd، d MMMM", "MMMM yyyy"],
            ),
            (
                "dddd, d בMMMM yyyy",
                ["d בMMM", "d בMMM yyyy", "dddd, d בMMMM", "בMMMM yyyy"],
            ),
            (
                "dddd, d MMMM yyyy 'г'.",
                ["d MMM", "d MMM yyyy г.", "dddd, d MMMM", "MMMM yyyy г."],
            ),
        ];
        for (long, [day_month, with_year, with_weekday, month_year]) in cases {
            assert_eq!(pattern(long, Parts::DAY_MONTH), day_month, "{long}");
            assert_eq!(pattern(long, Parts::DAY_MONTH_YEAR), with_year, "{long}");
            assert_eq!(
                pattern(long, Parts::WEEKDAY_DAY_MONTH),
                with_weekday,
                "{long}"
            );
            assert_eq!(pattern(long, Parts::MONTH_YEAR), month_year, "{long}");
            assert_eq!(
                pattern(long, Parts::WEEKDAY_DAY_MONTH_YEAR),
                // Only Qt's letters need quotes.
                long.replace("'г'", "г"),
                "the whole format is the long one"
            );
        }
    }

    #[test]
    fn quoted_literals_and_quotes_survive() {
        assert_eq!(
            pattern("d 'o''clock' MMMM yyyy", Parts::DAY_MONTH),
            "d 'o''clock' MMM"
        );
    }

    #[test]
    fn qt_writes_the_days_in_each_locale() {
        let day = jiff::civil::date(2026, 11, 21);
        assert_eq!(date("en_US", day, Parts::DAY_MONTH), "Nov 21");
        assert_eq!(
            date("en_US", day, Parts::WEEKDAY_DAY_MONTH),
            "Saturday, November 21"
        );
        assert_eq!(date("en_US", day, Parts::MONTH_YEAR), "November 2026");
        assert_eq!(
            date("en_US", day, Parts::SHORT_WEEKDAY_DAY_MONTH),
            "Sat, Nov 21"
        );
        assert_eq!(
            date("en_GB", day, Parts::SHORT_WEEKDAY_DAY_MONTH),
            "Sat, 21 Nov"
        );
        assert_eq!(with_locale("de_DE", || time("", 9, 5)), "09:05");
        assert_eq!(date("en_GB", day, Parts::DAY_MONTH), "21 Nov");
        assert_eq!(
            date("de_DE", day, Parts::WEEKDAY_DAY_MONTH),
            "Samstag, 21. November"
        );
        assert_eq!(date("ja_JP", day, Parts::DAY_MONTH), "11月21日");
        assert_eq!(date("fr_FR", day, Parts::DAY_MONTH_YEAR), "21 nov. 2026");
        assert_eq!(time("en_US", 14, 30), "2:30\u{202f}PM");
        assert_eq!(time("de_DE", 14, 30), "14:30");
        assert_eq!(day_name("en_US", Weekday::Monday, false), "Mon");
        assert_eq!(day_name("fr_FR", Weekday::Sunday, true), "dimanche");
        assert!(is_rtl("ar_EG") && is_rtl("he_IL") && !is_rtl("en_US"));
    }

    /// Qt's own long formats, for the locales above, give what the patterns
    /// were derived from: if CLDR changes one, this says which.
    #[test]
    fn the_long_formats_are_the_ones_the_patterns_assume() {
        let long = |locale: &str| {
            let locale = chosen(locale);
            // SAFETY: as in `date`.
            unsafe { take(matters_long_date_format(locale.as_ptr())) }
        };
        assert_eq!(long("en_US"), "dddd, MMMM d, yyyy");
        assert_eq!(long("ja_JP"), "yyyy年M月d日dddd");
    }
}
