use rusqlite::Connection;
use serde_json::{json, Value};
use std::{fs, process::Command};

#[test]
fn install_defaults_to_current_subscription_but_register_only_opts_out() {
    let dir = tempfile::tempdir().unwrap();
    let db = Connection::open(dir.path().join("database.sqlite")).unwrap();
    db.execute_batch("CREATE TABLE scripts(id INTEGER PRIMARY KEY,label TEXT NOT NULL,last_update_time INTEGER NOT NULL,url TEXT,\"order\" INTEGER); CREATE TABLE profiles(id INTEGER PRIMARY KEY,extend_type TEXT,script_id INTEGER); INSERT INTO profiles VALUES(369025290967453696,'standard',NULL);").unwrap();
    let prefs = dir.path().join("shared_preferences.json");
    fs::write(&prefs,json!({"flutter.config":json!({"currentProfileId":369025290967453696i64,"patchClashConfig":{"external-controller":"","secret":""}}).to_string()}).to_string()).unwrap();
    let script = dir.path().join("generated.js");
    fs::write(&script, "function main(c){return c;}").unwrap();
    for register_only in [false, true] {
        let mut command = Command::new(env!("CARGO_BIN_EXE_good-cat"));
        command.args([
            "--support-dir",
            dir.path().to_str().unwrap(),
            "--preferences",
            prefs.to_str().unwrap(),
            "install",
            "--name",
            "good-cat-subscription",
            "--script",
            script.to_str().unwrap(),
        ]);
        if register_only {
            command.arg("--register-only");
        }
        let output = command.output().unwrap();
        assert!(output.status.success());
        let plan: Value = serde_json::from_slice(&output.stdout).unwrap();
        assert_eq!(plan["status"], "confirmation_required");
        assert_eq!(
            plan["activate_profile"],
            if register_only {
                Value::Null
            } else {
                json!(369025290967453696i64)
            }
        );
    }
    let mode: String = db
        .query_row("SELECT extend_type FROM profiles", [], |row| row.get(0))
        .unwrap();
    assert_eq!(mode, "standard"); // Preview never modifies the active subscription.
}

#[test]
fn inspect_exposes_selected_subscription_label_without_url_or_secret() {
    let dir = tempfile::tempdir().unwrap();
    let db = Connection::open(dir.path().join("database.sqlite")).unwrap();
    db.execute_batch("CREATE TABLE scripts(id INTEGER PRIMARY KEY,label TEXT,last_update_time INTEGER,url TEXT,\"order\" INTEGER); CREATE TABLE profiles(id INTEGER PRIMARY KEY,extend_type TEXT,script_id INTEGER,label TEXT,url TEXT); INSERT INTO profiles VALUES(123,'standard',NULL,'良心云 · 全球专线套餐','https://secret.invalid/?token=private');").unwrap();
    let prefs = dir.path().join("preferences.json");
    fs::write(&prefs,json!({"flutter.config":json!({"currentProfileId":123,"patchClashConfig":{"external-controller":"","secret":"secret-not-for-output"}}).to_string()}).to_string()).unwrap();
    let result = Command::new(env!("CARGO_BIN_EXE_good-cat"))
        .args([
            "--support-dir",
            dir.path().to_str().unwrap(),
            "--preferences",
            prefs.to_str().unwrap(),
            "inspect",
            "--json",
        ])
        .output()
        .unwrap();
    assert!(
        result.status.success(),
        "{}",
        String::from_utf8_lossy(&result.stdout)
    );
    let value: Value = serde_json::from_slice(&result.stdout).unwrap();
    assert_eq!(value["current_profile_label"], "良心云 · 全球专线套餐");
    assert_eq!(
        value["current_script"]["profile_label"],
        "良心云 · 全球专线套餐"
    );
    assert_eq!(value["current_script"]["managed"], false);
    assert!(!String::from_utf8_lossy(&result.stdout).contains("secret.invalid"));
    assert!(!String::from_utf8_lossy(&result.stdout).contains("secret-not-for-output"));

    // The model must explicitly choose the name; no random fallback is generated.
    let script = dir.path().join("candidate.js");
    fs::write(&script, "function main(c){return c;}").unwrap();
    let result = Command::new(env!("CARGO_BIN_EXE_good-cat"))
        .args([
            "--support-dir",
            dir.path().to_str().unwrap(),
            "--preferences",
            prefs.to_str().unwrap(),
            "install",
            "--script",
            script.to_str().unwrap(),
            "--apply",
        ])
        .output()
        .unwrap();
    assert!(!result.status.success());
    let value: Value = serde_json::from_slice(&result.stdout).unwrap();
    assert!(value["error"].as_str().unwrap().contains("--name"));
    assert_eq!(
        db.query_row("SELECT COUNT(*) FROM scripts", [], |row| row
            .get::<_, i64>(0))
            .unwrap(),
        0
    );
}
