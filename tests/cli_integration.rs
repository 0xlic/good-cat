use rusqlite::Connection;
use serde_json::{json, Value};
use std::{
    fs,
    io::{Read, Write},
    net::TcpListener,
    path::{Path, PathBuf},
    process::{Command, Output},
    thread,
};

fn run(args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_good-cat"))
        .args(args)
        .output()
        .unwrap()
}
fn decoded(out: &Output) -> Value {
    serde_json::from_slice(&out.stdout)
        .unwrap_or_else(|_| panic!("non-json output: {}", String::from_utf8_lossy(&out.stdout)))
}
fn fixture() -> (tempfile::TempDir, PathBuf) {
    let dir = tempfile::tempdir().unwrap();
    let db = Connection::open(dir.path().join("database.sqlite")).unwrap();
    db.execute_batch("CREATE TABLE scripts(id INTEGER PRIMARY KEY,label TEXT NOT NULL,last_update_time INTEGER NOT NULL,url TEXT,\"order\" INTEGER); CREATE TABLE profiles(id INTEGER PRIMARY KEY,extend_type TEXT,script_id INTEGER); INSERT INTO profiles VALUES(123,'standard',NULL);").unwrap();
    let path = dir.path().join("shared_preferences.json");
    fs::write(&path,serde_json::to_vec(&json!({"flutter.config":json!({"currentProfileId":123,"patchClashConfig":{"external-controller":"","secret":"integration-private-secret","other":true}}).to_string(),"untouched":true})).unwrap()).unwrap();
    (dir, path)
}
fn args_for(dir: &Path, prefs: &Path) -> Vec<String> {
    vec![
        "--support-dir".into(),
        dir.to_str().unwrap().into(),
        "--preferences".into(),
        prefs.to_str().unwrap().into(),
    ]
}
fn run_owned(args: &[String]) -> Output {
    run(&args.iter().map(String::as_str).collect::<Vec<_>>())
}

#[test]
fn inspect_and_controller_plan_never_write_or_leak_secret() {
    let (dir, prefs) = fixture();
    let original = fs::read(&prefs).unwrap();
    let db = fs::read(dir.path().join("database.sqlite")).unwrap();
    for extra in [
        vec!["inspect"],
        vec!["controller", "status"],
        vec!["controller", "enable"],
    ] {
        let mut args = args_for(dir.path(), &prefs);
        args.extend(extra.iter().map(|s| s.to_string()));
        let out = run_owned(&args);
        assert!(out.status.success(), "{:?}", decoded(&out));
        assert!(!String::from_utf8_lossy(&out.stdout).contains("integration-private-secret"));
    }
    assert_eq!(fs::read(&prefs).unwrap(), original);
    assert_eq!(fs::read(dir.path().join("database.sqlite")).unwrap(), db);
    assert!(!dir
        .path()
        .join(".good-cat-pending-controller.json")
        .exists());
}
#[test]
fn install_plan_does_not_mutate_database_or_register() {
    let (dir, prefs) = fixture();
    let script = dir.path().join("generated.js");
    fs::write(&script, "function main(config) {return config;}").unwrap();
    let mut args = args_for(dir.path(), &prefs);
    args.extend([
        "install".into(),
        "--script".into(),
        script.to_str().unwrap().into(),
        "--activate-current".into(),
    ]);
    let out = run_owned(&args);
    assert!(out.status.success());
    let result = decoded(&out);
    assert_eq!(result["status"], "confirmation_required");
    assert_eq!(result["activate_profile"], 123);
    let db = Connection::open(dir.path().join("database.sqlite")).unwrap();
    assert_eq!(
        db.query_row("SELECT count(*) FROM scripts", [], |r| r.get::<_, i64>(0))
            .unwrap(),
        0
    );
    assert!(!dir.path().join("scripts").exists());
}
#[test]
fn renderer_requires_model_selection_and_preserves_template() {
    let dir = tempfile::tempdir().unwrap();
    let report = dir.path().join("report.json");
    let selection = dir.path().join("selection.json");
    let output = dir.path().join("nested/out.js");
    fs::write(&report,json!({"reports":[
        {"url":"https://chatgpt.com/cdn-cgi/trace","results":[{"name":"US 美国 1","provider":null,"delay_ms":200,"error":null}]},
        {"url":"https://claude.ai/cdn-cgi/trace","results":[{"name":"JP 日本 1","provider":null,"delay_ms":100,"error":null}]}
    ]}).to_string()).unwrap();
    fs::write(
        &selection,
        json!({"chatgpt":["US"],"claude":["JP"]}).to_string(),
    )
    .unwrap();
    let out = run(&[
        "render",
        "--report",
        report.to_str().unwrap(),
        "--selection",
        selection.to_str().unwrap(),
        "--output",
        output.to_str().unwrap(),
    ]);
    assert!(out.status.success(), "{:?}", decoded(&out));
    let js = fs::read_to_string(output).unwrap();
    let template = include_str!("../skill/good-cat/assets/good-cat.js");
    let expected = template
        .replace(
            "const CHATGPT_COUNTRIES = [\"US\", \"JP\", \"SG\", \"TW\"];",
            "const CHATGPT_COUNTRIES = [\"US\"];",
        )
        .replace(
            "const CLAUDE_COUNTRIES = [\"US\", \"JP\", \"SG\", \"TW\"];",
            "const CLAUDE_COUNTRIES = [\"JP\"];",
        );
    assert_eq!(js, expected);
}
#[test]
fn failed_render_never_replaces_existing_artifact() {
    let dir = tempfile::tempdir().unwrap();
    let report = dir.path().join("report.json");
    let selection = dir.path().join("selection.json");
    let output = dir.path().join("out.js");
    fs::write(&report, json!({"reports":[]}).to_string()).unwrap();
    fs::write(
        &selection,
        json!({"chatgpt":["US"],"claude":["JP"]}).to_string(),
    )
    .unwrap();
    fs::write(&output, "previous").unwrap();
    let out = run(&[
        "render",
        "--report",
        report.to_str().unwrap(),
        "--selection",
        selection.to_str().unwrap(),
        "--output",
        output.to_str().unwrap(),
    ]);
    assert!(!out.status.success());
    assert_eq!(fs::read_to_string(output).unwrap(), "previous");
}
#[test]
fn invalid_controller_port_rejected_before_any_mutation() {
    let out = run(&["controller", "enable", "--port", "12345", "--apply"]);
    assert!(!out.status.success());
}
#[test]
fn unresolved_install_journal_blocks_mutations_before_process_control() {
    let (dir, prefs) = fixture();
    let script = dir.path().join("generated.js");
    fs::write(&script, "function main(c){return c;}").unwrap();
    fs::write(dir.path().join(".good-cat-pending-install.json"), "{}").unwrap();
    let before = fs::read(&prefs).unwrap();
    for extra in [
        vec!["controller", "enable", "--apply"],
        vec!["install", "--apply", "--script", script.to_str().unwrap()],
    ] {
        let mut args = args_for(dir.path(), &prefs);
        args.extend(extra.iter().map(|s| s.to_string()));
        let out = run_owned(&args);
        assert!(!out.status.success());
        let value = decoded(&out);
        assert_eq!(value["status"], "error");
        assert!(value["error"].as_str().unwrap().contains("recover"));
    }
    assert_eq!(fs::read(prefs).unwrap(), before);
}

