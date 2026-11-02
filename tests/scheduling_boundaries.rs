mod common;
use chrono_tz::Europe::Paris;
use common::epoch;
use publicator::{
    config::Config,
    model::Publications,
    scheduling::{Profile, profiles, repack, slots},
};
use serde_json::{Value, json};

// Fixed epochs in the default zone (Europe/Paris); 2026-01-01 is a Thursday.
const NOW: &str = "2026-01-01T12:00:00Z";
const TWO_PROFILES: &str = "[[profiles]]\nname='tue'\nday='tuesday'\nhour=20\nper_slot=1\n\
    [[profiles]]\nname='fri'\nday='friday'\nhour=18\nper_slot=2";

fn config(text: &str) -> Config {
    Config {
        schedule: toml::from_str(text).unwrap(),
        ..Config::default()
    }
}

fn first_profile(cfg: &Config) -> Profile {
    profiles(cfg).unwrap().remove(0)
}

fn automatic(profile: Option<&str>) -> Value {
    let mut row = json!({"state": "PICKED", "scheduleIsManual": false});
    if let Some(name) = profile {
        row["scheduleProfile"] = json!(name);
    }
    row
}

/// Capacity that repacking must keep and never move: successful history, approved
/// dates, manual pins and an automatic row whose profile was removed.
fn reservation(kind: usize, ts: i64) -> Value {
    match kind % 4 {
        0 => json!({"state": "APPEARED", "submissionTimestamp": ts,
            "apparitions": [{"platformName": "deviantart", "state": "published_or_scheduled"}]}),
        1 => json!({"state": "SCHEDULED", "apparitionTimestampIfDifferentThanSubmission": ts}),
        2 => json!({"state": "PICKED", "scheduleIsManual": true,
            "apparitionTimestampIfDifferentThanSubmission": ts}),
        _ => json!({"state": "PICKED", "scheduleProfile": "removed", "scheduleIsManual": false,
            "apparitionTimestampIfDifferentThanSubmission": ts}),
    }
}

fn dates(rows: &[Value]) -> Vec<Option<i64>> {
    rows.iter()
        .map(|row| {
            row.get("apparitionTimestampIfDifferentThanSubmission")
                .and_then(Value::as_i64)
        })
        .collect()
}

#[test]
fn now_exactly_at_a_slot_allocates_the_next_week_and_keeps_the_pin() {
    let cfg = config(TWO_PROFILES);
    let tuesday = first_profile(&cfg);
    let slot = epoch("2026-01-06T19:00:00Z"); // Tuesday 20:00 CET
    let next = epoch("2026-01-13T19:00:00Z");
    assert_eq!(slots(&tuesday, Paris, slot - 1).unwrap()[0], slot);
    assert_eq!(slots(&tuesday, Paris, slot).unwrap()[0], next);
    let mut due = automatic(Some("tue"));
    due["apparitionTimestampIfDifferentThanSubmission"] = json!(slot);
    let mut rows: Publications = vec![reservation(2, slot), due];
    repack(&mut rows, &cfg, slot).unwrap();
    assert_eq!(dates(&rows), [Some(slot), Some(next)]);
    assert_eq!(rows[0], reservation(2, slot));
}

#[test]
fn weekly_slots_in_dst_gap_and_fold_hours_resolve_like_zoneinfo_fold_zero() {
    let sunday = first_profile(&config("day='sunday'\nhour=2\nper_slot=1"));
    let gap = epoch("2026-03-29T01:00:00Z"); // 02:00 CET does not exist; pre-transition offset.
    let fold = epoch("2026-10-25T00:00:00Z"); // 02:00 occurs twice; the earlier CEST instant.
    let out = slots(&sunday, Paris, epoch("2026-03-22T12:00:00Z")).unwrap();
    assert_eq!(out[..2], [gap, epoch("2026-04-05T00:00:00Z")]);
    assert_eq!(out[30..32], [fold, epoch("2026-11-01T01:00:00Z")]);
    // now exactly at a resolved gap slot, or inside the repeated hour after the fold slot,
    // has passed it; one second before the fold slot has not.
    assert_eq!(slots(&sunday, Paris, gap).unwrap()[0], out[1]);
    assert_eq!(slots(&sunday, Paris, fold - 1).unwrap()[0], fold);
    assert_eq!(slots(&sunday, Paris, fold + 1800).unwrap()[0], out[31]);
}

#[test]
fn profiles_consume_rows_in_persisted_order_around_preserved_reservations() {
    let cfg = config(TWO_PROFILES);
    let tue = |week: i64| epoch("2026-01-06T19:00:00Z") + week * 7 * 86_400;
    let fri = |week: i64| epoch("2026-01-02T17:00:00Z") + week * 7 * 86_400;
    let reserved = vec![
        reservation(0, tue(0)),
        reservation(2, fri(0)),
        reservation(3, tue(1)),
    ];
    let mut rows = reserved.clone();
    rows.extend([
        automatic(Some("fri")),
        automatic(None),
        automatic(Some("fri")),
        automatic(Some("tue")),
        automatic(Some("fri")),
    ]);
    repack(&mut rows, &cfg, epoch(NOW)).unwrap();
    assert_eq!(rows[..3], reserved);
    let expected = [fri(0), tue(2), fri(1), tue(3), fri(1)];
    assert_eq!(dates(&rows[3..]), expected.map(Some));
    assert_eq!(
        rows[4]["scheduleProfile"], "tue",
        "fallback is the first profile"
    );
}

#[test]
fn exhausting_all_52_slots_allocates_the_last_unit_and_clears_the_next() {
    let cfg = config("day='tuesday'\nhour=20\nper_slot=2");
    let horizon = slots(&first_profile(&cfg), Paris, epoch(NOW)).unwrap();
    assert_eq!(horizon.len(), 52);
    let last = horizon[51];
    let mut units: Vec<i64> = horizon.iter().flat_map(|ts| [*ts, *ts]).collect();
    units.pop(); // Leave exactly one unit free, in the last slot.
    let reserved: Publications = units
        .into_iter()
        .enumerate()
        .map(|(kind, ts)| reservation(kind, ts))
        .collect();
    let mut stale = automatic(None);
    stale["apparitionTimestampIfDifferentThanSubmission"] = json!(1);
    let mut rows = reserved.clone();
    rows.extend([automatic(None), stale]);
    repack(&mut rows, &cfg, epoch(NOW)).unwrap();
    assert_eq!(rows[..reserved.len()], reserved);
    assert_eq!(dates(&rows[reserved.len()..]), [Some(last), None]);
}
