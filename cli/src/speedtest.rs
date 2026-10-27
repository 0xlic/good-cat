//! Leaf-only speed tests. Dropping `run` aborts its bounded set of requests.
use crate::api::{Api, HttpStatusError};
use anyhow::{anyhow, bail, Result};
use serde::Serialize;
use serde_json::Value;
use std::{
    collections::HashSet,
    time::{Duration, SystemTime, UNIX_EPOCH},
};
use tokio::task::JoinSet;

pub const CLAUDE_IP_WARNING: &str = "Claude 频繁切换出口 IP，尤其跨国家/地区切换，可能触发账号风控，存在访问受限或封号风险。建议先检测出口 IP 信誉/纯净度，再仅保留一个国家并尽量固定可信节点；低延迟、单一国家或第三方评分均不能保证账号安全。本工具未检测 IP 信誉。";

pub const DEFAULT_URLS: [&str; 3] = [
    "https://chatgpt.com/cdn-cgi/trace",
    "https://claude.ai/cdn-cgi/trace",
    "https://www.gstatic.com/generate_204",
];

pub struct SpeedOptions {
    pub urls: Vec<String>,
    pub threshold_ms: u64,
    pub timeout_ms: u64,
    pub concurrency: usize,
    pub rounds: usize,
}

impl Default for SpeedOptions {
    fn default() -> Self {
        Self {
            urls: DEFAULT_URLS.iter().map(|url| (*url).to_owned()).collect(),
            threshold_ms: 600,
            timeout_ms: 5000,
            concurrency: 8,
            rounds: 3,
        }
    }
}

#[derive(Serialize)]
pub struct SpeedReport {
    pub created_at: String,
    pub threshold_ms: u64,
    pub order: &'static str,
    pub rounds: usize,
    pub aggregation: &'static str,
    pub reports: Vec<UrlReport>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub warnings: Vec<String>,
}

#[derive(Serialize)]
pub struct UrlReport {
    pub url: String,
    pub tested: usize,
    pub failed: usize,
    pub eligible: Vec<SpeedResult>,
    pub results: Vec<SpeedResult>,
}

#[derive(Clone, Serialize)]
pub struct SpeedResult {
    pub name: String,
    pub provider: Option<String>,
    pub url: String,
    pub delay_ms: Option<f64>,
    pub error: Option<String>,
    pub samples: Vec<RoundSample>,
}

#[derive(Clone, Serialize)]
pub struct RoundSample {
    pub round: usize,
    pub delay_ms: Option<f64>,
    pub error: Option<String>,
}

#[derive(Clone)]
struct Node {
    name: String,
    provider: Option<String>,
}

pub async fn run(api: &Api, options: &SpeedOptions) -> Result<SpeedReport> {
    let urls = validated_urls(options)?;
    let (nodes, mut warnings) = discover_nodes(api).await?;
    if tests_claude(&urls) {
        warnings.push(CLAUDE_IP_WARNING.to_owned());
    }
    if nodes.is_empty() {
        bail!("controller has no loaded testable nodes");
    }
    let mut reports = Vec::with_capacity(urls.len());
    // Only one URL's work is scheduled at a time, with at most concurrency
    // tasks alive. JoinSet aborts those tasks when the parent drops this future.
    for url in urls {
        let mut results: Vec<_> = nodes
            .iter()
            .map(|node| SpeedResult {
                name: node.name.clone(),
                provider: node.provider.clone(),
                url: url.clone(),
                delay_ms: None,
                error: None,
                samples: Vec::with_capacity(options.rounds),
            })
            .collect();
        for round in 1..=options.rounds {
            let mut pending = nodes.iter().enumerate();
            let mut tasks = JoinSet::new();
            for (index, node) in pending.by_ref().take(options.concurrency) {
                spawn_measurement(
                    &mut tasks,
                    api,
                    node,
                    &url,
                    options.timeout_ms,
                    index,
                    round,
                );
            }
            while let Some(result) = tasks.join_next().await {
                let (index, sample) = result.map_err(|_| anyhow!("speed test worker failed"))?;
                results[index].samples.push(sample);
                if let Some((index, node)) = pending.next() {
                    spawn_measurement(
                        &mut tasks,
                        api,
                        node,
                        &url,
                        options.timeout_ms,
                        index,
                        round,
                    );
                }
            }
        }
        let results = results.into_iter().map(aggregate).collect();
        reports.push(summarize(url, results, options.threshold_ms));
    }
    Ok(SpeedReport {
        created_at: SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs()
            .to_string(),
        threshold_ms: options.threshold_ms,
        order: "delay_desc",
        rounds: options.rounds,
        aggregation: "median_all_success",
        reports,
        warnings,
    })
}

