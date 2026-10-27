//! Current-script updates only. Apply requires a stopped app and lifecycle lock.
use crate::{install, platform};
use anyhow::{ensure, Context, Result};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use similar::TextDiff;
use std::{
    fs,
    path::{Path, PathBuf},
};

const SOURCE: &str = "current-good-cat.js";
const BASELINE: &str = ".good-cat-current-script.json";
const DIFF: &str = "update.diff";
const PLAN: &str = ".good-cat-update-plan.json";

#[derive(Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
struct Approval {
    support: PathBuf,
    baseline: Baseline,
    candidate_path: PathBuf,
    candidate_sha256: String,
}

#[derive(Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
struct Baseline {
    profile_id: i64,
    script_id: i64,
    label: String,
    sha256: String,
}

fn hash(bytes: &[u8]) -> String {
    format!("{:x}", Sha256::digest(bytes))
}

// Check every component, not just the leaf: canonicalization alone hides symlinks.
fn checked(path: &Path, directory: bool) -> Result<PathBuf> {
    let absolute = if path.is_absolute() {
        path.to_owned()
    } else {
        std::env::current_dir()?.join(path)
    };
    for ancestor in absolute.ancestors() {
        let meta = fs::symlink_metadata(ancestor).context("checking update path")?;
        ensure!(
            !meta.file_type().is_symlink(),
            "symlink update paths are not allowed"
        );
    }
    let meta = fs::metadata(&absolute)?;
    ensure!(
        if directory {
            meta.is_dir()
        } else {
            meta.is_file()
        },
        "unexpected update file type"
    );
    Ok(absolute.canonicalize()?)
}

fn workspace_path(workspace: &Path, support: &Path) -> Result<PathBuf> {
    let workspace = checked(workspace, true)?;
    let support = checked(support, true)?;
    ensure!(
        !workspace.starts_with(&support),
        "workspace must be outside the application support directory"
    );
    // Private artifacts must never follow pre-existing symlinks or special files.
    for name in [SOURCE, BASELINE, DIFF, PLAN] {
        let path = workspace.join(name);
        match fs::symlink_metadata(&path) {
            Ok(_) => {
                checked(&path, false)?;
            }
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => (),
            Err(e) => return Err(e.into()),
        }
    }
    Ok(workspace)
}

fn same_file(a: &Path, b: &Path) -> Result<bool> {
    if a == b {
        return Ok(true);
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        let a = fs::metadata(a)?;
        let b = fs::metadata(b)?;
        Ok(a.dev() == b.dev() && a.ino() == b.ino())
    }
    #[cfg(windows)]
    {
        use std::os::windows::io::AsRawHandle;
        #[link(name = "kernel32")]
        extern "system" {
            fn GetFileInformationByHandle(handle: *mut std::ffi::c_void, info: *mut u32) -> i32;
        }
        fn identity(path: &Path) -> Result<(u32, u32, u32)> {
            let file = fs::File::open(path)?;
            // BY_HANDLE_FILE_INFORMATION consists of thirteen DWORDs.
            let mut info = [0u32; 13];
            ensure!(
                unsafe { GetFileInformationByHandle(file.as_raw_handle(), info.as_mut_ptr()) } != 0,
                "cannot verify file identity"
            );
            Ok((info[7], info[11], info[12]))
        }
        Ok(identity(a)? == identity(b)?)
    }
    #[cfg(not(any(unix, windows)))]
    anyhow::bail!("file identity validation is unsupported on this platform")
}

