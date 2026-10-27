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
    assert!(!text.contains("将固定选择写到"));
    assert!(!text.contains("默认 TW/SG/JP"));
    assert!(!text.contains("默认筛选台湾、新加坡、日本"));
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