fn tests_claude(urls: &[String]) -> bool {
    urls.iter()
        .filter_map(|value| url::Url::parse(value).ok())
        .any(|url| {
            url.host_str()
                .is_some_and(|host| host == "claude.ai" || host.ends_with(".claude.ai"))
        })
}

fn validated_urls(options: &SpeedOptions) -> Result<Vec<String>> {
    if options.threshold_ms == 0 || options.timeout_ms == 0 {
        bail!("speed test threshold and timeout must be positive");
    }
    if !(1..=64).contains(&options.concurrency) {
        bail!("speed test concurrency must be between 1 and 64");
    }
    if !(1..=20).contains(&options.rounds) {
        bail!("speed test rounds must be between 1 and 20");
    }
    // Leave headroom for the API overhead, without overflowing Duration.
    if options.timeout_ms.checked_add(3000).is_none() {
        bail!("speed test timeout is too large");
    }
    let urls = if options.urls.is_empty() {
        DEFAULT_URLS.iter().map(|url| (*url).to_owned()).collect()
    } else {
        options.urls.clone()
    };
    let mut seen = HashSet::new();
    let mut unique = Vec::new();
    for input in urls {
        let url = url::Url::parse(&input).map_err(|_| anyhow!("invalid speed test URL"))?;
        if !matches!(url.scheme(), "http" | "https")
            || url.host_str().is_none()
            || !url.username().is_empty()
            || url.password().is_some()
        {
            bail!("speed test URLs must use HTTP(S) without credentials");
        }
        if seen.insert(input.clone()) {
            unique.push(input);
        }
    }
    Ok(unique)
}

async fn discover_nodes(api: &Api) -> Result<(Vec<Node>, Vec<String>)> {
    let globals: Value = api
        .get_json(&["proxies"], &[], Duration::from_secs(5))
        .await?;
    let providers: Result<Value> = api
        .get_json(&["providers", "proxies"], &[], Duration::from_secs(5))
        .await;
    let (providers, warnings) = optional_providers(providers)?;
    Ok((nodes_from_payloads(&globals, &providers)?, warnings))
}

fn optional_providers(response: Result<Value>) -> Result<(Value, Vec<String>)> {
    match response {
        Ok(value) => Ok((value, Vec::new())),
        Err(error) if error.downcast_ref::<HttpStatusError>().is_some_and(|status| status.0 == 404) => {
            Ok((serde_json::json!({"providers": {}}), vec![
                "Controller does not support provider discovery (HTTP 404); only global nodes were tested.".to_owned(),
            ]))
        }
        Err(error) => Err(error),
    }
}

fn is_node(proxy: &Value) -> bool {
    let Some(proxy) = proxy.as_object() else {
        return false;
    };
    let Some(kind) = proxy.get("type").and_then(Value::as_str) else {
        return false;
    };
    let kind: String = kind
        .chars()
        .filter(|c| c.is_ascii_alphanumeric())
        .map(|c| c.to_ascii_lowercase())
        .collect();
    !kind.is_empty()
        && !proxy.contains_key("all")
        && !matches!(
            kind.as_str(),
            "direct"
                | "reject"
                | "rejectdrop"
                | "pass"
                | "passrule"
                | "selector"
                | "urltest"
                | "fallback"
                | "loadbalance"
                | "relay"
                | "compatible"
                | "dns"
        )
}

