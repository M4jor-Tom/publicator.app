use super::{Generated, MetadataClient, metadata_prompt, parse_metadata, title_description_schema};
use crate::{
    error::{Error, Result},
    images::PreparedImage,
};
use reqwest::{StatusCode, header::RETRY_AFTER};
use serde_json::{Value, json};
use std::{
    collections::HashSet,
    future::{Future, poll_fn},
    pin::pin,
    task::Poll,
    time::{Duration, SystemTime},
};
use tokio::time::{Instant, sleep_until, timeout_at};

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CatalogModel {
    pub id: String,
    pub image_input: bool,
    pub free: bool,
    pub schema_supported: Option<bool>,
}
impl CatalogModel {
    pub fn from_api(value: &Value) -> Result<Self> {
        let id = value
            .get("id")
            .and_then(Value::as_str)
            .filter(|s| !s.is_empty())
            .ok_or_else(|| Error::Protocol("catalogue model missing id".into()))?
            .to_owned();
        let image_input = value
            .pointer("/architecture/input_modalities")
            .and_then(Value::as_array)
            .is_some_and(|xs| xs.iter().any(|v| v.as_str() == Some("image")));
        let pricing = value
            .get("pricing")
            .and_then(Value::as_object)
            .ok_or_else(|| Error::Protocol(format!("catalogue model {id} missing pricing")))?;
        let free = ["prompt", "completion"]
            .iter()
            .all(|key| pricing.get(*key).and_then(price).is_some_and(|n| n == 0.0))
            && pricing.values().all(|v| price(v).is_some_and(|n| n == 0.0));
        let schema_supported = value
            .get("supported_parameters")
            .and_then(Value::as_array)
            .map(|xs| xs.iter().any(|v| v.as_str() == Some("response_format")));
        Ok(Self {
            id,
            image_input,
            free,
            schema_supported,
        })
    }
}
fn price(v: &Value) -> Option<f64> {
    v.as_str()
        .and_then(|s| s.parse().ok())
        .or_else(|| v.as_f64())
}
impl MetadataClient {
    /// The configured key, else a nonempty `OPENROUTER_KEY`; without one no request is sent.
    fn openrouter_key(&self) -> Result<String> {
        self.openrouter_key
            .clone()
            .or_else(|| std::env::var("OPENROUTER_KEY").ok())
            .filter(|key| !key.is_empty())
            .ok_or_else(|| Error::Provider("OPENROUTER_KEY is not set".into()))
    }
    async fn bounded<T>(
        &self,
        request: reqwest::RequestBuilder,
        deadline: Instant,
        secrets: &[&str],
        parse: impl FnOnce(&[u8]) -> Result<T>,
    ) -> Result<T> {
        let request = request.build().map_err(request_error)?;
        loop {
            // GETs and JSON bodies can be cloned without rebuilding the payload or changing modes.
            let (response, dispatch_cooldown) =
                self.dispatch(request.try_clone(), deadline).await?;
            let status = response.status();
            if status == StatusCode::TOO_MANY_REQUESTS {
                let retry_after = response
                    .headers()
                    .get(RETRY_AFTER)
                    .and_then(|v| v.to_str().ok());
                self.openrouter_cooldown
                    .lock()
                    .unwrap()
                    .throttled(retry_after);
                // Drop the response without reading potentially slow or sensitive error content.
                continue;
            }
            let bytes = timeout_at(deadline, response.bytes())
                .await
                .map_err(|_| Error::OpenRouterDeadline)?
                .map_err(request_error)?;
            if !status.is_success() {
                return Err(Error::Provider(format!(
                    "OpenRouter HTTP {}: {}",
                    status.as_u16(),
                    safe_error_detail(&bytes, secrets)
                )));
            }
            let parsed = parse(&bytes)?;
            let mut cooldown = self.openrouter_cooldown.lock().unwrap();
            if cooldown.until == dispatch_cooldown {
                cooldown.headerless = 0;
            }
            return Ok(parsed);
        }
    }
    async fn dispatch(
        &self,
        request: Option<reqwest::Request>,
        deadline: Instant,
    ) -> Result<(reqwest::Response, Option<Instant>)> {
        let request = request
            .ok_or_else(|| Error::Protocol("OpenRouter request cannot be retried".into()))?;
        let mut send = pin!(async { self.http.execute(request).await });
        let mut wake = pin!(sleep_until(deadline));
        let mut admitted = None;
        let dispatch = poll_fn(|cx| {
            if admitted.is_some() {
                return send.as_mut().poll(cx);
            }
            let cooldown = self.openrouter_cooldown.lock().unwrap();
            loop {
                let now = Instant::now();
                if now >= deadline {
                    return Poll::Pending; // The outer timeout owns the deadline.
                }
                if let Some(until) = cooldown.until.filter(|until| now < *until) {
                    wake.as_mut().reset(until.min(deadline));
                    if wake.as_mut().poll(cx).is_pending() {
                        return Poll::Pending;
                    }
                    continue;
                }
                // Start the lazy HTTP future atomically with admission. Later polls never lock.
                admitted = Some(cooldown.until);
                return send.as_mut().poll(cx);
            }
        });
        let response = timeout_at(deadline, dispatch)
            .await
            .map_err(|_| Error::OpenRouterDeadline)?
            .map_err(request_error)?;
        Ok((
            response,
            admitted.expect("response requires an admitted request"),
        ))
    }
    pub async fn catalogue(&self, deadline: Instant) -> Result<Vec<CatalogModel>> {
        if let Some((at, models)) = self.catalogue.lock().unwrap().as_ref()
            && at.elapsed() < Duration::from_secs(300)
        {
            return Ok(models.clone());
        }
        let url = format!("{}/models", self.endpoints.openrouter.trim_end_matches('/'));
        let key = self.openrouter_key()?;
        let req = self.http.get(url).bearer_auth(&key);
        let models = self
            .bounded(req, deadline, &[key.as_str()], parse_catalogue)
            .await?;
        *self.catalogue.lock().unwrap() = Some((std::time::Instant::now(), models.clone()));
        Ok(models)
    }
    pub async fn openrouter_attempt(
        &self,
        image: &PreparedImage,
        model: &str,
        schema: bool,
        deadline: Instant,
    ) -> Result<Generated> {
        let key = self.openrouter_key()?;
        let model = model.strip_prefix("openrouter/").unwrap_or(model);
        let rejected = self.rejected_schemas.lock().unwrap().contains(model);
        let use_schema = schema && !rejected;
        let data_url = super::image_data_url(image)?;
        let body = completion_body(model, &data_url, use_schema);
        let url = format!(
            "{}/chat/completions",
            self.endpoints.openrouter.trim_end_matches('/')
        );
        let send = |body: Value| self.http.post(&url).bearer_auth(&key).json(&body);
        let parse = |bytes: &[u8]| parse_completion(bytes, model);
        let secrets = [key.as_str(), data_url.as_str()];
        match self.bounded(send(body), deadline, &secrets, parse).await {
            Err(Error::Provider(detail)) if use_schema && unsupported_schema(&detail) => {
                self.rejected_schemas
                    .lock()
                    .unwrap()
                    .insert(model.to_owned());
                let retry = completion_body(model, &data_url, false);
                self.bounded(send(retry), deadline, &secrets, parse).await
            }
            result => result,
        }
    }
    pub async fn openrouter_generate(
        &self,
        image: &PreparedImage,
        request: &super::GenerationRequest,
        preferred: Option<&str>,
    ) -> Result<Generated> {
        let deadline = Instant::now() + request.timeout;
        self.openrouter_generate_until(image, request, preferred, deadline)
            .await
    }
    pub(crate) async fn openrouter_generate_until(
        &self,
        image: &PreparedImage,
        request: &super::GenerationRequest,
        preferred: Option<&str>,
        deadline: Instant,
    ) -> Result<Generated> {
        let selected = preferred.unwrap_or(&request.model.openrouter_name);
        let selected = selected.strip_prefix("openrouter/").unwrap_or(selected);
        let generated = match self
            .openrouter_attempt(image, selected, true, deadline)
            .await
        {
            Ok(generated) => generated,
            Err(error @ Error::OpenRouterDeadline) => return Err(error),
            Err(original) => {
                self.openrouter_fallback(image, selected, deadline, original)
                    .await?
            }
        };
        eprintln!("OpenRouter metadata generated by {}", generated.model_used);
        Ok(generated)
    }
    async fn openrouter_fallback(
        &self,
        image: &PreparedImage,
        selected: &str,
        deadline: Instant,
        original: Error,
    ) -> Result<Generated> {
        let models = match self.catalogue(deadline).await {
            Ok(models) => models,
            Err(error @ Error::OpenRouterDeadline) => return Err(error),
            Err(_) => return Err(original),
        };
        let mut last = original;
        for candidate in fallback_models(models, selected) {
            if Instant::now() >= deadline {
                return Err(Error::OpenRouterDeadline);
            }
            if candidate.schema_supported == Some(false) {
                self.rejected_schemas
                    .lock()
                    .unwrap()
                    .insert(candidate.id.clone());
            }
            match self
                .openrouter_attempt(
                    image,
                    &candidate.id,
                    candidate.schema_supported.unwrap_or(true),
                    deadline,
                )
                .await
            {
                Ok(generated) => return Ok(generated),
                Err(error @ Error::OpenRouterDeadline) => return Err(error),
                Err(error) => last = error,
            }
        }
        Err(last)
    }
}
#[derive(Default)]
pub(super) struct Cooldown {
    // Keep the last expiry even after it passes: it identifies work older than a later 429.
    until: Option<Instant>,
    headerless: u32,
}

