use rusqlite::Connection;
use serde_json::{json, Value};
use std::{
    fs,
    path::{Path, PathBuf},
    process::Command,
};

fn fixture(label: &str) -> (tempfile::TempDir, PathBuf) {
    let dir = tempfile::tempdir().unwrap();
    let db = Connection::open(dir.path().join("database.sqlite")).unwrap();
    db.execute_batch("CREATE TABLE scripts(id INTEGER PRIMARY KEY,label TEXT NOT NULL,last_update_time INTEGER NOT NULL,url TEXT,\"order\" INTEGER); CREATE TABLE profiles(id INTEGER PRIMARY KEY,extend_type TEXT,script_id INTEGER); INSERT INTO profiles VALUES(123,'script',7);").unwrap();
    db.execute("INSERT INTO scripts VALUES(7,?1,0,NULL,0)", [label])
        .unwrap();
    fs::create_dir(dir.path().join("scripts")).unwrap();
    fs::write(
        dir.path().join("scripts/7.js"),
        "const CLAUDE_COUNTRIES = [\"TW\", \"SG\", \"JP\"];\n// untouched\n",
    )
    .unwrap();
    let prefs = dir.path().join("preferences.json");
    fs::write(
        &prefs,
        json!({"flutter.config":json!({"currentProfileId":123,"patchClashConfig":{"external-controller":"","secret":""}}).to_string()}).to_string(),
    )
    .unwrap();
    (dir, prefs)
}

fn invoke(support: &Path, prefs: &Path, work: &Path, args: &[&str]) -> (bool, Value) {
    let out = Command::new(env!("CARGO_BIN_EXE_good-cat"))
        .args(["--json", "--support-dir"])
        .arg(support.canonicalize().unwrap())
        .arg("--preferences")
        .arg(prefs)
        .arg("--workspace")
        .arg(work)
        // If a guard accidentally reaches process control it must fail, not stop a live app.
        .arg("--app")
        .arg(work.join("nonexistent-FlClash.app"))
        .args(args)
        .output()
        .unwrap();
    (
        out.status.success(),
        serde_json::from_slice(&out.stdout).unwrap(),
    )
}

#[test]
fn managed_export_and_plan_are_offline_and_binding_guard_precedes_stop() {
    let (dir, prefs) = fixture("good-cat-fixture");
    let work = good_cat::workspace::create().unwrap();
    let before = fs::read(dir.path().join("database.sqlite")).unwrap();
    let source = fs::read(dir.path().join("scripts/7.js")).unwrap();
    let inspected = Command::new(env!("CARGO_BIN_EXE_good-cat"))
        .args(["--json", "--support-dir"])
        .arg(dir.path())
        .arg("--preferences")
        .arg(&prefs)
        .args(["inspect"])
        .output()
        .unwrap();
    assert!(inspected.status.success());
    let inspected: Value = serde_json::from_slice(&inspected.stdout).unwrap();
    assert_eq!(inspected["current_script"]["managed"], true);
    assert_eq!(
        inspected["current_script"]["script_label"],
        "good-cat-fixture"
    );
    let (ok, exported) = invoke(dir.path(), &prefs, &work, &["current-script"]);
    assert!(ok, "{exported}");
    assert_eq!(exported["label"], "good-cat-fixture");
    assert_eq!(fs::read(work.join("current-good-cat.js")).unwrap(), source);
    fs::write(
        work.join("good-cat.generated.js"),
        "const CLAUDE_COUNTRIES = [\"JP\"];\n// untouched\n",
    )
    .unwrap();
    let (ok, plan) = invoke(dir.path(), &prefs, &work, &["update"]);
    assert!(ok, "{plan}");
    assert_eq!(plan["status"], "confirmation_required");
    let diff = fs::read_to_string(plan["diff_path"].as_str().unwrap()).unwrap();
    assert!(diff.contains("-const CLAUDE_COUNTRIES"));
    assert_eq!(
        fs::read(dir.path().join("database.sqlite")).unwrap(),
        before
    );
    assert_eq!(fs::read(dir.path().join("scripts/7.js")).unwrap(), source);
    let db = Connection::open(dir.path().join("database.sqlite")).unwrap();
    db.execute("UPDATE profiles SET script_id=NULL WHERE id=123", [])
        .unwrap();
    for args in [vec!["update"], vec!["update", "--apply"]] {
        let (ok, error) = invoke(dir.path(), &prefs, &work, &args);
        assert!(!ok);
        assert!(
            error["error"].as_str().unwrap().contains("bound script"),
            "{error}"
        );
    }
    fs::remove_dir_all(work).unwrap();
}

#[test]
fn unmanaged_and_legacy_names_never_export_or_install() {
    for label in [
        "good-cat",
        "user-private",
        "good-cat-",
        "good-cat-unsafe/name",
    ] {
        let (dir, prefs) = fixture(label);
        let work = good_cat::workspace::create().unwrap();
        let (ok, error) = invoke(dir.path(), &prefs, &work, &["current-script"]);
        assert!(!ok);
        assert!(
            error["error"].as_str().unwrap().contains("not managed"),
            "{error}"
        );
        assert!(!work.join("current-good-cat.js").exists());
        assert!(!work.join(".good-cat-current-script.json").exists());
        // Validation must happen before script IO or any process disruption.
        let (ok, error) = invoke(
            dir.path(),
            &prefs,
            &work,
            &["install", "--name", label, "--apply"],
        );
        assert!(!ok);
        assert!(
            error["error"].as_str().unwrap().contains("脚本名称"),
            "{error}"
        );
        fs::remove_dir_all(work).unwrap();
    }
}

