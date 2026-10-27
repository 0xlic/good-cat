//! Offline installation only. The caller must stop FlClash and hold lifecycle locks.
use anyhow::{anyhow, bail, ensure, Context, Result};
use rusqlite::{backup::Backup, params, Connection, OpenFlags};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::{
    error::Error,
    fmt, fs,
    io::Write,
    path::{Path, PathBuf},
    time::{Duration, SystemTime, UNIX_EPOCH},
};

const PENDING: &str = ".good-cat-pending-install.json";

/// An install/recovery could not restore consistency. Do not restart FlClash.
#[derive(Debug)]
pub struct RecoveryFailure {
    pub operation: String,
    pub recovery: String,
}
impl fmt::Display for RecoveryFailure {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "{}; recovery failed: {} (keep FlClash stopped)",
            self.operation, self.recovery
        )
    }
}
impl Error for RecoveryFailure {}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Journal {
    version: u32,
    id: i64,
    target: String,
    backup: String,
    had_script: bool,
    phase: Phase,
}
#[derive(Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
enum Phase {
    Prepared,
    Replaced,
}

fn exists(path: &Path) -> Result<bool> {
    match fs::symlink_metadata(path) {
        Ok(_) => Ok(true),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(false),
        Err(e) => Err(e.into()),
    }
}

// Reject symlinks, including dangling ones, rather than following external paths.
fn regular(path: &Path, directory: bool) -> Result<()> {
    let meta =
        fs::symlink_metadata(path).with_context(|| format!("checking {}", path.display()))?;
    ensure!(
        !meta.file_type().is_symlink(),
        "symlink not allowed: {}",
        path.display()
    );
    ensure!(
        if directory {
            meta.is_dir()
        } else {
            meta.is_file()
        },
        "unexpected file type: {}",
        path.display()
    );
    Ok(())
}
fn directory(path: &Path) -> Result<()> {
    if !exists(path)? {
        fs::create_dir(path)?;
    }
    regular(path, true)
}
fn root(support: &Path) -> Result<PathBuf> {
    let support = support
        .canonicalize()
        .context("support directory must exist")?;
    regular(&support, true)?;
    regular(&support.join("database.sqlite"), false)?;
    Ok(support)
}
fn open(support: &Path, readonly: bool) -> Result<Connection> {
    // SQLite can follow sidecar symlinks too.
    for suffix in ["-wal", "-shm", "-journal"] {
        let path = support.join(format!("database.sqlite{suffix}"));
        if exists(&path)? {
            regular(&path, false)?;
        }
    }
    let flags = if readonly {
        OpenFlags::SQLITE_OPEN_READ_ONLY
    } else {
        OpenFlags::SQLITE_OPEN_READ_WRITE
    };
    let db = Connection::open_with_flags(support.join("database.sqlite"), flags)?;
    db.busy_timeout(Duration::from_secs(5))?;
    Ok(db)
}
fn schema(db: &Connection) -> Result<()> {
    for (table, required) in [
        (
            "scripts",
            &["id", "label", "last_update_time", "url", "order"][..],
        ),
        ("profiles", &["id", "extend_type", "script_id"][..]),
    ] {
        let kind: String = db
            .query_row(
                "SELECT type FROM sqlite_master WHERE name=?1",
                [table],
                |r| r.get(0),
            )
            .with_context(|| format!("missing {table} table"))?;
        ensure!(kind == "table", "{table} must be a table");
        let mut stmt = db.prepare(&format!("PRAGMA table_info({table})"))?;
        let columns = stmt
            .query_map([], |r| {
                Ok((
                    r.get::<_, String>(1)?,
                    r.get::<_, String>(2)?,
                    r.get::<_, i64>(5)?,
                ))
            })?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        for column in required {
            ensure!(
                columns.iter().any(|c| c.0 == *column),
                "missing {table}.{column}"
            );
        }
        ensure!(
            columns
                .iter()
                .any(|c| c.0 == "id" && c.1.eq_ignore_ascii_case("INTEGER") && c.2 == 1)
                && columns.iter().filter(|c| c.2 != 0).count() == 1,
            "{table}.id must be an INTEGER primary key"
        );
    }
    Ok(())
}