impl Cooldown {
    fn throttled(&mut self, retry_after: Option<&str>) {
        let now = Instant::now();
        let until = retry_after
            .and_then(|value| retry_delay(value, SystemTime::now()))
            .and_then(|delay| now.checked_add(delay))
            .unwrap_or_else(|| {
                let delay = Duration::from_secs(1 << self.headerless);
                self.headerless = (self.headerless + 1).min(3);
                now + delay
            });
        self.until = self.until.max(Some(until));
    }
}

fn retry_delay(value: &str, now: SystemTime) -> Option<Duration> {
    let value = value.trim();
    let delay = if value.bytes().all(|b| b.is_ascii_digit()) {
        Duration::from_secs(value.parse().ok()?)
    } else {
        httpdate::parse_http_date(value)
            .ok()?
            .duration_since(now)
            .ok()?
    };
    (!delay.is_zero()).then_some(delay)
}

fn request_error(error: reqwest::Error) -> Error {
    if error.is_timeout() {
        Error::OpenRouterDeadline
    } else {
        Error::Provider(error.to_string())
    }
}

fn parse_catalogue(bytes: &[u8]) -> Result<Vec<CatalogModel>> {
    let v: Value = serde_json::from_slice(bytes)?;
    let rows = v
        .get("data")
        .and_then(Value::as_array)
        .ok_or_else(|| Error::Protocol("OpenRouter catalogue missing data".into()))?;
    rows.iter().map(CatalogModel::from_api).collect()
}

