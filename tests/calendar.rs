mod common;
use chrono::{DateTime, Datelike, NaiveDate, TimeZone, Utc};
use chrono_tz::{America::Los_Angeles, Europe::Paris, Tz, UTC};
use common::epoch;
use publicator::{
    calendar::{Event, months, timeline},
    error::Error,
    model::{Delivery, Publications},
};
use serde_json::json;

fn row(n: usize, title: &str, timestamp: i64) -> serde_json::Value {
    json!({"uuid":format!("row-{n}"),"state":"DESCRIBED","title":title,"description":"d",
        "submissionTimestamp":timestamp,"files":[{"basename":"art.png","sha512sum":"a".repeat(128)}],
        "apparitions":[]})
}

#[test]
fn calendar_keeps_two_apparitions_with_one_shared_title() {
    let mut p = row(1, "Shared title", 1700000000);
    p["apparitions"] = json!([
        {"platformName":"deviantart","state":"unpublished","url":"https://da.example/artwork/123"},
        {"platformName":"pixiv","state":"published_or_scheduled","url":"https://pixiv.example/artwork/123"}
    ]);
    let ev = timeline(&[p]).unwrap();
    assert_eq!(ev.len(), 2);
    assert_ne!(ev[0].apparition_index, ev[1].apparition_index);
    assert_eq!(ev[0].title, "Shared title");
    assert_eq!(ev[1].title, "Shared title");
    assert_eq!(ev[0].platform, "deviantart");
    assert_eq!(ev[1].platform, "pixiv");
    assert_eq!(ev[0].url.as_deref(), Some("https://da.example/artwork/123"));
    assert_eq!(
        ev[1].url.as_deref(),
        Some("https://pixiv.example/artwork/123")
    );
    assert_eq!(ev[0].delivery, Delivery::Unpublished);
    assert_eq!(ev[1].delivery, Delivery::PublishedOrScheduled);
    assert_eq!(ev[0].basename.as_deref(), Some("art.png"));
}

#[test]
fn calendar_labels_pending_metadata_and_preserves_file_name() {
    let mut complete = row(2, "Stored title", 1700000000);
    complete["state"] = json!("SCHEDULED");
    complete["apparitions"] = json!([{"platformName":"other","state":"unpublished"}]);
    assert_eq!(timeline(&[complete]).unwrap()[0].title, "Stored title");
    let mut p = row(1, "real", 1700000000);
    p["state"] = json!("SCHEDULED");
    p["description"] = json!(" ");
    p["files"][0] = json!({"basename":"old.png"});
    p["apparitions"] = json!([{"platformName":"other","state":"unpublished"}]);
    let event = &timeline(&[p]).unwrap()[0];
    assert_eq!(event.title, "AI metadata pending");
    assert_eq!(event.url, None);
    assert_eq!(event.basename.as_deref(), Some("old.png"));
    assert_eq!(event.timestamp, 1700000000);
}

#[test]
fn calendar_orders_events_and_months_include_now() {
    let mut later = row(
        2,
        "Later",
        Paris
            .with_ymd_and_hms(2026, 3, 3, 20, 0, 0)
            .unwrap()
            .timestamp(),
    );
    later["apparitions"] = json!([{"platformName":"pixiv","state":"unpublished"}]);
    let mut earlier = row(
        1,
        "Earlier",
        Paris
            .with_ymd_and_hms(2025, 11, 3, 20, 0, 0)
            .unwrap()
            .timestamp(),
    );
    earlier["apparitions"] =
        json!([{"platformName":"historical","state":"published_or_scheduled"}]);
    let rows: Publications = vec![later, earlier];
    let events = timeline(&rows).unwrap();
    assert_eq!(
        events.iter().map(|e| e.title.as_str()).collect::<Vec<_>>(),
        vec!["Earlier", "Later"]
    );
    let now = Paris
        .with_ymd_and_hms(2026, 1, 6, 12, 0, 0)
        .unwrap()
        .timestamp();
    assert_eq!(
        months(&events, Paris, now).unwrap(),
        vec![
            NaiveDate::from_ymd_opt(2025, 11, 1).unwrap(),
            NaiveDate::from_ymd_opt(2025, 12, 1).unwrap(),
            NaiveDate::from_ymd_opt(2026, 1, 1).unwrap(),
            NaiveDate::from_ymd_opt(2026, 2, 1).unwrap(),
            NaiveDate::from_ymd_opt(2026, 3, 1).unwrap(),
        ]
    );
    assert_eq!(
        months(&[], Paris, now).unwrap(),
        vec![NaiveDate::from_ymd_opt(2026, 1, 1).unwrap()]
    );
}

#[test]
fn calendar_falls_back_to_submission_time_and_handles_publications_without_files() {
    let mut p = row(1, "Fallback", 1700000000);
    p.as_object_mut()
        .unwrap()
        .shift_remove("apparitionTimestampIfDifferentThanSubmission");
    p["files"] = json!([]);
    p["apparitions"] = json!([{"platformName":"deviantart","state":"unpublished"}]);
    let event = &timeline(&[p]).unwrap()[0];
    assert_eq!(event.timestamp, 1700000000);
    assert_eq!(event.basename, None);
    assert_eq!(event.url, None);
}

#[test]
fn calendar_keeps_equal_timestamp_apparitions_in_source_order() {
    let mut first = row(1, "First", 1700000000);
    first["apparitions"] = json!([
        {"platformName":"first-platform","state":"unpublished"},
        {"platformName":"second-platform","state":"unpublished"}
    ]);
    let mut second = row(2, "Second", 1700000000);
    second["apparitions"] = json!([{"platformName":"third-platform","state":"unpublished"}]);
    let events = timeline(&[first, second]).unwrap();
    assert_eq!(
        events
            .iter()
            .map(|event| event.platform.as_str())
            .collect::<Vec<_>>(),
        ["first-platform", "second-platform", "third-platform"]
    );
}

