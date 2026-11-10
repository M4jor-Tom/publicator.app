use crate::{
    error::{Error, Result},
    model::{Delivery, GENERATE_MARKER, effective_schedule},
};
use chrono::{DateTime, Datelike, NaiveDate, Offset, TimeZone, Utc};
use chrono_tz::Tz;
use serde_json::Value;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Event {
    pub uuid: String,
    pub apparition_index: usize,
    pub platform: String,
    pub title: String,
    pub timestamp: i64,
    pub url: Option<String>,
    pub basename: Option<String>,
    pub delivery: Delivery,
}

/// Project every publication apparition into a stable date-ordered event list.
pub fn timeline(rows: &[Value]) -> Result<Vec<Event>> {
    let mut events = Vec::new();
    for row in rows {
        let uuid = row
            .get("uuid")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_owned();
        let title = row.get("title").and_then(Value::as_str).unwrap_or_default();
        let description = row
            .get("description")
            .and_then(Value::as_str)
            .unwrap_or_default();
        let title = if title.contains(GENERATE_MARKER) || description.contains(GENERATE_MARKER) {
            "AI metadata pending"
        } else {
            title
        };
        let timestamp = effective_schedule(row).unwrap_or(0);
        let basename = row
            .get("files")
            .and_then(Value::as_array)
            .and_then(|items| items.first())
            .and_then(|file| file.get("basename"))
            .and_then(Value::as_str)
            .map(str::to_owned);
        let apparitions = match row.get("apparitions") {
            None => &[][..],
            Some(Value::Array(items)) => items.as_slice(),
            Some(_) => {
                return Err(Error::Validation(format!(
                    "{uuid}: apparitions must be an array"
                )));
            }
        };
        for (apparition_index, app) in apparitions.iter().enumerate() {
            let platform = app
                .get("platformName")
                .and_then(Value::as_str)
                .ok_or_else(|| {
                    Error::Validation(format!("{uuid}: apparition platformName must be a string"))
                })?;
            let delivery = apparition_delivery(app, &uuid)?;
            events.push(Event {
                uuid: uuid.clone(),
                apparition_index,
                platform: platform.to_owned(),
                title: title.to_owned(),
                timestamp,
                url: app.get("url").and_then(Value::as_str).map(str::to_owned),
                basename: basename.clone(),
                delivery,
            });
        }
    }
    events.sort_by_key(|event| event.timestamp);
    Ok(events)
}

fn apparition_delivery(app: &Value, uuid: &str) -> Result<Delivery> {
    match app.get("state").and_then(Value::as_str) {
        Some("unpublished") => Ok(Delivery::Unpublished),
        Some("published_or_scheduled") => Ok(Delivery::PublishedOrScheduled),
        _ => Err(Error::Validation(format!(
            "{uuid}: apparition state is invalid"
        ))),
    }
}

/// First day of the zone-local month, or None when the instant or its local date is outside
/// chrono's range (`with_timezone(..).date_naive()` would panic at the range edges).
fn local_month(timestamp: i64, zone: Tz) -> Option<NaiveDate> {
    let utc = DateTime::<Utc>::from_timestamp(timestamp, 0)?.naive_utc();
    let offset = zone.offset_from_utc_datetime(&utc).fix();
    utc.checked_add_offset(offset)?.date().with_day(1)
}

/// Inclusive month starts from the earliest event through the later of the last event and now.
pub fn months(events: &[Event], zone: Tz, now: i64) -> Result<Vec<NaiveDate>> {
    let mut first = local_month(now, zone)
        .ok_or_else(|| Error::Validation("calendar timestamp is outside supported range".into()))?;
    let mut last = first;
    for event in events {
        let month = local_month(event.timestamp, zone).ok_or_else(|| {
            Error::Validation("calendar event timestamp is outside supported range".into())
        })?;
        first = first.min(month);
        last = last.max(month);
    }
    let mut output = Vec::new();
    let mut month = first;
    loop {
        output.push(month);
        if month >= last {
            break;
        }
        month = if month.month() == 12 {
            NaiveDate::from_ymd_opt(
                month
                    .year()
                    .checked_add(1)
                    .ok_or_else(|| Error::Validation("calendar month range overflow".into()))?,
                1,
                1,
            )
        } else {
            NaiveDate::from_ymd_opt(month.year(), month.month() + 1, 1)
        }
        .ok_or_else(|| Error::Validation("calendar month range overflow".into()))?;
    }
    Ok(output)
}