fn completion_body(model: &str, data_url: &str, use_schema: bool) -> Value {
    let mut prompt = metadata_prompt().to_owned();
    if !use_schema {
        prompt.push_str(" Return JSON matching this shape: ");
        prompt.push_str(&title_description_schema().to_string());
    }
    let content =
        json!([{"type":"text","text":prompt},{"type":"image_url","image_url":{"url":data_url}}]);
    let mut body = json!({"model":model,"messages":[{"role":"user","content":content}]});
    if use_schema {
        body["response_format"] = json!({"type":"json_schema","json_schema":{"name":"title_description","strict":true,"schema":title_description_schema()}});
    }
    body
}
fn parse_completion(bytes: &[u8], model: &str) -> Result<Generated> {
    let v: Value = serde_json::from_slice(bytes)?;
    let content = v
        .pointer("/choices/0/message/content")
        .and_then(Value::as_str)
        .ok_or_else(|| Error::Protocol("OpenRouter response missing message content".into()))?;
    Ok(Generated {
        metadata: parse_metadata(content)?,
        model_used: model.to_owned(),
    })
}
fn fallback_models(models: Vec<CatalogModel>, selected: &str) -> Vec<CatalogModel> {
    let original_vendor = selected.split('/').next().unwrap_or("");
    let mut other = Vec::new();
    let mut sibling = Vec::new();
    let mut seen = HashSet::from([selected.to_owned()]);
    for model in models
        .into_iter()
        .filter(|model| model.image_input && model.free)
    {
        if seen.insert(model.id.clone()) {
            if model.id.split('/').next().unwrap_or("") == original_vendor {
                sibling.push(model);
            } else {
                other.push(model);
            }
        }
    }
    other.extend(sibling);
    other
}
fn safe_error_detail(body: &[u8], secrets: &[&str]) -> String {
    let value: Value = serde_json::from_slice(body).unwrap_or(Value::Null);
    let detail = value
        .pointer("/error/message")
        .or_else(|| value.get("message"))
        .and_then(Value::as_str)
        .map(str::to_owned)
        .unwrap_or_else(|| String::from_utf8_lossy(body).into_owned());
    let mut detail = detail;
    for secret in secrets.iter().filter(|secret| !secret.is_empty()) {
        detail = detail.replace(secret, "[REDACTED]");
    }
    detail = redact_image_data_uris(&detail);
    let mut chars = detail.chars();
    let bounded: String = chars.by_ref().take(512).collect();
    if chars.next().is_some() {
        format!("{bounded}…")
    } else {
        bounded
    }
}

