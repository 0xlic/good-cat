//! No real network: stock macOS tools plus mock curl/uname/sysctl.
#![cfg(target_os = "macos")]
use serde_json::{json, Value};
use std::{
    fs,
    os::unix::fs::PermissionsExt,
    path::{Path, PathBuf},
    process::{Command, Output},
};

fn write_exec(path: &Path, text: &str) {
    fs::write(path, text).unwrap();
    fs::set_permissions(path, fs::Permissions::from_mode(0o700)).unwrap();
}
fn run(
    script: &Path,
    bin: &Path,
    fixtures: &Path,
    agent: &Path,
    cache: &Path,
    force: bool,
) -> Output {
    let mut command = Command::new("/bin/bash");
    command.arg(script).args([
        "--repo",
        "EXAMPLE/GOOD-CAT",
        "--cache-dir",
        cache.to_str().unwrap(),
    ]);
    if force {
        command.arg("--force");
    }
    command
        .current_dir(agent)
        .env(
            "PATH",
            format!("{}:{}", bin.display(), std::env::var("PATH").unwrap()),
        )
        .env("GOOD_CAT_MOCK_FIXTURES", fixtures)
        .output()
        .unwrap()
}

#[test]
fn downloads_verify_before_publish_and_preserve_existing_parent_and_binary() {
    let temp = tempfile::tempdir().unwrap();
    let fixtures = temp.path().join("fixtures");
    let bin = temp.path().join("bin");
    let parent = temp.path().join("unrelated parent");
    let cache = parent.join("cache");
    fs::create_dir(&fixtures).unwrap();
    fs::create_dir(&bin).unwrap();
    fs::create_dir(&parent).unwrap();
    fs::set_permissions(&parent, fs::Permissions::from_mode(0o755)).unwrap();
    fs::write(parent.join("unrelated.txt"), "untouched").unwrap();
    write_exec(
        &bin.join("uname"),
        "#!/bin/bash\ncase \"$1\" in -s) echo Darwin;; -m) echo x86_64;; esac\n",
    );
    // Simulate an Intel shell under Rosetta on Apple Silicon.
    write_exec(&bin.join("sysctl"), "#!/bin/bash\necho 1\n");
    write_exec(
        &bin.join("curl"),
        r#"#!/bin/bash
set -eu
out=
url=
while [ "$#" -gt 0 ]; do
    if [ "$1" = --output ]; then out=$2; shift 2; else url=$1; shift; fi
done
case "$url" in
  https://api.github.com/repos/EXAMPLE/GOOD-CAT/releases/latest) source=release.json;;
  https://github.com/example/good-cat/releases/download/v0.1.0/SHA256SUMS) source=SHA256SUMS;;
  https://github.com/example/good-cat/releases/download/v0.1.0/good-cat-aarch64-apple-darwin) source=payload;;
  *) echo 'unexpected network URL' >&2; exit 1;;
esac
cp "$GOOD_CAT_MOCK_FIXTURES/$source" "$out"
"#,
    );
    let asset = "good-cat-aarch64-apple-darwin";
    fs::write(fixtures.join("payload"), "verified payload; never executed").unwrap();
    let hash = Command::new("/usr/bin/shasum")
        .args(["-a", "256"])
        .arg(fixtures.join("payload"))
        .output()
        .unwrap();
    assert!(hash.status.success());
    let hash = String::from_utf8(hash.stdout)
        .unwrap()
        .split_whitespace()
        .next()
        .unwrap()
        .to_owned();
    fs::write(fixtures.join("SHA256SUMS"), format!("{hash}  {asset}\n")).unwrap();
    fs::write(fixtures.join("release.json"),json!({"tag_name":"v0.1.0","assets":[
        {"name":asset,"browser_download_url":format!("https://github.com/example/good-cat/releases/download/v0.1.0/{asset}")},
        {"name":"SHA256SUMS","browser_download_url":"https://github.com/example/good-cat/releases/download/v0.1.0/SHA256SUMS"}
    ]}).to_string()).unwrap();
    let source = Path::new(env!("CARGO_MANIFEST_DIR")).join("../skill/good-cat/scripts/install.sh");
    // An installed Skill need not have cli/ or the source repository's layout.
    let portable = temp.path().join("installed-skill/scripts");
    fs::create_dir_all(&portable).unwrap();
    let script = portable.join("install.sh");
    fs::copy(source, &script).unwrap();
    let result = run(&script, &bin, &fixtures, temp.path(), &cache, false);
    assert!(
        result.status.success(),
        "{}",
        String::from_utf8_lossy(&result.stderr)
    );
    let value: Value = serde_json::from_slice(&result.stdout).unwrap();
    let executable = PathBuf::from(value["executable"].as_str().unwrap());
    assert_eq!(value["target"], "aarch64-apple-darwin");
    assert_eq!(value["sha256"], hash);
    assert!(executable.starts_with(cache.canonicalize().unwrap()));
    assert_eq!(value["executable_relative"], "./unrelated parent/cache/EXAMPLE/GOOD-CAT/v0.1.0/aarch64-apple-darwin/good-cat-aarch64-apple-darwin");
    assert_eq!(
        fs::metadata(&parent).unwrap().permissions().mode() & 0o777,
        0o755
    );
    assert_eq!(
        fs::read_to_string(parent.join("unrelated.txt")).unwrap(),
        "untouched"
    );
    let original = fs::read(&executable).unwrap();
    fs::write(fixtures.join("payload"), "bad download").unwrap();
    // Existing cache is reverified and reused, without touching the bad download.
    assert!(run(&script, &bin, &fixtures, temp.path(), &cache, false)
        .status
        .success());
    // Forced redownload fails verification but never replaces the working binary.
    assert!(!run(&script, &bin, &fixtures, temp.path(), &cache, true)
        .status
        .success());
    assert_eq!(fs::read(executable).unwrap(), original);
}