#[test]
fn update_requires_workspace_and_refuses_pending_journals() {
    let (dir, prefs) = fixture("good-cat-fixture");
    let work = good_cat::workspace::create().unwrap();
    for (journal, recovery) in [
        (".good-cat-pending-install.json", "recover"),
        (".good-cat-pending-controller.json", "controller restore"),
    ] {
        fs::write(dir.path().join(journal), "{}").unwrap();
        for args in [
            vec!["current-script"],
            vec!["update"],
            vec!["update", "--apply"],
        ] {
            let (ok, error) = invoke(dir.path(), &prefs, &work, &args);
            assert!(!ok);
            assert!(
                error["error"].as_str().unwrap().contains(recovery),
                "{error}"
            );
        }
        fs::remove_file(dir.path().join(journal)).unwrap();
    }
    let out = Command::new(env!("CARGO_BIN_EXE_good-cat"))
        .arg("--support-dir")
        .arg(dir.path())
        .arg("current-script")
        .output()
        .unwrap();
    assert!(!out.status.success());
    let error: Value = serde_json::from_slice(&out.stdout).unwrap();
    assert!(error["error"].as_str().unwrap().contains("--workspace"));
    fs::remove_dir_all(work).unwrap();
}

#[test]
fn changed_candidate_requires_new_preview_before_process_control() {
    let (dir, prefs) = fixture("good-cat-fixture");
    let work = good_cat::workspace::create().unwrap();
    let original = fs::read(dir.path().join("scripts/7.js")).unwrap();
    assert!(invoke(dir.path(), &prefs, &work, &["current-script"]).0);
    let candidate = work.join("good-cat.generated.js");
    fs::write(
        &candidate,
        "const CLAUDE_COUNTRIES = [\"JP\"];\n// untouched\n",
    )
    .unwrap();
    assert!(invoke(dir.path(), &prefs, &work, &["update"]).0);
    let approved_diff = fs::read(work.join("update.diff")).unwrap();
    fs::write(
        &candidate,
        "const CLAUDE_COUNTRIES = [\"TW\"];\n// changed after confirmation\n",
    )
    .unwrap();
    let (ok, result) = invoke(dir.path(), &prefs, &work, &["update", "--apply"]);
    assert!(!ok);
    assert!(
        result["error"].as_str().unwrap().contains("changed"),
        "{result}"
    );
    assert_eq!(fs::read(work.join("update.diff")).unwrap(), approved_diff);
    assert_eq!(fs::read(dir.path().join("scripts/7.js")).unwrap(), original);
    fs::remove_dir_all(work).unwrap();
}

#[test]
fn unchanged_update_does_not_enter_process_control() {
    let (dir, prefs) = fixture("good-cat-fixture");
    let work = good_cat::workspace::create().unwrap();
    assert!(invoke(dir.path(), &prefs, &work, &["current-script"]).0);
    fs::copy(
        work.join("current-good-cat.js"),
        work.join("good-cat.generated.js"),
    )
    .unwrap();
    assert!(invoke(dir.path(), &prefs, &work, &["update"]).0);
    // invoke supplies a nonexistent app path, so success proves no restart was attempted.
    let (ok, result) = invoke(dir.path(), &prefs, &work, &["update", "--apply"]);
    assert!(ok, "{result}");
    assert_eq!(result["changed"], false);
    assert_eq!(result["restarted"], false);
    fs::remove_dir_all(work).unwrap();
}

#[test]
fn install_cannot_overwrite_another_subscriptions_managed_script() {
    let (dir, prefs) = fixture("good-cat-current");
    let work = good_cat::workspace::create().unwrap();
    let db = Connection::open(dir.path().join("database.sqlite")).unwrap();
    db.execute_batch("INSERT INTO scripts VALUES(8,'good-cat-other',0,NULL,0); INSERT INTO profiles VALUES(456,'script',8);").unwrap();
    fs::write(dir.path().join("scripts/8.js"), "// other subscription").unwrap();
    fs::write(work.join("good-cat.generated.js"), "// candidate").unwrap();
    let (ok, result) = invoke(
        dir.path(),
        &prefs,
        &work,
        &[
            "install",
            "--name",
            "good-cat-other",
            "--register-only",
            "--apply",
        ],
    );
    assert!(!ok);
    assert!(
        result["error"].as_str().unwrap().contains("current-script"),
        "{result}"
    );
    assert_eq!(
        fs::read_to_string(dir.path().join("scripts/8.js")).unwrap(),
        "// other subscription"
    );
    assert_eq!(
        db.query_row("SELECT script_id FROM profiles WHERE id=123", [], |row| row
            .get::<_, i64>(0))
            .unwrap(),
        7
    );
    fs::remove_dir_all(work).unwrap();
}