fn redact_image_data_uris(input: &str) -> String {
    let mut output = String::with_capacity(input.len());
    let mut rest = input;
    while let Some(start) = rest.find("data:image/") {
        output.push_str(&rest[..start]);
        let uri = &rest[start..];
        let end = uri
            .find([' ', '\t', '\r', '\n', '"', '\'', '<', '>'])
            .unwrap_or(uri.len());
        output.push_str("[REDACTED]");
        rest = &uri[end..];
    }
    output.push_str(rest);
    output
}

fn unsupported_schema(message: &str) -> bool {
    let m = message.to_ascii_lowercase();
    m.contains("response_format")
        && (m.contains("not support")
            || m.contains("unsupported")
            || m.contains("not allowed")
            || m.contains("does not support"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn retry_after_accepts_seconds_and_http_dates_but_not_zero_past_or_invalid_values() {
        let now = httpdate::parse_http_date("Sun, 06 Nov 1994 08:49:07 GMT").unwrap();
        for value in [
            " 30 ",
            "Sun, 06 Nov 1994 08:49:37 GMT",
            "Sunday, 06-Nov-94 08:49:37 GMT",
            "Sun Nov  6 08:49:37 1994",
        ] {
            assert_eq!(
                retry_delay(value, now),
                Some(Duration::from_secs(30)),
                "{value}"
            );
        }
        for value in [
            "",
            "0",
            "-1",
            "+1",
            "1.5",
            "bogus",
            "18446744073709551616",
            "Sun, 06 Nov 1994 08:49:07 GMT",
            "Sun, 06 Nov 1994 08:49:06 GMT",
        ] {
            assert_eq!(retry_delay(value, now), None, "{value}");
        }
    }
}
