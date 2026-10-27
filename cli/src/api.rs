//! Authenticated controller access. Errors deliberately omit URLs, headers and bodies.
use anyhow::{anyhow, bail, Result};
use reqwest::header::{HeaderMap, HeaderValue, ACCEPT, AUTHORIZATION};
use serde::de::DeserializeOwned;
use std::{fmt, net::IpAddr, time::Duration};
use url::Url;

const MAX_RESPONSE_BYTES: usize = 8 * 1024 * 1024;
const DEFAULT_TIMEOUT: Duration = Duration::from_secs(5);

#[derive(Clone)]
pub struct Api {
    client: reqwest::Client,
    base: Url,
}

impl fmt::Debug for Api {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Api").finish_non_exhaustive()
    }
}

/// A status-only error, safe to inspect without retaining a response body.
#[derive(Debug)]
pub struct HttpStatusError(pub u16);

impl fmt::Display for HttpStatusError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "controller API HTTP {}", self.0)
    }
}

impl std::error::Error for HttpStatusError {}

impl Api {
    pub fn new(url: &str, secret: &str, allow_insecure: bool) -> Result<Self> {
        let base = controller_url(url, allow_insecure)?;
        let mut headers = HeaderMap::new();
        headers.insert(ACCEPT, HeaderValue::from_static("application/json"));
        if !secret.is_empty() {
            let mut value = HeaderValue::from_str(&format!("Bearer {secret}"))
                .map_err(|_| anyhow!("invalid controller authentication header"))?;
            value.set_sensitive(true);
            headers.insert(AUTHORIZATION, value);
        }
        let client = reqwest::Client::builder()
            .no_proxy()
            .redirect(reqwest::redirect::Policy::none())
            .default_headers(headers)
            .timeout(DEFAULT_TIMEOUT)
            .build()
            .map_err(|_| anyhow!("could not initialize controller client"))?;
        Ok(Self { client, base })
    }

    pub async fn version(&self) -> Result<serde_json::Value> {
        let value: serde_json::Value = self.get_json(&["version"], &[], DEFAULT_TIMEOUT).await?;
        if value
            .get("version")
            .and_then(serde_json::Value::as_str)
            .is_none_or(str::is_empty)
        {
            bail!("controller /version returned an unsupported response");
        }
        Ok(value)
    }

    /// Path components are encoded independently; never pass an already-encoded path.
    /// Query values are encoded by `url`, and the timeout includes reading the body.
    pub async fn get_json<T: DeserializeOwned>(
        &self,
        segments: &[&str],
        query: &[(&str, &str)],
        timeout: Duration,
    ) -> Result<T> {
        if timeout.is_zero() {
            bail!("controller request timeout must be positive");
        }
        let url = request_url(&self.base, segments, query)?;
        let mut response = self
            .client
            .get(url)
            .timeout(timeout)
            .send()
            .await
            .map_err(|_| anyhow!("controller API connection failed or timed out"))?;
        if !response.status().is_success() {
            return Err(HttpStatusError(response.status().as_u16()).into());
        }
        if response
            .content_length()
            .is_some_and(|n| n > MAX_RESPONSE_BYTES as u64)
        {
            bail!("controller API response is too large");
        }
        let mut body = Vec::new();
        while let Some(chunk) = response
            .chunk()
            .await
            .map_err(|_| anyhow!("controller API response failed or timed out"))?
        {
            if chunk.len() > MAX_RESPONSE_BYTES - body.len() {
                bail!("controller API response is too large");
            }
            body.extend_from_slice(&chunk);
        }
        serde_json::from_slice(&body).map_err(|_| anyhow!("controller API returned invalid JSON"))
    }
}

fn controller_url(input: &str, allow_insecure: bool) -> Result<Url> {
    let input = if input.contains("://") {
        input.to_owned()
    } else {
        format!("http://{input}")
    };
    let url = Url::parse(&input).map_err(|_| anyhow!("invalid controller address"))?;
    if !matches!(url.scheme(), "http" | "https")
        || url.host_str().is_none()
        || !url.username().is_empty()
        || url.password().is_some()
        || url.query().is_some()
        || url.fragment().is_some()
        || !matches!(url.path(), "" | "/")
    {
        bail!("controller must be an HTTP(S) origin without credentials, path, query or fragment");
    }
    let loopback = match url.host() {
        Some(url::Host::Ipv4(ip)) => IpAddr::V4(ip).is_loopback(),
        Some(url::Host::Ipv6(ip)) => IpAddr::V6(ip).is_loopback(),
        Some(url::Host::Domain(host)) => host.eq_ignore_ascii_case("localhost"),
        None => false,
    };
    if url.scheme() == "http" && !loopback && !allow_insecure {
        bail!("remote HTTP controller requires explicit insecure permission; use HTTPS instead");
    }
    Ok(url)
}

fn request_url(base: &Url, segments: &[&str], query: &[(&str, &str)]) -> Result<Url> {
    // url intentionally normalizes dot segments, so reject those rather than
    // silently addressing a different endpoint with the authentication header.
    if segments.iter().any(|s| matches!(*s, "." | "..")) {
        bail!("controller path contains an unsupported dot segment");
    }
    let mut url = base.clone();
    url.path_segments_mut()
        .map_err(|_| anyhow!("invalid controller origin"))?
        .clear()
        .extend(segments.iter().copied());
    if !query.is_empty() {
        url.query_pairs_mut().extend_pairs(query.iter().copied());
    }
    Ok(url)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn validates_origins_without_echoing_input() {
        for input in [
            "http://user:private@localhost",
            "http://localhost/path",
            "http://localhost/?private",
            "http://localhost/#private",
            "ftp://localhost",
            "http://192.0.2.1",
            "http://[::]",
            "not a host",
        ] {
            let error = controller_url(input, false).unwrap_err();
            assert!(!format!("{error:?}").contains("private"));
        }
        for input in [
            "localhost:9090",
            "http://127.0.0.2:9090",
            "http://[::1]:9090",
            "https://example.invalid",
        ] {
            assert!(controller_url(input, false).is_ok());
        }
        assert!(controller_url("http://192.0.2.1", true).is_ok());
    }

    #[test]
    fn encodes_components_and_query_without_changing_origin() {
        let base = controller_url("http://localhost:9090", false).unwrap();
        let url = request_url(
            &base,
            &["providers", "proxies", "源/A", "日本 /?#%", "healthcheck"],
            &[
                ("url", "https://example.invalid/a?b=c&d=1"),
                ("expected", "200-299"),
            ],
        )
        .unwrap();
        assert_eq!(
            url.path(),
            "/providers/proxies/%E6%BA%90%2FA/%E6%97%A5%E6%9C%AC%20%2F%3F%23%25/healthcheck"
        );
        assert_eq!(
            url.query_pairs().next().unwrap().1,
            "https://example.invalid/a?b=c&d=1"
        );
        assert_eq!(url.origin(), base.origin());
        assert!(request_url(&base, &[".."], &[]).is_err());
    }

    #[test]
    fn status_error_is_safe_and_inspectable() {
        let error: anyhow::Error = HttpStatusError(404).into();
        assert_eq!(error.downcast_ref::<HttpStatusError>().unwrap().0, 404);
        assert_eq!(error.to_string(), "controller API HTTP 404");
    }
}
