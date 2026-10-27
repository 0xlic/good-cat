use serde_json::{json, Value};
use std::{
    fs,
    path::{Path, PathBuf},
    process::Command,
};

#[test]
fn workspace_and_render_are_independent_of_cwd() {
    let unrelated = tempfile::tempdir().unwrap();
    let cli = env!("CARGO_BIN_EXE_good-cat");
    let result = Command::new(cli)
        .current_dir(unrelated.path())
        .args(["workspace", "create", "--json"])
        .output()
        .unwrap();
    assert!(result.status.success());
    let value: Value = serde_json::from_slice(&result.stdout).unwrap();
    let workspace = PathBuf::from(value["workspace_absolute"].as_str().unwrap());
    let expected = unrelated.path().join(".good-cat/workspaces");
    assert!(workspace.starts_with(expected.canonicalize().unwrap()));
    let relative = PathBuf::from(value["workspace"].as_str().unwrap());
    assert_eq!(relative.file_name(), workspace.file_name());
    let suffix = Path::new(".good-cat")
        .join("workspaces")
        .join(workspace.file_name().unwrap());
    assert!(relative.ends_with(suffix));
    let selection = PathBuf::from(value["selection_absolute"].as_str().unwrap());
    let report = PathBuf::from(value["report_absolute"].as_str().unwrap());
    fs::write(
        &selection,
        json!({"chatgpt":["TW"],"claude":["JP"]}).to_string(),
    )
    .unwrap();
    fs::write(&report,json!({"reports":[
        {"url":"https://chatgpt.com/cdn-cgi/trace","results":[{"name":"Taiwan","delay_ms":100}]},
        {"url":"https://claude.ai/cdn-cgi/trace","results":[{"name":"Japan","delay_ms":200}]}
    ]}).to_string()).unwrap();
    let result = Command::new(cli)
        .current_dir(unrelated.path())
        .args([
            "--workspace",
            value["workspace"].as_str().unwrap(),
            "render",
            "--json",
        ])
        .output()
        .unwrap();
    assert!(
        result.status.success(),
        "{}",
        String::from_utf8_lossy(&result.stdout)
    );
    let rendered: Value = serde_json::from_slice(&result.stdout).unwrap();
    assert_eq!(rendered["output"], value["script"]);
    assert!(PathBuf::from(value["script_absolute"].as_str().unwrap()).is_file());
    assert_eq!(fs::read_dir(unrelated.path()).unwrap().count(), 1);
    fs::remove_dir_all(workspace).unwrap();
}

#[test]
fn missing_workspace_does_not_fall_back_to_cwd() {
    let unrelated = tempfile::tempdir().unwrap();
    let result = Command::new(env!("CARGO_BIN_EXE_good-cat"))
        .current_dir(unrelated.path())
        .args(["render", "--json"])
        .output()
        .unwrap();
    assert!(!result.status.success());
    let value: Value = serde_json::from_slice(&result.stdout).unwrap();
    assert!(value["error"].as_str().unwrap().contains("--workspace"));
    assert_eq!(fs::read_dir(unrelated.path()).unwrap().count(), 1);
    assert!(unrelated.path().join(".good-cat").is_dir());
}
