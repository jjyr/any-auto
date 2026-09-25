//! TypeSafe System One transport, with a total deadline and bounded responses.
use crate::audit;
use anyhow::{Result, anyhow, ensure};
use reqwest::{Client, Url};
use serde_json::{Value, json};
use std::time::{Duration, SystemTime, UNIX_EPOCH};
use tokio::time::Instant;

const BUDGET: Duration = Duration::from_secs(20);
const MAX_BODY: usize = 4 * 1024 * 1024;

pub(crate) fn endpoint(base: &str) -> Result<Url> {
    let mut url = Url::parse(base).map_err(|_| anyhow!("Invalid Jev base URL"))?;
    ensure!(
        url.username().is_empty()
            && url.password().is_none()
            && url.query().is_none()
            && url.fragment().is_none(),
        "Jev base URL must not contain credentials, query, or fragment"
    );
    ensure!(
        matches!(url.scheme(), "http" | "https"),
        "Jev base URL must use HTTP or HTTPS"
    );
    ensure!(url.host_str().is_some(), "Jev base URL requires a host");
    url.set_path(&format!("{}/systemone", url.path().trim_end_matches('/')));
    Ok(url)
}

pub struct JevClient {
    http: Client,
    url: Url,
    pub(crate) retries: u32,
    pub(crate) collect_usage: bool,
}
struct Failure {
    message: String,
    retry: bool,
    delay: Option<Duration>,
}
impl Failure {
    fn network(error: reqwest::Error) -> Self {
        // reqwest errors can include the URL; never forward their text.
        Self {
            message: "Jev HTTP connection or read failed".into(),
            retry: !error.is_builder(),
            delay: None,
        }
    }
}
impl JevClient {
    pub fn new(base: &str) -> Result<Self> {
        // Resolve proxies from this caller's scoped context, not the first daemon starter.
        let variable = |lower: &str, upper: &str| {
            crate::context::var(lower)
                .ok()
                .or_else(|| crate::context::var(upper).ok())
                .filter(|v| !v.is_empty())
        };
        let excluded =
            variable("no_proxy", "NO_PROXY").and_then(|v| reqwest::NoProxy::from_string(&v));
        let mut builder = Client::builder()
            .no_proxy()
            .redirect(reqwest::redirect::Policy::none())
            .connect_timeout(Duration::from_secs(5));
        for (lower, upper) in [
            ("http_proxy", "HTTP_PROXY"),
            ("https_proxy", "HTTPS_PROXY"),
            ("all_proxy", "ALL_PROXY"),
        ] {
            if let Some(value) = variable(lower, upper) {
                let proxy = match lower {
                    "http_proxy" => reqwest::Proxy::http(value),
                    "https_proxy" => reqwest::Proxy::https(value),
                    _ => reqwest::Proxy::all(value),
                }
                .map_err(|_| anyhow!("Invalid Jev HTTP proxy configuration"))?;
                builder = builder.proxy(proxy.no_proxy(excluded.clone()));
            }
        }
        Ok(Self {
            url: endpoint(base)?,
            retries: 2,
            collect_usage: false,
            http: builder
                .build()
                .map_err(|_| anyhow!("Cannot initialize Jev HTTP client"))?,
        })
    }