fn current(support: &Path, profile_id: i64) -> Result<(Baseline, PathBuf, String)> {
    let support = checked(support, true)?;
    let (_snapshot, db) = install::snapshot(&support)?;
    let (mode, id): (Option<String>, Option<i64>) = db
        .query_row(
            "SELECT extend_type,script_id FROM profiles WHERE id=?1",
            [profile_id],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .context("current profile does not exist")?;
    ensure!(
        mode.as_deref() == Some("script"),
        "current profile is not in script mode"
    );
    let id = id.context("current profile has no bound script")?;
    let label: String = db
        .query_row("SELECT label FROM scripts WHERE id=?1", [id], |r| r.get(0))
        .context("bound script does not exist")?;
    // Do not inspect or read a user's script source, even when it is readable.
    ensure!(
        install::is_managed_name(&label),
        "current script is not managed by good-cat"
    );
    let count: i64 = db.query_row(
        "SELECT COUNT(*) FROM scripts WHERE label=?1",
        [&label],
        |r| r.get(0),
    )?;
    ensure!(count == 1, "managed script label is ambiguous");
    let refs: i64 = db.query_row(
        "SELECT COUNT(*) FROM profiles WHERE script_id=?1",
        [id],
        |r| r.get(0),
    )?;
    ensure!(refs == 1, "shared script updates are not allowed");
    match fs::symlink_metadata(install::pending_install_path(&support)) {
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => (),
        _ => anyhow::bail!("pending install requires recovery"),
    }
    let source = String::from_utf8(install::read_managed_source(&support, id)?)
        .context("reading managed UTF-8 script")?;
    let baseline = Baseline {
        profile_id,
        script_id: id,
        label,
        sha256: hash(source.as_bytes()),
    };
    Ok((
        baseline,
        support.join("scripts").join(format!("{id}.js")),
        source,
    ))
}

/// Export only the current, uniquely bound managed script into private artifacts.
pub fn export_current(support: &Path, profile_id: i64, workspace: &Path) -> Result<Value> {
    let workspace = workspace_path(workspace, support)?;
    let (baseline, installed, source) = current(support, profile_id)?;
    for name in [SOURCE, BASELINE, DIFF, PLAN] {
        let artifact = workspace.join(name);
        if artifact.exists() {
            ensure!(
                !same_file(&artifact, &installed)?,
                "workspace artifact aliases installed script"
            );
        }
    }
    if workspace.join(PLAN).exists() {
        fs::remove_file(workspace.join(PLAN))?;
    }
    platform::private_write(&workspace.join(SOURCE), source.as_bytes())?;
    platform::private_write(&workspace.join(BASELINE), &serde_json::to_vec(&baseline)?)?;
    Ok(
        json!({"status":"ok", "operation":"export_current", "profile_id": profile_id,
        "script_id":baseline.script_id, "label":baseline.label, "sha256":baseline.sha256,
        "source_path":crate::local::display(&workspace.join(SOURCE)),
        "source_path_absolute":workspace.join(SOURCE),
        "baseline_path":crate::local::display(&workspace.join(BASELINE)),
        "baseline_path_absolute":workspace.join(BASELINE)}),
    )
}

struct Validated {
    baseline: Baseline,
    source: String,
    candidate: String,
    workspace: PathBuf,
    candidate_path: PathBuf,
}
fn validate(support: &Path, profile_id: i64, workspace: &Path, script: &Path) -> Result<Validated> {
    let workspace = workspace_path(workspace, support)?;
    let baseline_path =
        checked(&workspace.join(BASELINE), false).context("export current script first")?;
    let baseline: Baseline = serde_json::from_slice(&fs::read(baseline_path)?)
        .context("invalid current-script baseline")?;
    ensure!(
        baseline.profile_id == profile_id,
        "baseline profile does not match"
    );
    let (now, installed, source) = current(support, profile_id)?;
    for name in [SOURCE, BASELINE, DIFF, PLAN] {
        let artifact = workspace.join(name);
        if artifact.exists() {
            ensure!(
                !same_file(&artifact, &installed)?,
                "workspace artifact aliases installed script"
            );
        }
    }
    ensure!(
        baseline == now,
        "current script changed; export again before updating"
    );
    let candidate_path = checked(script, false)?;
    ensure!(
        !same_file(&candidate_path, &installed)?,
        "candidate must not be the installed script"
    );
    for name in [SOURCE, BASELINE, DIFF, PLAN] {
        let artifact = workspace.join(name);
        if artifact.exists() {
            ensure!(
                !same_file(&candidate_path, &artifact)?,
                "candidate must not alias workspace artifacts"
            );
        }
    }
    let candidate =
        fs::read_to_string(&candidate_path).context("reading candidate UTF-8 script")?;
    ensure!(
        !candidate.trim().is_empty(),
        "candidate JavaScript must not be empty"
    );
    Ok(Validated {
        baseline,
        source,
        candidate,
        workspace,
        candidate_path,
    })
}

/// Build a private unified diff without changing application files or metadata.
pub fn plan(support: &Path, profile_id: i64, workspace: &Path, script: &Path) -> Result<Value> {
    let v = validate(support, profile_id, workspace, script)?;
    let diff = TextDiff::from_lines(&v.source, &v.candidate)
        .unified_diff()
        .header("current-good-cat.js", "candidate.js")
        .to_string();
    let diff_path = v.workspace.join(DIFF);
    platform::private_write(&diff_path, diff.as_bytes())?;
    let approval = Approval {
        support: checked(support, true)?,
        baseline: Baseline {
            profile_id,
            script_id: v.baseline.script_id,
            label: v.baseline.label.clone(),
            sha256: v.baseline.sha256.clone(),
        },
        candidate_path: v.candidate_path.clone(),
        candidate_sha256: hash(v.candidate.as_bytes()),
    };
    platform::private_write(&v.workspace.join(PLAN), &serde_json::to_vec(&approval)?)?;
    Ok(
        json!({"status":"confirmation_required", "operation":"update", "profile_id":profile_id,
        "script_id":v.baseline.script_id, "label":v.baseline.label,
        "source_sha256":v.baseline.sha256, "destination_sha256":hash(v.candidate.as_bytes()),
        "changed":v.source != v.candidate,
        "diff_path":crate::local::display(&diff_path),
        "diff_path_absolute":diff_path}),
    )
}

fn approved(support: &Path, profile_id: i64, workspace: &Path, script: &Path) -> Result<Validated> {
    let v = validate(support, profile_id, workspace, script)?;
    let approval: Approval = serde_json::from_slice(&fs::read(
        checked(&v.workspace.join(PLAN), false).context("plan update before applying")?,
    )?)?;
    ensure!(
        approval.support == checked(support, true)?
            && approval.baseline == v.baseline
            && approval.candidate_path == v.candidate_path
            && approval.candidate_sha256 == hash(v.candidate.as_bytes()),
        "approved update changed; plan again before applying"
    );
    Ok(v)
}

/// Read-only approval preflight; apply repeats this check under the lifecycle lock.
pub fn check_plan(
    support: &Path,
    profile_id: i64,
    workspace: &Path,
    script: &Path,
) -> Result<Value> {
    let v = approved(support, profile_id, workspace, script)?;
    Ok(
        json!({"status":"ok", "profile_id":profile_id, "script_id":v.baseline.script_id,
        "source_sha256":v.baseline.sha256, "destination_sha256":hash(v.candidate.as_bytes())}),
    )
}

/// Caller must hold lifecycle locks and stop FlClash before calling this function.
pub fn apply(support: &Path, profile_id: i64, workspace: &Path, script: &Path) -> Result<Value> {
    let v = approved(support, profile_id, workspace, script)?;
    let changed = v.source != v.candidate;
    let mut installation = Value::Null;
    if changed {
        // Register the validated bytes, not a mutable candidate pathname.
        let private = tempfile::Builder::new()
            .prefix("good-cat-update-")
            .tempdir_in(crate::local::temporary_root())?;
        let candidate = private.path().join("candidate.js");
        platform::private_write(&candidate, v.candidate.as_bytes())?;
        let (now, _, _) = current(support, profile_id)?;
        ensure!(
            now == v.baseline,
            "current script changed; export again before updating"
        );
        installation = install::register_current(
            support,
            &candidate,
            install::ExpectedTarget {
                profile_id,
                script_id: v.baseline.script_id,
                label: v.baseline.label.clone(),
                source_sha256: v.baseline.sha256.clone(),
            },
        )?;
        ensure!(
            installation["id"].as_i64() == Some(v.baseline.script_id),
            "registered script ID changed unexpectedly"
        );
    }
    // Leave the old baseline untouched: after a write its old hash rejects replay.
    // No fallible private-artifact cleanup after committing the real installation.
    Ok(
        json!({"status":"ok", "operation":"update", "profile_id":profile_id,
        "script_id":v.baseline.script_id, "label":v.baseline.label, "changed":changed,
        "source_sha256":v.baseline.sha256, "destination_sha256":hash(v.candidate.as_bytes()),
        "export_required":changed, "installation":installation}),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use rusqlite::Connection;
    const OLD: &str = "function main(c) {\n  c.keep = true;\n  return c;\n}\n";
    const NEW: &str = "function main(c) {\n  c.keep = false;\n  return c;\n}\n";

    struct Fixture {
        _dir: tempfile::TempDir,
        support: PathBuf,
        workspace: PathBuf,
        candidate: PathBuf,
    }
    impl Fixture {
        fn new() -> Self {
            let dir = tempfile::tempdir().unwrap();
            let root = dir.path().canonicalize().unwrap();
            let support = root.join("support");
            let workspace = root.join("workspace");
            fs::create_dir(&support).unwrap();
            fs::create_dir(&workspace).unwrap();
            fs::create_dir(support.join("scripts")).unwrap();
            let db = Connection::open(support.join("database.sqlite")).unwrap();
            db.execute_batch("CREATE TABLE scripts(id INTEGER PRIMARY KEY,label TEXT NOT NULL,last_update_time INTEGER NOT NULL,url TEXT,\"order\" INTEGER);
                CREATE TABLE profiles(id INTEGER PRIMARY KEY,extend_type TEXT,script_id INTEGER,selected_map TEXT);
                INSERT INTO scripts VALUES(1,'good-cat-test',10,'https://secret.invalid',9);
                INSERT INTO profiles VALUES(7,'script',1,'{\"🧠 Claude\":\"pinned-node\"}');").unwrap();
            fs::write(support.join("scripts/1.js"), OLD).unwrap();
            let candidate = root.join("candidate.js");
            fs::write(&candidate, NEW).unwrap();
            Self {
                _dir: dir,
                support,
                workspace,
                candidate,
            }
        }
        fn sql(&self, sql: &str) {
            Connection::open(self.support.join("database.sqlite"))
                .unwrap()
                .execute_batch(sql)
                .unwrap();
        }
        fn export(&self) -> Value {
            export_current(&self.support, 7, &self.workspace).unwrap()
        }
        fn plan(&self) -> Result<Value> {
            plan(&self.support, 7, &self.workspace, &self.candidate)
        }
        fn apply(&self) -> Result<Value> {
            apply(&self.support, 7, &self.workspace, &self.candidate)
        }
    }

    #[test]
    fn export_plan_partial_update_and_stale_replay() {
        let f = Fixture::new();
        let db_before = fs::read(f.support.join("database.sqlite")).unwrap();
        let exported = f.export();
        assert!(!exported.to_string().contains("secret.invalid"));
        assert!(!exported.to_string().contains("function main"));
        assert_eq!(fs::read_to_string(f.workspace.join(SOURCE)).unwrap(), OLD);
        assert_eq!(f.plan().unwrap()["status"], "confirmation_required");
        let diff = fs::read_to_string(f.workspace.join(DIFF)).unwrap();
        assert!(diff.contains("-  c.keep = true;"));
        assert!(diff.contains("+  c.keep = false;"));
        assert_eq!(
            fs::read(f.support.join("database.sqlite")).unwrap(),
            db_before
        );
        assert_eq!(
            fs::read_to_string(f.support.join("scripts/1.js")).unwrap(),
            OLD
        );
        let result = f.apply().unwrap();
        assert_eq!(result["script_id"], 1);
        assert_eq!(result["changed"], true);
        assert_eq!(
            fs::read_to_string(f.support.join("scripts/1.js")).unwrap(),
            NEW
        );
        let db = Connection::open(f.support.join("database.sqlite")).unwrap();
        let row: (i64, String, i64) = db
            .query_row("SELECT script_id,extend_type,id FROM profiles", [], |r| {
                Ok((r.get(0)?, r.get(1)?, r.get(2)?))
            })
            .unwrap();
        assert_eq!(row, (1, "script".into(), 7));
        let pinned: String = db
            .query_row("SELECT selected_map FROM profiles WHERE id=7", [], |r| {
                r.get(0)
            })
            .unwrap();
        assert_eq!(pinned, "{\"🧠 Claude\":\"pinned-node\"}");
        let row: (String, i64) = db
            .query_row("SELECT url,\"order\" FROM scripts", [], |r| {
                Ok((r.get(0)?, r.get(1)?))
            })
            .unwrap();
        assert_eq!(row, ("https://secret.invalid".into(), 9));
        assert!(f.apply().is_err());
        f.export();
        assert!(f.apply().is_err());
        f.plan().unwrap();
        assert_eq!(f.apply().unwrap()["changed"], false);
    }

    #[test]
    fn approval_pins_candidate_path_bytes_and_export_generation() {
        let f = Fixture::new();
        f.export();
        assert!(check_plan(&f.support, 7, &f.workspace, &f.candidate).is_err());
        assert!(f.apply().is_err());
        f.plan().unwrap();
        check_plan(&f.support, 7, &f.workspace, &f.candidate).unwrap();
        let alias = f.candidate.with_file_name("other.js");
        fs::write(&alias, NEW).unwrap();
        assert!(apply(&f.support, 7, &f.workspace, &alias).is_err());
        fs::write(&f.candidate, "// unapproved bytes").unwrap();
        assert!(check_plan(&f.support, 7, &f.workspace, &f.candidate).is_err());
        assert!(f.apply().is_err());
        assert_eq!(
            fs::read_to_string(f.support.join("scripts/1.js")).unwrap(),
            OLD
        );
        fs::write(&f.candidate, NEW).unwrap();
        f.export();
        assert!(!f.workspace.join(PLAN).exists());
        assert!(f.apply().is_err());
    }

    #[test]
    fn approval_binds_support_and_private_plan() {
        let f = Fixture::new();
        let other = Fixture::new();
        f.export();
        f.plan().unwrap();
        assert!(check_plan(&other.support, 7, &f.workspace, &f.candidate).is_err());
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            assert_eq!(
                fs::metadata(f.workspace.join(PLAN))
                    .unwrap()
                    .permissions()
                    .mode()
                    & 0o777,
                0o600
            );
        }
        let bytes = fs::read(f.workspace.join(PLAN)).unwrap();
        assert!(!String::from_utf8(bytes).unwrap().contains("secret.invalid"));
        assert!(plan(&f.support, 7, &f.workspace, &f.workspace.join(PLAN)).is_err());
    }

    #[test]
    fn inactive_profile_references_also_prohibit_update() {
        let f = Fixture::new();
        f.export();
        f.plan().unwrap();
        f.sql("INSERT INTO profiles VALUES(8,'none',1,NULL)");
        assert!(check_plan(&f.support, 7, &f.workspace, &f.candidate).is_err());
        assert!(f.apply().is_err());
    }

    #[test]
    fn missing_baseline_and_protected_profiles_never_export_source() {
        let f = Fixture::new();
        assert!(f.plan().is_err());
        assert!(f.apply().is_err());
        for sql in [
            "UPDATE scripts SET label='good-cat'",
            "UPDATE scripts SET label='user-script'",
            "UPDATE profiles SET extend_type='none'",
            "UPDATE profiles SET extend_type='script',script_id=NULL",
            "UPDATE profiles SET script_id=99",
        ] {
            let f = Fixture::new();
            f.sql(sql);
            assert!(export_current(&f.support, 7, &f.workspace).is_err());
            assert!(!f.workspace.join(SOURCE).exists());
            assert!(!f.workspace.join(BASELINE).exists());
        }
        assert!(export_current(&f.support, 99, &f.workspace).is_err());
    }

    #[test]
    fn stale_hash_binding_label_and_shared_refs_fail_closed() {
        for sql in [
            "UPDATE scripts SET label='good-cat-renamed'",
            "UPDATE scripts SET label='user-script'",
            "INSERT INTO scripts VALUES(2,'good-cat-other',10,NULL,0); UPDATE profiles SET script_id=2",
            "UPDATE profiles SET extend_type='none'",
            "INSERT INTO profiles VALUES(8,'script',1,NULL)",
        ] {
            let f = Fixture::new();
            f.export();
            f.sql(sql);
            assert!(f.plan().is_err());
            assert!(f.apply().is_err());
            assert_eq!(fs::read_to_string(f.support.join("scripts/1.js")).unwrap(), OLD);
        }
        let f = Fixture::new();
        f.export();
        fs::write(f.support.join("scripts/1.js"), "external edit").unwrap();
        assert!(f.plan().is_err());
        assert!(f.apply().is_err());
    }

    #[test]
    fn snapshot_preserves_live_wal_and_profile_guard() {
        let f = Fixture::new();
        let db = Connection::open(f.support.join("database.sqlite")).unwrap();
        db.execute_batch("PRAGMA journal_mode=WAL; UPDATE scripts SET last_update_time=11;")
            .unwrap();
        let files = [
            "database.sqlite",
            "database.sqlite-wal",
            "database.sqlite-shm",
        ];
        let before: Vec<_> = files
            .iter()
            .map(|p| fs::read(f.support.join(p)).unwrap())
            .collect();
        f.export();
        f.plan().unwrap();
        for (path, bytes) in files.iter().zip(before) {
            assert_eq!(fs::read(f.support.join(path)).unwrap(), bytes);
        }
        assert!(plan(&f.support, 8, &f.workspace, &f.candidate).is_err());
    }

    #[cfg(unix)]
    #[test]
    fn user_source_is_not_opened_and_source_symlinks_are_rejected() {
        use std::os::unix::fs::{symlink, PermissionsExt};
        let f = Fixture::new();
        f.sql("UPDATE scripts SET label='user-script'");
        let source = f.support.join("scripts/1.js");
        fs::set_permissions(&source, fs::Permissions::from_mode(0o000)).unwrap();
        let error = export_current(&f.support, 7, &f.workspace).unwrap_err();
        assert!(error.to_string().contains("not managed"));
        assert!(!f.workspace.join(SOURCE).exists());
        fs::set_permissions(&source, fs::Permissions::from_mode(0o600)).unwrap();
        f.sql("UPDATE scripts SET label='good-cat-test'");
        fs::remove_file(&source).unwrap();
        symlink(&f.candidate, &source).unwrap();
        assert!(export_current(&f.support, 7, &f.workspace).is_err());
        fs::remove_file(&source).unwrap();
        fs::write(&source, OLD).unwrap();
        symlink(
            f.support.join("absent"),
            crate::local::state_file(
                "install.json",
                f.support.join(".good-cat-pending-install.json"),
            ),
        )
        .unwrap();
        assert!(export_current(&f.support, 7, &f.workspace).is_err());
    }

    #[test]
    fn candidate_and_artifact_safety() {
        let f = Fixture::new();
        f.export();
        for path in [
            f.support.join("scripts/1.js"),
            f.workspace.join(SOURCE),
            f.workspace.join(BASELINE),
        ] {
            assert!(plan(&f.support, 7, &f.workspace, &path).is_err());
        }
        let alias = f.workspace.join("hardlink.js");
        fs::hard_link(f.support.join("scripts/1.js"), &alias).unwrap();
        assert!(plan(&f.support, 7, &f.workspace, &alias).is_err());
        for bytes in [&b" \n\t"[..], &b"\xff"[..]] {
            fs::write(&f.candidate, bytes).unwrap();
            assert!(f.plan().is_err());
        }
        #[cfg(unix)]
        {
            use std::os::unix::fs::symlink;
            fs::remove_file(&f.candidate).unwrap();
            symlink(f.support.join("scripts/1.js"), &f.candidate).unwrap();
            assert!(f.plan().is_err());
            fs::remove_file(f.workspace.join(SOURCE)).unwrap();
            symlink(f.support.join("scripts/1.js"), f.workspace.join(SOURCE)).unwrap();
            assert!(export_current(&f.support, 7, &f.workspace).is_err());
            assert_eq!(
                fs::read_to_string(f.support.join("scripts/1.js")).unwrap(),
                OLD
            );
        }
    }
}
