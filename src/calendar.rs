use crate::{
    error::{Error, Result},
    model::{Delivery, Phase, effective_schedule, needs_generation, phase},
};
use chrono::{DateTime, Datelike, NaiveDate, Offset, TimeZone, Utc};
use chrono_tz::Tz;
use serde_json::Value;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Event {
    pub uuid: String,
    /// None is a Calendar-only plan, not a saved delivery.
    pub apparition: Option<Apparition>,
    pub title: String,
    pub timestamp: i64,
    pub basename: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Apparition {
    pub index: usize,
    pub platform: String,
    pub url: Option<String>,
    pub delivery: Delivery,
}

impl Event {
    pub fn url(&self) -> Option<&str> {
        self.apparition.as_ref()?.url.as_deref()
    }
}

/// Project dated plans and saved apparitions into a stable date-ordered event list.
pub fn timeline(rows: &[Value]) -> Result<Vec<Event>> {
    let mut events = Vec::new();
    for row in rows {
        let uuid = row
            .get("uuid")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_owned();
        let title = if needs_generation(row) {
            "AI metadata pending"
        } else {
            row.get("title").and_then(Value::as_str).unwrap_or_default()
        };
        let basename = row
            .pointer("/files/0/basename")
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
        let Some(timestamp) = event_timestamp(row, !apparitions.is_empty()) else {
            continue;
        };
        let event = |apparition| Event {
            uuid: uuid.clone(),
            apparition,
            title: title.to_owned(),
            timestamp,
            basename: basename.clone(),
        };
        if apparitions.is_empty() {
            events.push(event(None));
        }
        for (index, app) in apparitions.iter().enumerate() {
            events.push(event(Some(saved_apparition(app, index, &uuid)?)));
        }
    }
    events.sort_by_key(|event| event.timestamp);
    Ok(events)
}

fn event_timestamp(row: &Value, has_platform: bool) -> Option<i64> {
    if !has_platform && phase(row).ok() == Some(Phase::Picked) {
        // Pick saves a submission timestamp even when there is no slot.
        row.get("apparitionTimestampIfDifferentThanSubmission")
            .and_then(Value::as_i64)
    } else {
        effective_schedule(row)
    }
}

fn saved_apparition(app: &Value, index: usize, uuid: &str) -> Result<Apparition> {
    let platform = app
        .get("platformName")
        .and_then(Value::as_str)
        .ok_or_else(|| {
            Error::Validation(format!("{uuid}: apparition platformName must be a string"))
        })?;
    let delivery = match app.get("state").and_then(Value::as_str) {
        Some("unpublished") => Delivery::Unpublished,
        Some("published_or_scheduled") => Delivery::PublishedOrScheduled,
        _ => {
            return Err(Error::Validation(format!(
                "{uuid}: apparition state is invalid"
            )));
        }
    };
    Ok(Apparition {
        index,
        platform: platform.to_owned(),
        url: app.get("url").and_then(Value::as_str).map(str::to_owned),
        delivery,
    })
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