fn identity(name: &str, proxy: &Value) -> String {
    if let Some(id) = proxy
        .get("id")
        .filter(|id| id.as_str().is_some_and(|s| !s.is_empty()) || id.is_number())
    {
        return format!("id:{id}");
    }
    let mut snapshot = proxy.clone();
    if let Some(object) = snapshot.as_object_mut() {
        object.insert("name".to_owned(), Value::String(name.to_owned()));
    }
    // Canonicalize object keys even if serde_json's preserve_order is enabled
    // elsewhere in the parent binary.
    fn canonical(value: &Value) -> Value {
        match value {
            Value::Object(map) => {
                let sorted: std::collections::BTreeMap<_, _> = map
                    .iter()
                    .map(|(key, value)| (key.clone(), canonical(value)))
                    .collect();
                Value::Object(sorted.into_iter().collect())
            }
            Value::Array(items) => Value::Array(items.iter().map(canonical).collect()),
            _ => value.clone(),
        }
    }
    format!("snapshot:{}", canonical(&snapshot))
}

fn nodes_from_payloads(globals: &Value, providers: &Value) -> Result<Vec<Node>> {
    let globals = globals
        .get("proxies")
        .and_then(Value::as_object)
        .ok_or_else(|| anyhow!("invalid global proxy discovery response"))?;
    let providers = providers
        .get("providers")
        .and_then(Value::as_object)
        .ok_or_else(|| anyhow!("invalid provider discovery response"))?;
    let mut nodes = Vec::new();
    let mut provided_identities = HashSet::new();
    for (provider_name, provider) in providers {
        let provider = provider
            .as_object()
            .ok_or_else(|| anyhow!("invalid provider discovery entry"))?;
        if provider
            .get("vehicleType")
            .and_then(Value::as_str)
            .is_some_and(|kind| kind.eq_ignore_ascii_case("compatible"))
        {
            continue;
        }
        let Some(proxies) = provider.get("proxies") else {
            continue;
        };
        let proxies = proxies
            .as_array()
            .ok_or_else(|| anyhow!("invalid provider node list"))?;
        // Provider namespaces are distinct: identical names/payloads in two
        // providers must still be tested via each provider's healthcheck.
        let mut seen = HashSet::new();
        for proxy in proxies.iter().filter(|proxy| is_node(proxy)) {
            let Some(name) = proxy
                .get("name")
                .and_then(Value::as_str)
                .filter(|s| !s.is_empty())
            else {
                continue;
            };
            let key = identity(name, proxy);
            if seen.insert(key.clone()) {
                provided_identities.insert(key);
                nodes.push(Node {
                    name: name.to_owned(),
                    provider: Some(provider_name.clone()),
                });
            }
        }
    }
    let mut seen = provided_identities;
    for (name, proxy) in globals {
        if !name.is_empty() && is_node(proxy) && seen.insert(identity(name, proxy)) {
            nodes.push(Node {
                name: name.clone(),
                provider: None,
            });
        }
    }
    nodes.sort_by(|a, b| {
        a.provider
            .as_deref()
            .unwrap_or("")
            .cmp(b.provider.as_deref().unwrap_or(""))
            .then_with(|| a.name.cmp(&b.name))
    });
    Ok(nodes)
}

fn spawn_measurement(
    tasks: &mut JoinSet<(usize, RoundSample)>,
    api: &Api,
    node: &Node,
    url: &str,
    timeout_ms: u64,
    index: usize,
    round: usize,
) {
    let (api, node, url) = (api.clone(), node.clone(), url.to_owned());
    tasks.spawn(async move { (index, measure(&api, &node, &url, timeout_ms, round).await) });
}

fn measurement_segments(node: &Node) -> Vec<&str> {
    match &node.provider {
        Some(provider) => vec!["providers", "proxies", provider, &node.name, "healthcheck"],
        None => vec!["proxies", &node.name, "delay"],
    }
}

