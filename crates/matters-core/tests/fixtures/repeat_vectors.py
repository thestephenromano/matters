"""Golden occurrence dates for repeat rules (#61), from python-dateutil.

dateutil's rrule is an independent implementation of RFC 5545 recurrence
rules. Each case pairs a Matters rule (as `RepeatRule` serializes it) with
the dates dateutil gives for the equivalent RRULE, so the Rust calculator
(`matters_core::repeat`) is checked against it.

Regenerate with:

    python3 -I crates/matters-core/tests/fixtures/repeat_vectors.py \
        > crates/matters-core/tests/fixtures/repeat_vectors.json

Generated with python-dateutil 2.9.0.post0.
"""

import json
import sys
from datetime import date, datetime

import dateutil
from dateutil import rrule as rr

WEEKDAYS = [rr.MO, rr.TU, rr.WE, rr.TH, rr.FR, rr.SA, rr.SU]
COUNT = 24


def iso(d):
    return d.isoformat()


def case(name, start, every, frequency, until=None):
    """A case: the rule as Matters stores it, and dateutil's dates."""
    y, m, d = (int(p) for p in start.split("-"))
    dtstart = datetime(y, m, d)
    kwargs = {"dtstart": dtstart, "interval": every, "wkst": rr.MO}
    if until is None:
        kwargs["count"] = COUNT
    else:
        uy, um, ud = (int(p) for p in until.split("-"))
        kwargs["until"] = datetime(uy, um, ud)
    kind = frequency["kind"]
    if kind == "daily":
        freq = rr.DAILY
    elif kind == "weekly":
        freq = rr.WEEKLY
        kwargs["byweekday"] = [WEEKDAYS[n - 1] for n in frequency["days"]]
    elif kind == "monthly_on_day":
        freq = rr.MONTHLY
        kwargs["bymonthday"] = frequency["day"]
    elif kind == "monthly_on_weekday":
        freq = rr.MONTHLY
        kwargs["byweekday"] = WEEKDAYS[frequency["weekday"] - 1](frequency["nth"])
    elif kind == "yearly":
        freq = rr.YEARLY
        kwargs["bymonth"] = frequency["month"]
        kwargs["bymonthday"] = frequency["day"]
    else:
        raise ValueError(kind)
    dates = [iso(dt.date()) for dt in rr.rrule(freq, **kwargs)]
    rule = {"every": every, "frequency": frequency, "start": start, "until": until}
    return {"name": name, "rule": rule, "dates": dates}


CASES = [
    case("daily", "2026-11-21", 1, {"kind": "daily"}),
    case("every 3 days across a year's end", "2026-12-20", 3, {"kind": "daily"}),
    case("every 10 days across February in a leap year", "2028-02-15", 10, {"kind": "daily"}),
    case("daily until", "2026-11-21", 1, {"kind": "daily"}, until="2026-11-30"),
    case("weekly on the start's day", "2026-11-21", 1, {"kind": "weekly", "days": [6]}),
    case("weekly on Monday and Thursday", "2026-11-18", 1, {"kind": "weekly", "days": [1, 4]}),
    case("every 2 weeks, Mon/Wed/Fri, starting Sunday", "2026-11-22", 2,
         {"kind": "weekly", "days": [1, 3, 5]}),
    case("every 3 weeks on Sunday, starting Monday", "2026-11-23", 3,
         {"kind": "weekly", "days": [7]}),
    case("every 2 weeks on Tuesday, starting Thursday", "2026-12-31", 2,
         {"kind": "weekly", "days": [2]}),
    case("weekly, days listed out of order", "2026-11-21", 1,
         {"kind": "weekly", "days": [5, 1, 3]}),
    case("weekly until", "2026-11-21", 1, {"kind": "weekly", "days": [2, 6]}, until="2027-01-10"),
    case("monthly on the 15th", "2026-11-15", 1, {"kind": "monthly_on_day", "day": 15}),
    case("monthly on the 31st skips short months", "2026-01-31", 1,
         {"kind": "monthly_on_day", "day": 31}),
    case("monthly on the 30th skips February", "2027-01-30", 1,
         {"kind": "monthly_on_day", "day": 30}),
    case("monthly on the 29th in a leap year", "2028-01-29", 1,
         {"kind": "monthly_on_day", "day": 29}),
    case("monthly on the last day", "2026-01-31", 1, {"kind": "monthly_on_day", "day": -1}),
    case("every 2 months on the 31st", "2026-01-31", 2, {"kind": "monthly_on_day", "day": 31}),
    case("every 3 months on the 10th, starting after it", "2026-11-20", 3,
         {"kind": "monthly_on_day", "day": 10}),
    case("monthly on the 1st, starting mid-month", "2026-11-21", 1,
         {"kind": "monthly_on_day", "day": 1}),
    case("monthly on the third Saturday", "2026-11-21", 1,
         {"kind": "monthly_on_weekday", "nth": 3, "weekday": 6}),
    case("monthly on the first Monday, starting after it", "2026-11-03", 1,
         {"kind": "monthly_on_weekday", "nth": 1, "weekday": 1}),
    case("monthly on the last Friday", "2026-11-27", 1,
         {"kind": "monthly_on_weekday", "nth": -1, "weekday": 5}),
    case("monthly on the fifth Thursday skips months without one", "2026-10-29", 1,
         {"kind": "monthly_on_weekday", "nth": 5, "weekday": 4}),
    case("every 2 months on the second Tuesday", "2026-12-08", 2,
         {"kind": "monthly_on_weekday", "nth": 2, "weekday": 2}),
    case("yearly on 21 November", "2026-11-21", 1, {"kind": "yearly", "month": 11, "day": 21}),
    case("yearly on 29 February", "2028-02-29", 1, {"kind": "yearly", "month": 2, "day": 29}),
    case("yearly on 29 February from a common year", "2026-01-01", 1,
         {"kind": "yearly", "month": 2, "day": 29}),
    case("every 2 years on 31 December", "2026-12-31", 2,
         {"kind": "yearly", "month": 12, "day": 31}),
    case("yearly on 1 March, starting after it", "2026-03-02", 1,
         {"kind": "yearly", "month": 3, "day": 1}),
    case("yearly until", "2026-06-15", 1, {"kind": "yearly", "month": 6, "day": 15},
         until="2030-06-15"),
]

if __name__ == "__main__":
    out = {"generator": "python-dateutil " + dateutil.__version__, "cases": CASES}
    json.dump(out, sys.stdout, indent=1)
    sys.stdout.write("\n")
