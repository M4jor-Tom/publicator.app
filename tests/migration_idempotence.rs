mod common;
use common::{Fixture, id};
use publicator::{config::Config, migration::migrate, model::Publications};
use serde_json::{Value, json};
use std::collections::HashMap;

const HISTORY: &str = "https://example.invalid/history";
const LEGACY_DATE: i64 = 1_800_000_000;

fn published(platform: &str) -> Value {
    json!({"platformName": platform, "state": "published_or_scheduled"})
}

fn pending(platform: &str) -> Value {
    json!({"platformName": platform, "state": "unpublished"})
}

/// Adds distinct publication, file and apparition sentinels to a legacy row.
fn extended(n: u64, mut row: Value) -> Value {
    row["uuid"] = json!(id(n));
    row["submissionTimestamp"] = json!(1_700_000_000);
    row["futurePublication"] = json!({"n": n});
    row["files"] = json!([{"basename": format!("{n}.png"), "fileSha512sum": "a".repeat(128),
        "futureFile": n}]);
    let apps = row.get_mut("apparitions").and_then(Value::as_array_mut);
    for app in apps.into_iter().flatten() {
        app["futureApparition"] = json!([n]);
    }
    row
}

/// Historical, mixed-success, marker, modern-draft and omitted-collection rows.
fn legacy_rows() -> Publications {
    let mut all_success = published("deviantart");
    all_success["urlElsePublicationName"] = json!(HISTORY);
    all_success["apparitionTimestampIfDifferentThanSubmission"] = json!(LEGACY_DATE);
    let mut mixed_url = published("deviantart");
    mixed_url["urlElsePublicationName"] = json!(format!("{HISTORY}/2"));
    let mut mixed_name = pending("pixiv");
    mixed_name["urlElsePublicationName"] = json!("Legacy name");
    for app in [&mut mixed_url, &mut mixed_name] {
        app["apparitionTimestampIfDifferentThanSubmission"] = json!(LEGACY_DATE);
    }
    let mut partial = published("deviantart");
    partial["url"] = json!(format!("{HISTORY}/3"));
    let mut marker = pending("deviantart");
    marker["apparitionTimestampIfDifferentThanSubmission"] = json!(LEGACY_DATE);
    vec![
        extended(1, json!({"apparitions": [all_success]})),
        extended(
            2,
            json!({"description": "d", "apparitions": [mixed_url, mixed_name]}),
        ),
        extended(
            3,
            json!({"title": "Partial", "description": "d",
            "apparitions": [partial, pending("pixiv")]}),
        ),
        extended(
            4,
            json!({"title": "[[PUBLICATOR:TODO_GENERATE]]", "description": "d",
            "apparitions": [marker]}),
        ),
        extended(
            5,
            json!({"state": "PICKED", "title": "", "description": ""}),
        ),
    ]
}

fn assert_sentinels(n: u64, row: &Value) {
    assert_eq!(row["futurePublication"], json!({"n": n}), "{row}");
    assert_eq!(row["files"][0]["futureFile"], n, "{row}");
    let apps = row["apparitions"].as_array().unwrap();
    assert!(
        apps.iter().all(|app| app["futureApparition"] == json!([n])),
        "{row}"
    );
}

#[test]
fn migration_output_is_a_complete_fixed_point_including_extensions_and_urls() {
    let recovered = HashMap::from([(id(1), "Recovered".to_owned())]);
    let config = Config::default();
    let once = migrate(&legacy_rows(), &recovered, &config).unwrap();
    for (n, row) in (1..).zip(&once) {
        assert_sentinels(n, row);
    }
    let urls: Vec<_> = once[..3]
        .iter()
        .map(|row| row["apparitions"][0]["url"].clone())
        .collect();
    assert_eq!(
        urls,
        [
            HISTORY.to_owned(),
            format!("{HISTORY}/2"),
            format!("{HISTORY}/3")
        ]
    );
    assert_eq!(migrate(&once, &recovered, &config).unwrap(), once);
    assert_eq!(
        migrate(&once, &HashMap::new(), &config).unwrap(),
        once,
        "migrated output must not need the recovery map again"
    );
    let f = Fixture::new();
    f.write(json!(once));
    let stored = f.store.read().unwrap();
    assert_eq!(stored, once, "migrated output is Store-valid as written");
    assert_eq!(migrate(&stored, &HashMap::new(), &config).unwrap(), once);
}