fn metadata(db: &Connection, name: &str) -> Result<Vec<Value>> {
    let mut stmt = db.prepare(
        "SELECT id,label,last_update_time,url,\"order\" FROM scripts WHERE label=?1 ORDER BY id",
    )?;
    let rows = stmt
        .query_map([name], |r| {
            Ok(
                json!({"id": r.get::<_, i64>(0)?, "label": r.get::<_, String>(1)?,
            "last_update_time": r.get::<_, Option<i64>>(2)?, "url_configured": r.get::<_, Option<String>>(3)?.is_some(),
            "order": r.get::<_, Option<i64>>(4)?}),
            )
        })?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    Ok(rows)
}
fn active(db: &Connection, id: i64) -> Result<Vec<i64>> {
    let mut stmt = db.prepare(
        "SELECT id FROM profiles WHERE extend_type='script' AND script_id=?1 ORDER BY id",
    )?;
    let rows = stmt
        .query_map([id], |r| r.get(0))?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    Ok(rows)
}
fn next_id(db: &Connection, support: &Path) -> Result<i64> {
    let maximum: i64 = db.query_row("SELECT COALESCE(MAX(id),0) FROM scripts", [], |r| r.get(0))?;
    let mut id = maximum.checked_add(1).context("script ID exhausted")?;
    loop {
        let referenced: bool = db.query_row(
            "SELECT EXISTS(SELECT 1 FROM profiles WHERE script_id=?1)",
            [id],
            |r| r.get(0),
        )?;
        if !referenced && !exists(&support.join("scripts").join(format!("{id}.js")))? {
            break;
        }
        id = id.checked_add(1).context("script ID exhausted")?;
    }
    Ok(id)
}

/// Read-only plan, including exact existing metadata and active profile references.
/// Does not create a database, directories, backups, or a journal.
pub fn inspect(support: &Path, name: &str) -> Result<Value> {
    let support = root(support)?;
    if exists(&support.join("scripts"))? {
        regular(&support.join("scripts"), true)?;
    }
    // Never open the live DB with SQLite: even READ_ONLY may write WAL shared
    // memory. Read a private copy (including uncheckpointed WAL) instead. Like
    // any plan, this snapshot may become stale before registration; register
    // revalidates against the stopped application's database.
    let snapshot = tempfile::tempdir()?;
    fs::copy(
        support.join("database.sqlite"),
        snapshot.path().join("database.sqlite"),
    )?;
    for suffix in ["-wal", "-journal"] {
        let sidecar = support.join(format!("database.sqlite{suffix}"));
        if exists(&sidecar)? {
            regular(&sidecar, false)?;
            fs::copy(
                &sidecar,
                snapshot.path().join(format!("database.sqlite{suffix}")),
            )?;
        }
    }
    let db = open(snapshot.path(), false)?;
    schema(&db)?;
    let scripts = metadata(&db, name)?;
    let mut references = Vec::new();
    for script in &scripts {
        let id = script["id"].as_i64().context("invalid script ID")?;
        references.push(json!({"script_id": id, "profile_ids": active(&db, id)?}));
    }
    let id = match scripts.as_slice() {
        [] => Some(next_id(&db, &support)?),
        [row] => row["id"].as_i64(),
        _ => None,
    };
    Ok(
        json!({"name": name, "scripts": scripts, "active_refs": references,
        "planned_id": id, "ambiguous": scripts.len() > 1,
        "pending_recovery": exists(&support.join(PENDING))?}),
    )
}

fn sync_dir(path: &Path) -> Result<()> {
    #[cfg(unix)]
    {
        fs::File::open(path)?.sync_all()?;
        Ok(())
    }
    // Windows does not expose POSIX directory fsync through std. Individual
    // temporary files are flushed before atomic replacement, and the recovery
    // journal is retained until commit. Power-loss directory durability is not
    // guaranteed on Windows; do not claim cross-file crash atomicity.
    #[cfg(not(unix))]
    {
        let _ = path;
        Ok(())
    }
}
fn atomic(path: &Path, bytes: &[u8]) -> Result<()> {
    let parent = path.parent().context("missing parent directory")?;
    let mut temp = tempfile::NamedTempFile::new_in(parent)?;
    temp.write_all(bytes)?;
    temp.as_file().sync_all()?;
    temp.persist(path).map_err(|e| e.error)?;
    sync_dir(parent)
}
fn write_journal(support: &Path, journal: &Journal) -> Result<()> {
    atomic(&support.join(PENDING), &serde_json::to_vec(journal)?)
}
fn remove_journal(support: &Path) -> Result<()> {
    fs::remove_file(support.join(PENDING))?;
    sync_dir(support)
}
fn copy_database(source: &Connection, target: &mut Connection) -> Result<()> {
    Backup::new(source, target)?.run_to_completion(128, Duration::from_millis(10), None)?;
    Ok(())
}