#[test]
fn calendar_treats_a_publication_without_apparitions_as_empty() {
    let mut p = row(1, "No delivery", 1700000000);
    p.as_object_mut().unwrap().shift_remove("apparitions");
    assert!(timeline(&[p]).unwrap().is_empty());
}

#[test]
fn calendar_uses_publication_schedule_for_every_platform() {
    let mut p = row(1, "Shared", 1700000000);
    p["apparitionTimestampIfDifferentThanSubmission"] = json!(
        Paris
            .with_ymd_and_hms(2026, 2, 1, 0, 30, 0)
            .unwrap()
            .timestamp()
    );
    p["apparitions"] = json!([{"platformName":"one","state":"unpublished"},{"platformName":"two","state":"unpublished"}]);
    let events = timeline(&[p]).unwrap();
    assert_eq!(events[0].timestamp, events[1].timestamp);
    assert_eq!(
        events[0].timestamp,
        Paris
            .with_ymd_and_hms(2026, 2, 1, 0, 30, 0)
            .unwrap()
            .timestamp()
    );
}

fn at(timestamp: i64) -> Event {
    Event {
        uuid: "event".into(),
        apparition_index: 0,
        platform: "deviantart".into(),
        title: "Title".into(),
        timestamp,
        url: None,
        basename: None,
        delivery: Delivery::Unpublished,
    }
}

fn month(year: i32, month: u32) -> NaiveDate {
    NaiveDate::from_ymd_opt(year, month, 1).unwrap()
}

fn months_of(pairs: &[(i32, u32)]) -> Vec<NaiveDate> {
    pairs.iter().map(|&(year, m)| month(year, m)).collect()
}

/// The month bucket of a single event, with now at the same instant.
fn bucket(timestamp: i64, zone: Tz) -> publicator::error::Result<Vec<NaiveDate>> {
    months(&[at(timestamp)], zone, timestamp)
}

#[test]
fn leap_february_ends_at_local_midnight_after_the_29th() {
    let late_29th = epoch("2028-02-29T22:30:00Z"); // 23:30 in Paris
    let after = epoch("2028-02-29T23:30:00Z"); // 00:30 on 1 March in Paris
    assert_eq!(bucket(late_29th, Paris).unwrap(), [month(2028, 2)]);
    assert_eq!(bucket(after, Paris).unwrap(), [month(2028, 3)]);
    assert_eq!(bucket(after, UTC).unwrap(), [month(2028, 2)]);
}

#[test]
fn year_rollover_follows_the_configured_zone() {
    let instant = epoch("2026-12-31T23:30:00Z");
    assert_eq!(bucket(instant, Paris).unwrap(), [month(2027, 1)]);
    assert_eq!(bucket(instant, UTC).unwrap(), [month(2026, 12)]);
    assert_eq!(bucket(instant, Los_Angeles).unwrap(), [month(2026, 12)]);
    let new_year_utc = epoch("2027-01-01T05:00:00Z"); // 21:00 on 31 December in Los Angeles
    assert_eq!(bucket(new_year_utc, UTC).unwrap(), [month(2027, 1)]);
    assert_eq!(
        bucket(new_year_utc, Los_Angeles).unwrap(),
        [month(2026, 12)]
    );
}

#[test]
fn month_span_crosses_the_year_boundary_in_order() {
    let events = [
        at(epoch("2027-02-28T23:30:00Z")), // 1 March 2027 in Paris
        at(epoch("2026-11-15T12:00:00Z")),
    ];
    let now = epoch("2026-12-31T23:30:00Z"); // January 2027 in Paris
    assert_eq!(
        months(&events, Paris, now).unwrap(),
        months_of(&[(2026, 11), (2026, 12), (2027, 1), (2027, 2), (2027, 3)])
    );
    assert_eq!(
        months(&events, UTC, now).unwrap(),
        months_of(&[(2026, 11), (2026, 12), (2027, 1), (2027, 2)])
    );
}

#[test]
fn out_of_range_timestamps_are_rejected_instead_of_panicking() {
    let max = DateTime::<Utc>::MAX_UTC.timestamp();
    let min = DateTime::<Utc>::MIN_UTC.timestamp();
    let now = epoch("2026-01-01T00:00:00Z");
    for timestamp in [i64::MAX, i64::MIN, max + 1, min - 1] {
        let event = months(&[at(timestamp)], UTC, now);
        assert!(
            matches!(event, Err(Error::Validation(_))),
            "{timestamp}: {event:?}"
        );
        let current = months(&[], UTC, timestamp);
        assert!(
            matches!(current, Err(Error::Validation(_))),
            "{timestamp}: {current:?}"
        );
    }
    // Representable instants whose local date in the zone is outside chrono's range.
    for (timestamp, zone) in [(max, Paris), (min, Los_Angeles)] {
        let current = bucket(timestamp, zone);
        assert!(
            matches!(current, Err(Error::Validation(_))),
            "{zone}: {current:?}"
        );
        let event = months(&[at(timestamp)], zone, now);
        assert!(
            matches!(event, Err(Error::Validation(_))),
            "{zone}: {event:?}"
        );
    }
    let last = DateTime::<Utc>::MAX_UTC.date_naive();
    assert_eq!(bucket(max, UTC).unwrap(), [month(last.year(), 12)]);
}
