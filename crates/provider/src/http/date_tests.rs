//! The `Date` header as Unix seconds: IMF-fixdate only (RFC 9110, 5.6.7),
//! so the usage-limit wait is measured from the failed reply itself
//! (`docs/model-routing.md`, "Protocols and providers").

use super::http_date;

#[test]
fn an_imf_fixdate_is_its_unix_seconds() {
    for (text, seconds) in [
        ("Wed, 07 Oct 2026 16:00:00 GMT", 1_791_388_800),
        ("Thu, 01 Jan 1970 00:00:00 GMT", 0),
        ("Thu, 01 Jan 1970 00:00:01 GMT", 1),
        ("Mon, 29 Feb 2028 00:00:00 GMT", 1_835_395_200),
        ("Thu, 31 Dec 2026 23:59:59 GMT", 1_798_761_599),
    ] {
        assert_eq!(http_date(text), Some(seconds), "{text}");
    }
}

#[test]
fn anything_but_an_imf_fixdate_is_none() {
    for text in [
        "",
        "not a date",
        "Wed, 07 Oct 2026 16:00:00",      // no zone
        "Wed, 07 Oct 2026 16:00:00 UTC",  // not GMT
        "Wed, 07 Oct 2026 16:00:00 gmt",  // the zone is exactly `GMT`
        "Wed, 7 Oct 2026 16:00:00 GMT",   // the day is two digits
        "Wed, 007 Oct 2026 16:00:00 GMT", // the day is two digits
        "Wed, 07 oct 2026 16:00:00 GMT",  // the month is exactly `Oct`
        "Wed, 07 Foo 2026 16:00:00 GMT",  // no such month
        "Wed, 07 Oct 26 16:00:00 GMT",    // the year is four digits
        "Wed 07 Oct 2026 16:00:00 GMT",   // no comma after the weekday
        "Wed, 07 Oct 2026 16:00 GMT",     // no seconds
        "Wed, 07 Oct 2026 16:00:00 GMT ", // nothing after the zone
        " Wed, 07 Oct 2026 16:00:00 GMT", // nothing before the weekday
        "Wed, 31 Dec 1969 23:59:59 GMT",  // before the epoch
        "Thu, 01 Jan 1970 00:00:60 GMT",  // no leap second
    ] {
        assert_eq!(http_date(text), None, "{text}");
    }
}

#[test]
fn out_of_range_fields_are_none() {
    for text in [
        "Wed, 00 Oct 2026 16:00:00 GMT", // the day starts at 01
        "Wed, 32 Oct 2026 16:00:00 GMT", // October has 31 days
        "Wed, 31 Sep 2026 16:00:00 GMT", // September has 30
        "Wed, 29 Feb 2027 16:00:00 GMT", // 2027 is no leap year
        "Wed, 30 Feb 2028 16:00:00 GMT", // February never has 30
        "Wed, 07 Oct 2026 24:00:00 GMT", // the hour ends at 23
        "Wed, 07 Oct 2026 16:60:00 GMT", // the minute ends at 59
        "Wed, 07 Oct 2026 16:00:61 GMT", // the second ends at 59
        "Wed, 07 Oct 2026 1x:00:00 GMT", // digits only
    ] {
        assert_eq!(http_date(text), None, "{text}");
    }
}

#[test]
fn the_weekday_keeps_its_shape_but_is_not_checked() {
    // A wrong weekday still parses: only the shape matters.
    assert_eq!(
        http_date("Mon, 07 Oct 2026 16:00:00 GMT"),
        Some(1_791_388_800)
    );
    // But the three-letter shape is required.
    for text in [
        "W, 07 Oct 2026 16:00:00 GMT",
        "Wedn, 07 Oct 2026 16:00:00 GMT",
        "We3, 07 Oct 2026 16:00:00 GMT",
    ] {
        assert_eq!(http_date(text), None, "{text}");
    }
}