#[test]
fn speedtest_against_fake_api_all_nodes_per_url() {
    let server = TcpListener::bind("127.0.0.1:0").unwrap();
    let address = server.local_addr().unwrap();
    let thread = thread::spawn(move || {
        // version + global nodes + providers + four nodes * three rounds.
        for _ in 0..15 {
            let (mut stream, _) = server.accept().unwrap();
            let mut buffer = [0u8; 16384];
            let length = stream.read(&mut buffer).unwrap();
            let request = String::from_utf8_lossy(&buffer[..length]);
            assert!(request
                .to_ascii_lowercase()
                .contains("authorization: bearer fixture-secret"));
            let path = request
                .lines()
                .next()
                .unwrap()
                .split_whitespace()
                .nth(1)
                .unwrap();
            let payload = if path == "/version" {
                json!({"version":"fixture"})
            } else if path == "/proxies" {
                json!({"proxies":{"fast":{"type":"Trojan"},"middle":{"type":"Trojan"},"boundary":{"type":"Trojan"},"failed":{"type":"Trojan"},"DIRECT":{"type":"Direct"},"PASS-RULE":{"type":"PassRule"}}})
            } else if path == "/providers/proxies" {
                json!({"providers":{}})
            } else if path.starts_with("/proxies/fast/") {
                json!({"delay":10})
            } else if path.starts_with("/proxies/middle/") {
                json!({"delay":550})
            } else if path.starts_with("/proxies/boundary/") {
                json!({"delay":600})
            } else {
                json!({"delay":0})
            };
            let body = payload.to_string();
            write!(stream,"HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",body.len(),body).unwrap();
        }
    });
    let dir = tempfile::tempdir().unwrap();
    let out = Command::new(env!("CARGO_BIN_EXE_good-cat"))
        .current_dir(dir.path())
        .env("MIHOMO_SECRET", "fixture-secret")
        .args([
            "speedtest",
            "--controller",
            &format!("http://{address}"),
            "--url",
            "https://fixture.test/204",
        ])
        .output()
        .unwrap();
    assert!(out.status.success(), "{:?}", decoded(&out));
    thread.join().unwrap();
    let result = decoded(&out);
    let workspace = PathBuf::from(result["workspace"].as_str().unwrap());
    assert!(workspace.starts_with(std::env::temp_dir().canonicalize().unwrap()));
    assert_eq!(fs::read_dir(dir.path()).unwrap().count(), 0);
    let report: Value =
        serde_json::from_slice(&fs::read(result["output"].as_str().unwrap()).unwrap()).unwrap();
    fs::remove_dir_all(workspace).unwrap();
    assert_eq!(report["rounds"], 3);
    assert_eq!(report["aggregation"], "median_all_success");
    assert_eq!(
        report["reports"][0]["results"][0]["samples"]
            .as_array()
            .unwrap()
            .len(),
        3
    );
    assert_eq!(report["reports"][0]["tested"], 4);
    assert_eq!(report["reports"][0]["failed"], 1);
    assert_eq!(report["reports"][0]["eligible"][0]["delay_ms"], 550.0);
    assert_eq!(report["reports"][0]["eligible"][1]["delay_ms"], 10.0);
}
