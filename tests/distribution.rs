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
fn skill_is_text_only_and_contains_platform_bootstraps() {
    let skill = root().join("skill/good-cat");
    assert!(
        !skill.join("bin").exists(),
        "platform binaries must be cached outside the skill"
    );
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
fn release_asset_contract_and_risk_guidance_are_present() {
    let release = fs::read_to_string(root().join(".github/workflows/release.yml")).unwrap();
    for asset in [
        "good-cat-aarch64-apple-darwin",
        "good-cat-x86_64-apple-darwin",
        "good-cat-x86_64-pc-windows-msvc.exe",
        "SHA256SUMS",
    ] {
        assert!(release.contains(asset), "missing {asset}");
    }
    assert!(release.contains("contents: write"));
    assert!(release.contains("--verify-tag"));
    let skill = fs::read_to_string(root().join("skill/good-cat/SKILL.md")).unwrap();
    assert!(skill.contains("封号风险"));
    assert!(skill.contains("单一国家"));
    assert!(skill.contains("未检测"));
    let readme = fs::read_to_string(root().join("README.md")).unwrap();
    assert!(readme.contains("npx skills add"));
    assert!(readme.contains("SHA256SUMS"));
    assert!(!readme.contains("bin/good-cat[.exe]"));
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