fn delay_from_response(response: &Value) -> Result<f64> {
    response
        .get("delay")
        .and_then(Value::as_f64)
        .filter(|delay| delay.is_finite() && *delay > 0.0)
        .ok_or_else(|| anyhow!("no valid positive delay returned"))
}

async fn measure(api: &Api, node: &Node, url: &str, timeout_ms: u64, round: usize) -> RoundSample {
    let timeout = timeout_ms.to_string();
    let response: Result<Value> = api
        .get_json(
            &measurement_segments(node),
            &[("url", url), ("timeout", &timeout), ("expected", "200-299")],
            Duration::from_millis(timeout_ms.saturating_add(3000)),
        )
        .await;
    let delay = response.and_then(|value| delay_from_response(&value));
    let (delay_ms, error) = match delay {
        Ok(delay) => (Some(delay), None),
        Err(error) => (None, Some(error.to_string())),
    };
    RoundSample {
        round,
        delay_ms,
        error,
    }
}

fn aggregate(mut result: SpeedResult) -> SpeedResult {
    let mut delays = Vec::with_capacity(result.samples.len());
    let mut errors = Vec::new();
    for sample in &result.samples {
        match (sample.delay_ms, &sample.error) {
            (Some(delay), None) if delay.is_finite() && delay > 0.0 => delays.push(delay),
            _ => errors.push(format!(
                "round {}: {}",
                sample.round,
                sample
                    .error
                    .as_deref()
                    .unwrap_or("no valid positive delay returned")
            )),
        }
    }
    if result.samples.is_empty() {
        errors.push("no round samples returned".to_owned());
    }
    if errors.is_empty() {
        delays.sort_by(f64::total_cmp);
        let middle = delays.len() / 2;
        result.delay_ms = Some(if delays.len() % 2 == 0 {
            // Avoid overflowing when both central values are large and finite.
            delays[middle - 1] + (delays[middle] - delays[middle - 1]) / 2.0
        } else {
            delays[middle]
        });
        result.error = None;
    } else {
        result.delay_ms = None;
        result.error = Some(errors.join("; "));
    }
    result
}

