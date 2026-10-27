use anyhow::{bail, ensure, Context, Result};
use regex::{Regex, RegexBuilder};
use serde_json::Value;
use std::collections::{HashMap, HashSet};

pub struct Rendered {
    pub javascript: String,
    pub selection: Value,
}

/// Validate an explicit model selection; never rank or select countries here.
/// Overrides identify a node by its exact (name, provider) pair. A null provider
/// means an unprovided node, not a wildcard across subscription providers.
pub fn render(
    template: &str,
    report: &Value,
    selection: &Value,
    threshold_ms: u64,
) -> Result<Rendered> {
    ensure!(threshold_ms > 0, "threshold_ms must be positive");
    let block_re = Regex::new(r"(?ms)^const COUNTRY_PATTERNS = \{\r?\n(.*?)^\};\r?$")?;
    let blocks: Vec<_> = block_re.captures_iter(template).collect();
    ensure!(blocks.len() == 1, "missing or duplicate COUNTRY_PATTERNS");
    let entry_re = Regex::new(r#"^\s*([A-Z]{2}):\s*("(?:\\.|[^"\\])*")\s*,?\s*$"#)?;
    let mut patterns = HashMap::new();
    for line in blocks[0][1].lines() {
        let entry = entry_re
            .captures(line)
            .context("unsupported COUNTRY_PATTERNS entry")?;
        let pattern: String = serde_json::from_str(&entry[2])?;
        // JavaScript/RE2 word boundaries are ASCII, even around Unicode names.
        let pattern = pattern.replace(r"\b", r"(?-u:\b)");
        let compiled = RegexBuilder::new(&pattern)
            .case_insensitive(true)
            .build()
            .context("invalid country pattern")?;
        ensure!(
            patterns.insert(entry[1].to_owned(), compiled).is_none(),
            "duplicate country pattern"
        );
    }
    ensure!(!patterns.is_empty(), "empty COUNTRY_PATTERNS");

    let object = selection
        .as_object()
        .context("selection must be an object")?;
    ensure!(
        object.keys().all(|key| matches!(
            key.as_str(),
            "chatgpt" | "claude" | "node_country_overrides"
        )),
        "unknown selection field"
    );
    let mut overrides = HashMap::new();
    if let Some(value) = object.get("node_country_overrides") {
        for item in value
            .as_array()
            .context("node_country_overrides must be an array")?
        {
            let item = item.as_object().context("override must be an object")?;
            ensure!(
                item.len() == 3
                    && item.contains_key("name")
                    && item.contains_key("provider")
                    && item.contains_key("country"),
                "override requires name, provider and country"
            );
            let name = item["name"]
                .as_str()
                .filter(|name| !name.is_empty())
                .context("override name must be nonempty")?;
            let provider = provider_key(&item["provider"])?;
            let country = item["country"]
                .as_str()
                .context("override country must be a string")?;
            ensure!(
                patterns.contains_key(country),
                "unknown override country: {country}"
            );
            ensure!(
                overrides
                    .insert((name.to_owned(), provider), country.to_owned())
                    .is_none(),
                "duplicate or conflicting override for {name}"
            );
        }
    }

    let reports = report
        .get("reports")
        .and_then(Value::as_array)
        .context("report requires reports array")?;
    let mut replacements = Vec::new();
    for (service, constant, group) in [
        ("chatgpt", "CHATGPT_COUNTRIES", "chatgptGroupName"),
        ("claude", "CLAUDE_COUNTRIES", "claudeGroupName"),
    ] {
        let countries = object
            .get(service)
            .and_then(Value::as_array)
            .context("service selection must be an array")?;
        ensure!(
            !countries.is_empty(),
            "{service} selection must not be empty"
        );
        let mut requested = HashSet::new();
        for country in countries {
            let code = country.as_str().context("country code must be a string")?;
            ensure!(patterns.contains_key(code), "unknown country code: {code}");
            ensure!(requested.insert(code), "duplicate country code: {code}");
        }

        let declaration_re = Regex::new(&format!(r"(?m)^const {constant} = \[[^\r\n]*\];\r?$"))?;
        let declarations: Vec<_> = declaration_re.find_iter(template).collect();
        ensure!(
            declarations.len() == 1,
            "missing or duplicate declaration: {constant}"
        );
        let declaration = declarations[0];
        // Keep the CR in a CRLF terminator outside the replacement span.
        let end = declaration.end() - usize::from(declaration.as_str().ends_with('\r'));
        replacements.push((
            declaration.start(),
            end,
            format!("const {constant} = {};", serde_json::to_string(countries)?),
        ));

        // Both services are url-test groups; read the URL from the real call.
        let url_re = Regex::new(&format!(
            r#"\burlTest\(\s*{group}\s*,\s*("(?:\\.|[^"\\])*")\s*,\s*{constant}\s*\)"#
        ))?;
        let urls: Vec<_> = url_re.captures_iter(template).collect();
        ensure!(
            urls.len() == 1,
            "missing or duplicate service URL: {service}"
        );
        let url: String = serde_json::from_str(&urls[0][1])?;
        let matching: Vec<_> = reports
            .iter()
            .filter(|item| item.get("url").and_then(Value::as_str) == Some(url.as_str()))
            .collect();
        ensure!(
            matching.len() == 1,
            "missing or duplicate {service} report for {url}"
        );
        let results = matching[0]
            .get("results")
            .and_then(Value::as_array)
            .context("service report requires results array")?;
        let mut evidence = HashSet::new();
        for item in results {
            ensure!(item.is_object(), "node result must be an object");
            // A nested URL, when supplied, must agree with its service report.
            if item
                .get("url")
                .is_some_and(|value| value.as_str() != Some(url.as_str()))
            {
                continue;
            }
            if item
                .get("error")
                .is_some_and(|error| !error.is_null() && error != "")
            {
                continue;
            }
            let Some(delay) = item.get("delay_ms").and_then(Value::as_f64) else {
                continue;
            };
            if !delay.is_finite() || delay <= 0.0 || delay >= threshold_ms as f64 {
                continue;
            }
            let name = item
                .get("name")
                .and_then(Value::as_str)
                .filter(|name| !name.is_empty())
                .context("qualified node requires a name")?;
            let provider = provider_key(item.get("provider").unwrap_or(&Value::Null))?;
            let matches: Vec<_> = patterns
                .iter()
                .filter(|(_, pattern)| pattern.is_match(name))
                .map(|(code, _)| code)
                .collect();
            let country = if let Some(country) = overrides.get(&(name.to_owned(), provider)) {
                ensure!(
                    matches.contains(&country),
                    "override cannot be represented by the template filter for {name}"
                );
                Some(country)
            } else if matches.len() == 1 {
                Some(matches[0])
            } else {
                None // Unknown or ambiguous names are not evidence without an override.
            };
            if let Some(country) = country {
                evidence.insert(country.as_str());
            }
        }
        for country in requested {
            ensure!(
                evidence.contains(country),
                "{service}: no unambiguous qualifying evidence for {country}"
            );
        }
    }
    replacements.sort_by_key(|replacement| replacement.0);
    let mut javascript = template.to_owned();
    for (start, end, replacement) in replacements.into_iter().rev() {
        javascript.replace_range(start..end, &replacement);
    }
    Ok(Rendered {
        javascript,
        selection: selection.clone(),
    })
}

fn provider_key(value: &Value) -> Result<Option<String>> {
    match value {
        Value::Null => Ok(None),
        Value::String(provider) => Ok(Some(provider.clone())),
        _ => bail!("provider must be a string or null"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    const TEMPLATE: &str = include_str!("../../skill/good-cat/assets/good-cat.js");

    fn report(name: &str) -> Value {
        json!({"reports": [
            {"url": "https://chatgpt.com/cdn-cgi/trace", "results": [
                {"name": name, "provider": null, "delay_ms": 100, "error": null}]},
            {"url": "https://claude.ai/cdn-cgi/trace", "results": [
                {"name": name, "provider": null, "delay_ms": 200, "error": null}]}
        ]})
    }

    fn selection() -> Value {
        json!({"chatgpt": ["US"], "claude": ["US"]})
    }

    #[test]
    fn template_private_rules_use_domain_and_ip_databases() {
        let body = TEMPLATE
            .split("const privateRules = [")
            .nth(1)
            .unwrap()
            .split("];")
            .next()
            .unwrap();
        let rules: Value = serde_json::from_str(&format!("[{body}]")).unwrap();
        assert_eq!(
            rules,
            json!(["GEOSITE,private,DIRECT", "GEOIP,private,DIRECT,no-resolve"])
        );
    }

    #[test]
    fn preserves_template_and_line_endings_and_explicit_order() {
        let lf_template = TEMPLATE.replace("\r\n", "\n");
        for template in [lf_template.clone(), lf_template.replace('\n', "\r\n")] {
            let mut report = report("US");
            report["reports"][0]["results"]
                .as_array_mut()
                .unwrap()
                .push(json!({"name":"JP", "delay_ms":50}));
            let selection = json!({"chatgpt":["US","JP"], "claude":["US"]});
            let rendered = render(&template, &report, &selection, 600).unwrap();
            let expected = template
                .replace(
                    "const CHATGPT_COUNTRIES = [\"TW\", \"SG\", \"JP\"];",
                    "const CHATGPT_COUNTRIES = [\"US\",\"JP\"];",
                )
                .replace(
                    "const CLAUDE_COUNTRIES = [\"TW\", \"SG\", \"JP\"];",
                    "const CLAUDE_COUNTRIES = [\"US\"];",
                );
            assert_eq!(rendered.javascript, expected);
            assert_eq!(rendered.selection, selection);
        }
    }

    #[test]
    fn claude_factory_url_and_country_count_are_configurable() {
        let template = TEMPLATE.replace(
            "https://claude.ai/cdn-cgi/trace",
            "https://claude.ai/cdn-cgi/trace?manual",
        );
        let mut report = report("TW");
        report["reports"][1]["url"] = json!("https://claude.ai/cdn-cgi/trace?manual");
        report["reports"][1]["results"] = json!([
            {"name":"TW", "delay_ms":100},
            {"name":"SG", "delay_ms":100},
            {"name":"JP", "delay_ms":100}
        ]);
        for countries in [
            json!(["TW"]),
            json!(["TW", "SG"]),
            json!(["TW", "SG", "JP"]),
        ] {
            let selection = json!({"chatgpt":["TW"], "claude":countries});
            let rendered = render(&template, &report, &selection, 600).unwrap();
            assert!(rendered.javascript.contains(&format!(
                "const CLAUDE_COUNTRIES = {};",
                serde_json::to_string(&countries).unwrap()
            )));
        }
        assert!(render(
            TEMPLATE,
            &report,
            &json!({"chatgpt":["TW"],"claude":["TW"]}),
            600
        )
        .is_err());
        let duplicate = format!(
            "{template}\nurlTest(claudeGroupName, \"https://duplicate\", CLAUDE_COUNTRIES);"
        );
        assert!(render(
            &duplicate,
            &report,
            &json!({"chatgpt":["TW"],"claude":["TW"]}),
            600
        )
        .is_err());
    }

    #[test]
    fn template_uses_urltest_for_claude_without_selector_children() {
        let script = r#"
const assert = require('node:assert/strict');
const vm = require('node:vm');
const template = process.argv[1];
function check(countries) {
  const source = countries ? template.replace(
    'const CLAUDE_COUNTRIES = ["TW", "SG", "JP"];',
    'const CLAUDE_COUNTRIES = ' + JSON.stringify(countries) + ';'
  ) : template;
  const context = vm.createContext({});
  vm.runInContext(source, context);
  const config = {
    proxies: [{name: 'TW node'}, {name: 'SG node'}, {name: 'JP node'}],
    'proxy-providers': {subscription: {type: 'file', path: 'nodes.yaml'}},
    'rule-providers': {old: {}}, 'sub-rules': {old: []}, rules: ['old']
  };
  context.config = config;
  const result = vm.runInContext('main(config)', context);
  assert.equal(result, config);
  const groups = JSON.parse(JSON.stringify(result['proxy-groups']));
  assert.equal(groups.length, 3);
  assert.deepEqual(groups.map(g => g.name), ['🤖 ChatGPT', '🧠 Claude', '🌐 Porxy']);
  for (const [name, url, constant] of [
    ['🤖 ChatGPT', 'https://chatgpt.com/cdn-cgi/trace', 'CHATGPT_COUNTRIES'],
    ['🧠 Claude', 'https://claude.ai/cdn-cgi/trace', 'CLAUDE_COUNTRIES'],
    ['🌐 Porxy', 'https://www.gstatic.com/generate_204', undefined]
  ]) {
    const expected = {name, type: 'url-test', 'include-all': true, url, interval: 300, tolerance: 50, lazy: true};
    if (constant) expected.filter = vm.runInContext('countryFilter(' + constant + ')', context);
    assert.deepEqual(groups.find(g => g.name === name), expected);
  }
  assert.ok(!groups.some(g => g.type === 'select' || g.name.includes(' · ')));
  assert.deepEqual(Array.from(result.rules), [
    'GEOSITE,private,DIRECT', 'GEOIP,private,DIRECT,no-resolve',
    'GEOSITE,openai,🤖 ChatGPT', 'GEOSITE,anthropic,🧠 Claude',
    'GEOSITE,microsoft,DIRECT', 'GEOSITE,apple,DIRECT',
    'GEOSITE,cn,DIRECT', 'GEOIP,CN,DIRECT,no-resolve', 'MATCH,🌐 Porxy'
  ]);
  assert.equal(result.proxies.length, 3);
  assert.ok(result['proxy-providers'].subscription);
  assert.equal(result['rule-providers'], undefined);
  assert.equal(result['sub-rules'], undefined);
  assert.equal(result.mode, 'rule');
}
check();
check(['US']);
check(['JP', 'TW']);
for (const countries of [[], ['ZZ']]) {
  assert.throws(() => check(countries), /good-cat:/);
}
"#;
        let output = match std::process::Command::new("node")
            .args(["-e", script, TEMPLATE])
            .output()
        {
            Ok(output) => output,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                eprintln!("skipping JavaScript behavior test: node is not installed");
                return;
            }
            Err(error) => panic!("failed to run node: {error}"),
        };
        assert!(
            output.status.success(),
            "JavaScript behavior test failed: {}",
            String::from_utf8_lossy(&output.stderr)
        );
    }

    #[test]
    fn rejects_invalid_selections_and_payloads() {
        for countries in [
            json!([]),
            json!(["US", "US"]),
            json!(["ZZ"]),
            json!(["US\"]; throw new Error('injected');//"]),
            json!(["US\nJP"]),
            json!([null]),
        ] {
            let mut selection = selection();
            selection["chatgpt"] = countries;
            assert!(render(TEMPLATE, &report("US"), &selection, 600).is_err());
        }
    }

    #[test]
    fn requires_exact_service_and_qualified_evidence() {
        assert!(render(TEMPLATE, &json!({}), &selection(), 600).is_err());
        for name in ["mystery", "US JP", "RUSSIAN"] {
            assert!(render(TEMPLATE, &report(name), &selection(), 600).is_err());
        }
        for patch in [
            json!({"delay_ms":0}),
            json!({"delay_ms":600}),
            json!({"delay_ms":-1}),
            json!({"delay_ms":true}),
            json!({"error":"timeout"}),
            json!({"url":"https://wrong"}),
        ] {
            let mut report = report("US");
            report["reports"][0]["results"][0]
                .as_object_mut()
                .unwrap()
                .extend(patch.as_object().unwrap().clone());
            assert!(render(TEMPLATE, &report, &selection(), 600).is_err());
        }
        let mut report = report("US");
        report["reports"][1]["url"] = json!("https://claude.ai/cdn-cgi/trace?different");
        assert!(render(TEMPLATE, &report, &selection(), 600).is_err());
    }

    #[test]
    fn overrides_are_explicit_and_provider_scoped() {
        let name = "US JP\";\nthrow new Error('payload')";
        let mut selection = selection();
        selection["node_country_overrides"] = json!([{"name":name,"provider":null,"country":"US"}]);
        let rendered = render(TEMPLATE, &report(name), &selection, 600).unwrap();
        assert!(!rendered.javascript.contains("payload"));
        assert_eq!(rendered.selection, selection);
        assert!(render(TEMPLATE, &report("mystery"), &json!({"chatgpt":["US"],"claude":["US"],"node_country_overrides":[{"name":"mystery","provider":null,"country":"US"}]}), 600).is_err());
        let mut provided = report(name);
        provided["reports"][0]["results"][0]["provider"] = json!("subscription");
        assert!(render(TEMPLATE, &provided, &selection, 600).is_err());
        selection["node_country_overrides"][0]["country"] = json!("ZZ");
        assert!(render(TEMPLATE, &report(name), &selection, 600).is_err());
        selection["node_country_overrides"] = json!([
            {"name":name,"provider":null,"country":"US"},
            {"name":name,"provider":null,"country":"JP"}]);
        assert!(render(TEMPLATE, &report(name), &selection, 600).is_err());
    }

    #[test]
    fn rejects_duplicate_template_and_report_entries() {
        let template = format!("{TEMPLATE}\nconst CHATGPT_COUNTRIES = [];\n");
        assert!(render(&template, &report("US"), &selection(), 600).is_err());
        let mut report = report("US");
        let duplicate = report["reports"][0].clone();
        report["reports"].as_array_mut().unwrap().push(duplicate);
        assert!(render(TEMPLATE, &report, &selection(), 600).is_err());
    }
}