/// Register UTF-8 JavaScript, optionally activating one explicitly selected profile.
/// The caller must ensure FlClash is stopped and lifecycle locks are held.
pub fn register(
    support: &Path,
    script: &Path,
    name: &str,
    allow_active_update: bool,
    activate_profile: Option<i64>,
) -> Result<Value> {
    let support = root(support)?;
    ensure!(!name.trim().is_empty(), "script label must not be empty");
    ensure!(
        !exists(&support.join(PENDING))?,
        "pending install journal exists; recover before registering"
    );
    let source = fs::read_to_string(script).context("reading UTF-8 JavaScript")?;
    ensure!(!source.trim().is_empty(), "JavaScript must not be empty");
    let mut db = open(&support, false)?;
    schema(&db)?;
    let rows = metadata(&db, name)?;
    ensure!(
        rows.len() <= 1,
        "multiple scripts have the same label; resolve ambiguity first"
    );
    directory(&support.join("scripts"))?;
    let existing = !rows.is_empty();
    let id = if existing {
        rows[0]["id"].as_i64().context("invalid script ID")?
    } else {
        next_id(&db, &support)?
    };
    let refs = active(&db, id)?;
    ensure!(
        refs.is_empty() || allow_active_update,
        "script is active; explicit allow_active_update is required"
    );
    if let Some(profile) = activate_profile {
        let count: i64 = db.query_row(
            "SELECT COUNT(*) FROM profiles WHERE id=?1",
            [profile],
            |r| r.get(0),
        )?;
        ensure!(count == 1, "activation profile ID does not exist");
    }
    let target = support.join("scripts").join(format!("{id}.js"));
    let had_script = exists(&target)?;
    if had_script {
        regular(&target, false)?;
    }
    let backups = support.join("skill-backups");
    directory(&backups)?;
    let backup_temp = tempfile::Builder::new()
        .prefix("good-cat-")
        .tempdir_in(&backups)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(backup_temp.path(), fs::Permissions::from_mode(0o700))?;
    }
    let backup = backup_temp.keep();
    let mut backup_db = Connection::open(backup.join("database.sqlite"))?;
    copy_database(&db, &mut backup_db)?;
    drop(backup_db);
    // Windows FlushFileBuffers requires a writable handle.
    fs::OpenOptions::new()
        .read(true)
        .write(true)
        .open(backup.join("database.sqlite"))?
        .sync_all()?;
    if had_script {
        atomic(&backup.join(format!("{id}.js")), &fs::read(&target)?)?;
    }
    sync_dir(&backup)?;
    sync_dir(&backups)?;
    let backup_name = backup
        .file_name()
        .and_then(|s| s.to_str())
        .context("invalid backup name")?;
    let mut journal = Journal {
        version: 1,
        id,
        target: format!("scripts/{id}.js"),
        backup: format!("skill-backups/{backup_name}"),
        had_script,
        phase: Phase::Prepared,
    };
    // The durable journal and backup precede *any* JavaScript replacement.
    write_journal(&support, &journal)?;
    let result = (|| -> Result<()> {
        let tx = db.transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
        atomic(&target, source.as_bytes())?;
        journal.phase = Phase::Replaced;
        write_journal(&support, &journal)?;
        let now = i64::try_from(SystemTime::now().duration_since(UNIX_EPOCH)?.as_secs())?;
        if existing {
            tx.execute(
                "UPDATE scripts SET last_update_time=?1 WHERE id=?2",
                params![now, id],
            )?;
        } else {
            tx.execute(
                "INSERT INTO scripts (id,label,last_update_time) VALUES (?1,?2,?3)",
                params![id, name, now],
            )?;
        }
        if let Some(profile) = activate_profile {
            ensure!(
                tx.execute(
                    "UPDATE profiles SET extend_type='script',script_id=?1 WHERE id=?2",
                    params![id, profile]
                )? == 1,
                "activation profile disappeared"
            );
        }
        tx.commit()?;
        Ok(())
    })();
    // Dropping the connection also rolls back a transaction whose rollback failed.
    drop(db);
    if let Err(original) = result {
        if let Err(recovery) = recover(&support) {
            return Err(RecoveryFailure {
                operation: format!("install failed: {original:#}"),
                recovery: format!("{recovery:#}"),
            }
            .into());
        }
        return Err(original);
    }
    // A failed directory sync after unlink must not be mistaken for a rollback:
    // SQLite has committed and recovery may no longer find a journal.
    remove_journal(&support).map_err(|e| RecoveryFailure {
        operation: "install committed but journal cleanup failed".into(),
        recovery: format!("{e:#}; backup retained at {}", backup.display()),
    })?;
    Ok(json!({"id": id, "target": target, "backup": backup,
        "updated": existing, "activated_profile": activate_profile}))
}