fn summarize(url: String, mut results: Vec<SpeedResult>, threshold_ms: u64) -> UrlReport {
    let mut eligible: Vec<_> = results
        .iter()
        .filter(|row| {
            row.delay_ms.is_some_and(|delay| {
                delay.is_finite() && delay > 0.0 && delay < threshold_ms as f64
            })
        })
        .cloned()
        .collect();
    eligible.sort_by(|a, b| {
        b.delay_ms
            .unwrap()
            .total_cmp(&a.delay_ms.unwrap())
            .then_with(|| a.name.cmp(&b.name))
            .then_with(|| {
                a.provider
                    .as_deref()
                    .unwrap_or("")
                    .cmp(b.provider.as_deref().unwrap_or(""))
            })
    });
    results.sort_by(|a, b| {
        a.provider
            .as_deref()
            .unwrap_or("")
            .cmp(b.provider.as_deref().unwrap_or(""))
            .then_with(|| a.name.cmp(&b.name))
    });
    UrlReport {
        url,
        tested: results.len(),
        failed: results.iter().filter(|row| row.delay_ms.is_none()).count(),
        eligible,
        results,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn filters_groups_and_builtin_types() {
        for kind in [
            "Selector",
            "URL-Test",
            "Fallback",
            "LoadBalance",
            "Relay",
            "Direct",
            "RejectDrop",
            "Pass",
            "PassRule",
            "Compatible",
            "DNS",
            "",
        ] {
            assert!(!is_node(&json!({"type": kind})));
        }
        assert!(!is_node(&json!({"type": "FutureGroup", "all": []})));
        for kind in ["Trojan", "Shadowsocks", "Vmess", "WireGuard", "Hysteria2"] {
            assert!(is_node(&json!({"type": kind})));
        }
    }

    #[test]
    fn preserves_provider_namespaces_and_distinct_colliding_globals() {
        let globals = json!({"proxies": {
            "shared": {"type": "Trojan", "id": "global"},
            "inline": {"type": "Trojan"},
            "duplicate": {"type": "Trojan", "id": "provider"},
            "builtin": {"type": "Direct"}
        }});
        let providers = json!({"providers": {
            "A": {"proxies": [
                {"name": "shared", "type": "Trojan", "id": "provider"},
                {"name": "shared", "type": "Trojan", "id": "provider"},
                {"name": "shared", "type": "Vmess", "id": "distinct"}
            ]},
            "B": {"proxies": [{"name": "shared", "type": "Trojan", "id": "provider"}]},
            "default": {"vehicleType": "Compatible", "proxies": [{"name": "inline", "type": "Trojan"}]}
        }});
        let nodes = nodes_from_payloads(&globals, &providers).unwrap();
        assert_eq!(nodes.len(), 5);
        assert_eq!(nodes.iter().filter(|n| n.provider.is_none()).count(), 2);
        assert_eq!(
            nodes
                .iter()
                .filter(|n| n.provider.as_deref() == Some("A"))
                .count(),
            2
        );
    }

    #[test]
    fn identical_snapshots_deduplicate_but_names_alone_do_not() {
        let globals = json!({"proxies": {"same": {"type": "Trojan"}, "other": {"type": "Vmess"}}});
        let providers = json!({"providers": {"P": {"proxies": [
            {"name": "same", "type": "Trojan"}, {"name": "other", "type": "Trojan"}
        ]}}});
        assert_eq!(nodes_from_payloads(&globals, &providers).unwrap().len(), 3);
        assert_eq!(
            identity(
                "same",
                &json!({"type": "Trojan", "extra": {"z": 1, "a": 2}})
            ),
            identity(
                "same",
                &json!({"extra": {"a": 2, "z": 1}, "type": "Trojan"})
            )
        );
    }

    #[test]
    fn optional_provider_404_and_malformed_discovery() {
        let (payload, warnings) = optional_providers(Err(HttpStatusError(404).into())).unwrap();
        assert_eq!(warnings.len(), 1);
        assert!(nodes_from_payloads(&json!({"proxies": {}}), &payload).is_ok());
        assert!(optional_providers(Err(HttpStatusError(401).into())).is_err());
        assert!(nodes_from_payloads(&json!({}), &payload).is_err());
        assert!(nodes_from_payloads(
            &json!({"proxies": {}}),
            &json!({"providers": {"P": {"proxies": {}}}})
        )
        .is_err());
    }

    #[test]
    fn rejects_invalid_delays_and_sorts_strictly() {
        for value in [
            Value::Null,
            json!(0),
            json!(-1),
            json!(true),
            json!("20"),
            json!({}),
        ] {
            assert!(delay_from_response(&json!({"delay": value})).is_err());
        }
        let rows = [Some(600.0), Some(599.0), Some(1.0), Some(250.0), None]
            .into_iter()
            .map(|delay| SpeedResult {
                name: "synthetic".into(),
                provider: None,
                url: "https://example.invalid".into(),
                delay_ms: delay,
                error: delay.is_none().then(|| "failure".into()),
                samples: Vec::new(),
            })
            .collect();
        let report = summarize("https://example.invalid".into(), rows, 600);
        assert_eq!(
            report
                .eligible
                .iter()
                .map(|row| row.delay_ms.unwrap())
                .collect::<Vec<_>>(),
            vec![599.0, 250.0, 1.0]
        );
        assert_eq!((report.tested, report.failed), (5, 1));
        let serialized = serde_json::to_value(report).unwrap();
        assert!(serialized["results"][4]["delay_ms"].is_null());
    }

    fn aggregate_samples(delays: &[Option<f64>]) -> SpeedResult {
        aggregate(SpeedResult {
            name: "synthetic".into(),
            provider: None,
            url: "https://example.invalid".into(),
            delay_ms: None,
            error: None,
            samples: delays
                .iter()
                .enumerate()
                .map(|(index, &delay_ms)| RoundSample {
                    round: index + 1,
                    delay_ms,
                    error: delay_ms.is_none().then(|| "request failed".into()),
                })
                .collect(),
        })
    }

    #[test]
    fn aggregates_odd_and_even_medians_without_nested_samples() {
        for (delays, expected) in [
            (vec![Some(500.0), Some(100.0), Some(300.0)], 300.0),
            (
                vec![Some(500.0), Some(100.0), Some(400.0), Some(200.0)],
                300.0,
            ),
            (vec![Some(42.0)], 42.0),
        ] {
            let result = aggregate_samples(&delays);
            assert_eq!(result.delay_ms, Some(expected));
            assert!(result.error.is_none());
            assert_eq!(result.samples.len(), delays.len());
            let serialized = serde_json::to_value(&result).unwrap();
            for (index, sample) in serialized["samples"].as_array().unwrap().iter().enumerate() {
                assert_eq!(sample["round"], index + 1);
                assert_eq!(sample["delay_ms"], json!(delays[index]));
                assert!(sample.get("samples").is_none());
            }
        }
    }

    #[test]
    fn any_failed_or_invalid_round_excludes_node() {
        for invalid in [
            None,
            Some(0.0),
            Some(-1.0),
            Some(f64::NAN),
            Some(f64::INFINITY),
        ] {
            let result = aggregate_samples(&[Some(100.0), invalid, Some(200.0)]);
            assert!(result.delay_ms.is_none());
            assert!(result.error.as_deref().unwrap().contains("round 2:"));
            assert_eq!(result.samples[1].delay_ms.is_none(), invalid.is_none());
            if invalid.is_none() {
                assert_eq!(result.samples[1].error.as_deref(), Some("request failed"));
                assert!(result.error.as_deref().unwrap().contains("request failed"));
            }
            let report = summarize(result.url.clone(), vec![result], 600);
            assert!(report.eligible.is_empty());
            assert_eq!((report.tested, report.failed), (1, 1));
        }
        let boundary = aggregate_samples(&[Some(500.0), Some(600.0), Some(700.0)]);
        assert!(summarize(boundary.url.clone(), vec![boundary], 600)
            .eligible
            .is_empty());
    }

    #[test]
    fn claude_warning_applies_to_default_and_claude_hosts() {
        assert!(tests_claude(&SpeedOptions::default().urls));
        assert!(tests_claude(&["https://api.claude.ai/test".into()]));
        assert!(!tests_claude(&[
            "https://claude.ai.example.invalid/test".into()
        ]));
        assert!(CLAUDE_IP_WARNING.contains("本工具未检测 IP 信誉"));
    }

    #[test]
    fn options_and_endpoint_protocol() {
        let mut options = SpeedOptions::default();
        assert_eq!(options.rounds, 3);
        assert_eq!(validated_urls(&options).unwrap().len(), 3);
        for rounds in [0, 21, usize::MAX] {
            options.rounds = rounds;
            assert!(validated_urls(&options).is_err());
        }
        for rounds in [1, 3, 20] {
            options.rounds = rounds;
            assert!(validated_urls(&options).is_ok());
        }
        options.concurrency = 65;
        assert!(validated_urls(&options).is_err());
        options.concurrency = 1;
        options.urls = vec!["https://user:private@example.invalid".into()];
        assert!(validated_urls(&options).is_err());
        options.urls = vec!["https://example.invalid".into(); 2];
        assert_eq!(validated_urls(&options).unwrap().len(), 1);
        let node = Node {
            name: "synthetic /?#".into(),
            provider: Some("源/A".into()),
        };
        assert_eq!(
            measurement_segments(&node),
            vec![
                "providers",
                "proxies",
                "源/A",
                "synthetic /?#",
                "healthcheck"
            ]
        );
        let node = Node {
            name: "synthetic".into(),
            provider: None,
        };
        assert_eq!(
            measurement_segments(&node),
            vec!["proxies", "synthetic", "delay"]
        );
    }
}
