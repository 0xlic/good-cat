use std::{
    fs,
    path::{Path, PathBuf},
    process::Command,
};

fn root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .unwrap()
        .to_owned()
}

#[test]
fn required_skill_resources_are_readable() {
    let skill = root().join("skill/good-cat");
    for path in [
        "SKILL.md",
        "assets/good-cat.js",
        "scripts/install.sh",
        "scripts/install.ps1",
    ] {
        let text = fs::read_to_string(skill.join(path)).unwrap();
        assert!(!text.is_empty());
    }
}

#[test]
fn skill_selects_countries_after_measurement_instead_of_fixed_defaults() {
    let text = fs::read_to_string(root().join("skill/good-cat/SKILL.md")).unwrap();
    assert!(text.contains("不预设任何国家"));
    assert!(text.contains("国家得分 = 该国全部合格节点的聚合中位延迟的中位数"));
    assert!(text.contains("取国家得分升序的前三名"));
    assert!(text.contains("不能直接取报告前三个节点"));
    assert!(text.contains("不足三个"));
    assert!(text.contains("测速后统一清理并分析结果"));
    assert!(text.contains("即使测速成功也不能作为有效证据"));
    assert!(text.contains("原始测速报告保持不变"));
    assert!(!text.contains("将固定选择写到"));
    assert!(!text.contains("默认 TW/SG/JP"));
    assert!(!text.contains("默认筛选台湾、新加坡、日本"));
}

#[test]
fn dynamic_country_patterns_support_new_countries_and_aliases() {
    let template = fs::read_to_string(root().join("skill/good-cat/assets/good-cat.js")).unwrap();
    let extended = template
        .replace(
            "const COUNTRY_PATTERNS = {",
            "const COUNTRY_PATTERNS = {\n  FI: \"芬兰|Finland|Helsinki\",",
        )
        .replace("US: \"美国", "US: \"Ashburn|美国");
    let report = serde_json::json!({"reports": [
        {"url":"https://chatgpt.com/cdn-cgi/trace", "results":[
            {"name":"芬兰 Helsinki 01", "delay_ms":70},
            {"name":"Ashburn 01", "delay_ms":90}]},
        {"url":"https://claude.ai/cdn-cgi/trace", "results":[
            {"name":"芬兰 Helsinki 01", "delay_ms":80}]}
    ]});
    let selection = serde_json::json!({"chatgpt":["FI","US"], "claude":["FI"]});
    assert!(good_cat::render::render(&template, &report, &selection, 600).is_err());
    let rendered = good_cat::render::render(&extended, &report, &selection, 600).unwrap();
    assert!(rendered
        .javascript
        .contains("const CHATGPT_COUNTRIES = [\"FI\",\"US\"];"));
    assert!(rendered
        .javascript
        .contains("const CLAUDE_COUNTRIES = [\"FI\"];"));
    assert!(rendered
        .javascript
        .contains("FI: \"芬兰|Finland|Helsinki\""));
    let skill = fs::read_to_string(root().join("skill/good-cat/SKILL.md")).unwrap();
    assert!(skill.contains("不是可选国家白名单"));
    assert!(skill.contains("render --template \"<WORKSPACE>/good-cat.template.js\""));
}

#[test]
fn regional_exclusion_is_an_agent_selection_rule_not_template_logic() {
    let text = fs::read_to_string(root().join("skill/good-cat/SKILL.md")).unwrap();
    assert!(text.contains("由 Agent 在测速完成后、国家排名前排除香港（HK）、澳门（MO）节点"));
    assert!(text.contains("`Porxy` 始终是全部节点，不排除香港、澳门"));
    let template = fs::read_to_string(root().join("skill/good-cat/assets/good-cat.js")).unwrap();
    assert!(!template.contains("exclude-filter"));
    assert!(!template.contains("code === \"HK\""));
    assert!(!template.contains("code === \"MO\""));
}

#[cfg(unix)]
#[test]
fn bash_bootstrap_parses_and_missing_repo_does_not_download() {
    let script = root().join("skill/good-cat/scripts/install.sh");
    assert!(Command::new("bash")
        .arg("-n")
        .arg(&script)
        .status()
        .unwrap()
        .success());
    let result = Command::new("bash").arg(script).output().unwrap();
    assert!(!result.status.success());
}

#[cfg(windows)]
#[test]
fn powershell_bootstrap_parses_without_execution() {
    // Parse the source only; do not download or change execution policies.
    let script = root().join("skill/good-cat/scripts/install.ps1");
    let result=Command::new("powershell.exe").args(["-NoProfile","-NonInteractive","-Command",
        "$e=$null; $t=$null; [System.Management.Automation.Language.Parser]::ParseFile($env:GOOD_CAT_TEST_SCRIPT,[ref]$t,[ref]$e) | Out-Null; if ($e.Count -gt 0) { $e | Out-String | Write-Error; exit 1 }"])
        .env("GOOD_CAT_TEST_SCRIPT",script).output().unwrap();
    assert!(
        result.status.success(),
        "{}",
        String::from_utf8_lossy(&result.stderr)
    );
}