    async fn attempt(
        &self,
        key: &str,
        body: &Value,
        id: &str,
        attempt: u32,
    ) -> std::result::Result<Value, Failure> {
        let mut response = self
            .http
            .post(self.url.clone())
            .bearer_auth(key)
            .json(body)
            .send()
            .await
            .map_err(Failure::network)?;
        let status = response.status();
        if !status.is_success() {
            let delay = response
                .headers()
                .get("retry-after")
                .and_then(|h| h.to_str().ok())
                .and_then(retry_after);
            if self.collect_usage {
                // Keep error bodies private; retain only numeric usage for evaluation.
                let mut bytes = Vec::new();
                while let Ok(Some(chunk)) = response.chunk().await {
                    if bytes.len() + chunk.len() > MAX_BODY {
                        break;
                    }
                    bytes.extend_from_slice(&chunk);
                }
                self.record_usage(
                    id,
                    attempt,
                    &serde_json::from_slice(&bytes).unwrap_or(Value::Null),
                );
            }
            return Err(Failure {
                message: format!("Jev request failed with HTTP {}", status.as_u16()),
                retry: matches!(status.as_u16(), 408 | 429 | 500..=599),
                delay,
            });
        }
        let mut bytes = Vec::new();
        while let Some(chunk) = response.chunk().await.map_err(Failure::network)? {
            if bytes.len() + chunk.len() > MAX_BODY {
                return Err(Failure {
                    message: "Jev response exceeds 4 MiB".into(),
                    retry: false,
                    delay: None,
                });
            }
            bytes.extend_from_slice(&chunk);
        }
        let value = serde_json::from_slice(&bytes).map_err(|_| Failure {
            message: "Invalid Jev response JSON".into(),
            retry: false,
            delay: None,
        })?;
        if self.collect_usage {
            self.record_usage(id, attempt, &value);
        }
        Ok(value)
    }
    fn record_usage(&self, id: &str, attempt: u32, value: &Value) {
        audit::record(
            id,
            "jev_attempt_usage",
            json!({"attempt":attempt,
            "usage_delta":{"input_tokens":value["usage"]["input_tokens"].as_u64(),
                "output_tokens":value["usage"]["output_tokens"].as_u64()}}),
        );
    }
    pub async fn evaluate(&self, key: &str, body: &Value, id: &str) -> Result<Value> {
        let deadline = Instant::now() + BUDGET;
        tokio::time::timeout_at(deadline, async {
            for attempt in 0..=self.retries {
                audit::record(id, "backend_request", json!({"provider":"jev", "attempt":attempt + 1}));
                let started = Instant::now();
                match self.attempt(key, body, id, attempt + 1).await {
                    Ok(value) => return Ok(value),
                    Err(failure) => {
                        if !failure.retry || attempt == self.retries { return Err(anyhow!(failure.message)); }
                        let jitter = SystemTime::now().duration_since(UNIX_EPOCH).unwrap_or_default().subsec_millis() % 126;
                        let delay = failure.delay.unwrap_or_else(|| Duration::from_millis((500u64 << attempt) + u64::from(jitter)));
                        if delay >= deadline.saturating_duration_since(Instant::now()) { return Err(anyhow!("Jev retry delay exceeds remaining review budget")); }
                        audit::record(id, "backend_error", json!({"provider":"jev","attempt":attempt + 1,
                            "error":failure.message,"retrying":true,"duration_ms":started.elapsed().as_millis()}));
                        tokio::time::sleep(delay).await;
                    }
                }
            }
            unreachable!()
        }).await.map_err(|_| anyhow!("Jev review deadline exceeded"))?
    }
}
fn retry_after(value: &str) -> Option<Duration> {
    if let Ok(seconds) = value.parse::<u64>() {
        return Some(Duration::from_secs(seconds));
    }
    let date = chrono::DateTime::parse_from_rfc2822(value).ok()?;
    Some(
        (date.with_timezone(&chrono::Utc) - chrono::Utc::now())
            .to_std()
            .unwrap_or_default(),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn urls_and_retry_headers() {
        assert_eq!(
            endpoint("https://gateway.example/typesafe/v1/")
                .unwrap()
                .as_str(),
            "https://gateway.example/typesafe/v1/systemone"
        );
        for url in [
            "ftp://example.com/v1",
            "https://secret@example.com/v1",
            "https://example.com/?key=secret",
            "https://example.com/#fragment",
        ] {
            assert!(endpoint(url).is_err());
        }
        for base in [
            "http://example.com/v1",
            "http://server.tailnet.ts.net:8000/v1",
            "http://100.64.0.1:8000/v1",
            "http://[::1]:8000/v1",
        ] {
            assert_eq!(
                endpoint(base).unwrap().as_str(),
                format!("{base}/systemone")
            );
        }
        assert_eq!(retry_after("123"), Some(Duration::from_secs(123)));
        assert!(retry_after("Wed, 21 Oct 2015 07:28:00 GMT").is_some());
    }
}