fn validated_journal(support: &Path) -> Result<(Journal, PathBuf, PathBuf)> {
    regular(&support.join(PENDING), false)?;
    let journal: Journal = serde_json::from_slice(&fs::read(support.join(PENDING))?)
        .context("invalid install journal")?;
    ensure!(journal.version == 1, "unsupported journal version");
    ensure!(
        journal.target == format!("scripts/{}.js", journal.id),
        "invalid journal target"
    );
    // Compare exact portable relative strings, not just a lexical starts_with.
    let name = journal
        .backup
        .strip_prefix("skill-backups/")
        .context("invalid journal backup")?;
    ensure!(
        name.starts_with("good-cat-")
            && name.len() > "good-cat-".len()
            && name
                .bytes()
                .all(|c| c.is_ascii_alphanumeric() || c == b'-' || c == b'_'),
        "invalid journal backup directory"
    );
    regular(&support.join("scripts"), true)?;
    regular(&support.join("skill-backups"), true)?;
    let target = support.join(&journal.target);
    if exists(&target)? {
        regular(&target, false)?;
    }
    let backup = support.join(&journal.backup);
    regular(&backup, true)?;
    regular(&backup.join("database.sqlite"), false)?;
    for suffix in ["-wal", "-shm", "-journal"] {
        let sidecar = backup.join(format!("database.sqlite{suffix}"));
        if exists(&sidecar)? {
            regular(&sidecar, false)?;
        }
    }
    if journal.had_script {
        regular(&backup.join(format!("{}.js", journal.id)), false)?;
    }
    Ok((journal, target, backup))
}

/// Roll back an interrupted install, even if SQLite committed before the crash.
/// Only call while FlClash is stopped and lifecycle locks are held. Keeps backups.
pub fn recover(support: &Path) -> Result<Value> {
    let operation = "pending install recovery";
    let result = (|| -> Result<Value> {
        let support = root(support)?;
        if !exists(&support.join(PENDING))? {
            return Ok(json!({"recovered": false}));
        }
        let (journal, target, backup) = validated_journal(&support)?;
        let source = Connection::open_with_flags(
            backup.join("database.sqlite"),
            OpenFlags::SQLITE_OPEN_READ_ONLY,
        )?;
        schema(&source)?;
        let mut errors = Vec::new();
        let restore_db = (|| -> Result<()> {
            let mut db = open(&support, false)?;
            copy_database(&source, &mut db)
        })();
        if let Err(e) = restore_db {
            errors.push(format!("database: {e:#}"));
        }
        let restore_script = (|| -> Result<()> {
            if journal.had_script {
                atomic(
                    &target,
                    &fs::read(backup.join(format!("{}.js", journal.id)))?,
                )?;
            } else if exists(&target)? {
                fs::remove_file(&target)?;
                sync_dir(target.parent().context("missing script directory")?)?;
            }
            Ok(())
        })();
        if let Err(e) = restore_script {
            errors.push(format!("JavaScript: {e:#}"));
        }
        if !errors.is_empty() {
            bail!("{}", errors.join("; "));
        }
        remove_journal(&support)?;
        Ok(json!({"recovered": true, "id": journal.id, "backup": backup}))
    })();
    result.map_err(|e| {
        anyhow!(RecoveryFailure {
            operation: operation.into(),
            recovery: format!("{e:#}")
        })
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fixture() -> (tempfile::TempDir, PathBuf) {
        let dir = tempfile::tempdir().unwrap();
        let db = Connection::open(dir.path().join("database.sqlite")).unwrap();
        db.execute_batch("CREATE TABLE scripts(id INTEGER PRIMARY KEY,label TEXT NOT NULL,last_update_time INTEGER NOT NULL,url TEXT,\"order\" INTEGER);
            CREATE TABLE profiles(id INTEGER PRIMARY KEY,extend_type TEXT,script_id INTEGER);
            INSERT INTO profiles VALUES(7,'none',NULL);").unwrap();
        let js = dir.path().join("input.js");
        fs::write(&js, "// new JavaScript\n").unwrap();
        (dir, js)
    }
    fn seed(dir: &Path) {
        let db = open(dir, false).unwrap();
        db.execute_batch(
            "INSERT INTO scripts VALUES(1,'cat',10,'https://example.invalid',9);
            UPDATE profiles SET extend_type='script',script_id=1 WHERE id=7;",
        )
        .unwrap();
        fs::create_dir(dir.join("scripts")).unwrap();
        fs::write(dir.join("scripts/1.js"), "// old").unwrap();
    }
    #[test]
    fn inspect_and_active_protection() {
        let (dir, js) = fixture();
        seed(dir.path());
        let plan = inspect(dir.path(), "cat").unwrap();
        assert_eq!(plan["scripts"][0]["order"], 9);
        assert_eq!(plan["active_refs"][0]["profile_ids"], json!([7]));
        assert!(register(dir.path(), &js, "cat", false, None).is_err());
        assert!(!dir.path().join("skill-backups").exists());
        register(dir.path(), &js, "cat", true, None).unwrap();
        let plan = inspect(dir.path(), "cat").unwrap();
        assert_eq!(plan["scripts"][0]["url_configured"], true);
        assert!(!plan.to_string().contains("https://example.invalid"));
        assert_eq!(
            open(dir.path(), true)
                .unwrap()
                .query_row("SELECT url FROM scripts WHERE id=1", [], |row| row
                    .get::<_, String>(0))
                .unwrap(),
            "https://example.invalid"
        );
        assert_eq!(plan["scripts"][0]["order"], 9);
    }
    #[test]
    fn activation_and_orphan() {
        let (dir, js) = fixture();
        fs::create_dir(dir.path().join("scripts")).unwrap();
        fs::write(dir.path().join("scripts/1.js"), "orphan").unwrap();
        let result = register(dir.path(), &js, "cat", false, Some(7)).unwrap();
        assert_eq!(result["id"], 2);
        assert_eq!(
            active(&open(dir.path(), true).unwrap(), 2).unwrap(),
            vec![7]
        );
        assert_eq!(
            fs::read_to_string(dir.path().join("scripts/1.js")).unwrap(),
            "orphan"
        );
        assert!(!dir.path().join(PENDING).exists());
    }
    #[test]
    fn sql_failure_restores_old_file_and_metadata() {
        let (dir, js) = fixture();
        seed(dir.path());
        open(dir.path(), false).unwrap().execute_batch(
            "CREATE TRIGGER fail BEFORE UPDATE ON scripts BEGIN SELECT RAISE(ABORT,'injected SQL failure'); END;"
        ).unwrap();
        let error = register(dir.path(), &js, "cat", true, None).unwrap_err();
        assert!(format!("{error:#}").contains("injected SQL failure"));
        assert!(error.downcast_ref::<RecoveryFailure>().is_none());
        assert_eq!(
            fs::read_to_string(dir.path().join("scripts/1.js")).unwrap(),
            "// old"
        );
        assert_eq!(
            inspect(dir.path(), "cat").unwrap()["scripts"][0]["last_update_time"],
            10
        );
        assert!(!dir.path().join(PENDING).exists());
    }
    #[test]
    fn recover_committed_crash_and_reject_paths() {
        let (dir, js) = fixture();
        seed(dir.path());
        let result = register(dir.path(), &js, "cat", true, None).unwrap();
        let backup = PathBuf::from(result["backup"].as_str().unwrap());
        let mut journal = Journal {
            version: 1,
            id: 1,
            target: "scripts/1.js".into(),
            backup: format!(
                "skill-backups/{}",
                backup.file_name().unwrap().to_str().unwrap()
            ),
            had_script: true,
            phase: Phase::Replaced,
        };
        write_journal(dir.path(), &journal).unwrap();
        assert!(register(dir.path(), &js, "cat", true, None).is_err());
        assert_eq!(recover(dir.path()).unwrap()["recovered"], true);
        assert_eq!(
            fs::read_to_string(dir.path().join("scripts/1.js")).unwrap(),
            "// old"
        );
        assert_eq!(
            inspect(dir.path(), "cat").unwrap()["scripts"][0]["last_update_time"],
            10
        );
        assert!(backup.exists());
        journal.backup = "skill-backups/good-cat-../../outside".into();
        write_journal(dir.path(), &journal).unwrap();
        assert!(recover(dir.path())
            .unwrap_err()
            .downcast_ref::<RecoveryFailure>()
            .is_some());
        assert!(dir.path().join(PENDING).exists());
    }
    #[test]
    fn dangling_refs_overflow_and_failed_new_install() {
        let (dir, js) = fixture();
        let db = open(dir.path(), false).unwrap();
        db.execute_batch("UPDATE profiles SET extend_type='script',script_id=1;")
            .unwrap();
        let result = register(dir.path(), &js, "cat", false, None).unwrap();
        assert_eq!(result["id"], 2);
        assert_eq!(active(&db, 1).unwrap(), vec![7]);
        db.execute_batch("CREATE TRIGGER fail BEFORE INSERT ON scripts BEGIN SELECT RAISE(ABORT,'injected insert failure'); END;").unwrap();
        assert!(register(dir.path(), &js, "other", false, None).is_err());
        assert!(!dir.path().join("scripts/3.js").exists());
        assert!(metadata(&db, "other").unwrap().is_empty());
        db.execute_batch("DROP TRIGGER fail; INSERT INTO scripts(id,label,last_update_time) VALUES(9223372036854775807,'max',0);").unwrap();
        assert!(register(dir.path(), &js, "overflow", false, None).is_err());
    }
    #[test]
    fn inspection_preserves_wal_files() {
        let (dir, _) = fixture();
        let db = open(dir.path(), false).unwrap();
        db.execute_batch(
            "PRAGMA journal_mode=WAL; INSERT INTO scripts VALUES(1,'wal',1,NULL,NULL);",
        )
        .unwrap();
        let paths = [
            "database.sqlite",
            "database.sqlite-wal",
            "database.sqlite-shm",
        ];
        let before: Vec<_> = paths
            .iter()
            .map(|p| fs::read(dir.path().join(p)).unwrap())
            .collect();
        assert_eq!(inspect(dir.path(), "wal").unwrap()["scripts"][0]["id"], 1);
        for (path, bytes) in paths.iter().zip(before) {
            assert_eq!(fs::read(dir.path().join(path)).unwrap(), bytes);
        }
        assert!(!dir.path().join("skill-backups").exists());
    }
    #[cfg(unix)]
    #[test]
    fn recovery_rejects_symlink_backup_and_preserves_journal() {
        use std::os::unix::fs::symlink;
        let (dir, js) = fixture();
        let result = register(dir.path(), &js, "cat", false, None).unwrap();
        let backup = PathBuf::from(result["backup"].as_str().unwrap());
        let alias = dir.path().join("skill-backups/good-cat-alias");
        symlink(&backup, &alias).unwrap();
        let journal = Journal {
            version: 1,
            id: 1,
            target: "scripts/1.js".into(),
            backup: "skill-backups/good-cat-alias".into(),
            had_script: false,
            phase: Phase::Prepared,
        };
        write_journal(dir.path(), &journal).unwrap();
        let error = recover(dir.path()).unwrap_err();
        assert!(error.downcast_ref::<RecoveryFailure>().is_some());
        assert!(dir.path().join(PENDING).exists());
        // Retrying with a valid prepared journal also removes newly installed JS.
        let journal = Journal {
            backup: format!(
                "skill-backups/{}",
                backup.file_name().unwrap().to_str().unwrap()
            ),
            ..journal
        };
        write_journal(dir.path(), &journal).unwrap();
        recover(dir.path()).unwrap();
        assert!(!dir.path().join("scripts/1.js").exists());
        assert!(inspect(dir.path(), "cat").unwrap()["scripts"]
            .as_array()
            .unwrap()
            .is_empty());
    }
    #[test]
    fn missing_database_bad_schema_and_empty_input() {
        let missing = tempfile::tempdir().unwrap();
        assert!(inspect(missing.path(), "cat").is_err());
        assert!(!missing.path().join("database.sqlite").exists());
        let (dir, js) = fixture();
        fs::write(&js, " \n\t").unwrap();
        assert!(register(dir.path(), &js, "cat", false, None).is_err());
        fs::write(&js, [0xff]).unwrap();
        assert!(register(dir.path(), &js, "cat", false, None).is_err());
        fs::write(&js, "// ok").unwrap();
        open(dir.path(), false)
            .unwrap()
            .execute_batch("ALTER TABLE profiles RENAME COLUMN script_id TO wrong;")
            .unwrap();
        assert!(register(dir.path(), &js, "cat", false, None).is_err());
        assert!(!dir.path().join("skill-backups").exists());
    }
}
