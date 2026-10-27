//! Offline installation only. The caller must stop FlClash and hold lifecycle locks.
use anyhow::{anyhow, bail, ensure, Context, Result};
use cap_fs_ext::{DirExt, FollowSymlinks, MetadataExt, OpenOptionsFollowExt};
use cap_std::{
    ambient_authority,
    fs::{Dir, OpenOptions},
};
use rusqlite::{backup::Backup, params, Connection, OpenFlags};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::{
    error::Error,
    fmt, fs,
    io::{Read, Write},
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
    #[serde(default)]
    before: String,
    #[serde(default)]
    after: String,
    #[serde(default)]
    script_before: Option<String>,
    #[serde(default)]
    script_after: String,
    #[serde(default)]
    scripts_identity: Option<(u64, u64)>,
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
fn anchored_directory(parent: &Dir, name: &str) -> Result<Dir> {
    match parent.create_dir(name) {
        Ok(()) => (),
        Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => (),
        Err(e) => return Err(e.into()),
    }
    Ok(parent.open_dir_nofollow(name)?)
}

fn identity(dir: &Dir) -> Result<(u64, u64)> {
    let meta = dir.dir_metadata()?;
    Ok((meta.dev(), meta.ino()))
}

fn still_attached(parent: &Dir, name: &str, held: &Dir) -> Result<()> {
    let current = parent
        .open_dir_nofollow(name)
        .context("managed directory entry changed; journal retained")?;
    ensure!(
        identity(&current)? == identity(held)?,
        "managed directory identity changed; journal retained"
    );
    Ok(())
}

fn read_leaf(dir: &Dir, name: &str) -> Result<Option<Vec<u8>>> {
    let mut options = OpenOptions::new();
    options.read(true).follow(FollowSymlinks::No);
    let mut file = match dir.open_with(name, &options) {
        Ok(file) => file,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(e) => return Err(e.into()),
    };
    ensure!(
        file.metadata()?.is_file(),
        "managed source must be a regular file"
    );
    let mut bytes = Vec::new();
    file.read_to_end(&mut bytes)?;
    Ok(Some(bytes))
}

pub(crate) fn read_managed_source(support: &Path, id: i64) -> Result<Vec<u8>> {
    let support = root(support)?;
    let root = Dir::open_ambient_dir(&support, ambient_authority())?;
    let scripts = root.open_dir_nofollow("scripts")?;
    let bytes = read_leaf(&scripts, &format!("{id}.js"))?.context("managed script missing")?;
    still_attached(&root, "scripts", &scripts)?;
    Ok(bytes)
}

fn sync_anchored(dir: &Dir) -> Result<()> {
    #[cfg(unix)]
    dir.try_clone()?.into_std_file().sync_all()?;
    #[cfg(not(unix))]
    let _ = dir;
    Ok(())
}

// Only single-component leaves are passed here. Neither creation nor rename
// resolves the mutable ambient pathname of the held directory.
fn atomic_in(dir: &Dir, name: &str, bytes: &[u8]) -> Result<()> {
    let (temp, mut file) = loop {
        let temp = format!(".good-cat-{:032x}.tmp", rand::random::<u128>());
        let mut options = OpenOptions::new();
        options
            .write(true)
            .create_new(true)
            .follow(FollowSymlinks::No);
        match dir.open_with(&temp, &options) {
            Ok(file) => break (temp, file),
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => continue,
            Err(e) => return Err(e.into()),
        }
    };
    let result = (|| -> Result<()> {
        file.write_all(bytes)?;
        file.sync_all()?;
        drop(file);
        dir.rename(&temp, dir, name)?;
        sync_anchored(dir)
    })();
    if result.is_err() {
        let _ = dir.remove_file(&temp);
    }
    result
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
fn next_id(db: &Connection, scripts: Option<&Dir>) -> Result<i64> {
    let maximum: i64 = db.query_row("SELECT COALESCE(MAX(id),0) FROM scripts", [], |r| r.get(0))?;
    let mut id = maximum.checked_add(1).context("script ID exhausted")?;
    loop {
        let referenced: bool = db.query_row(
            "SELECT EXISTS(SELECT 1 FROM profiles WHERE script_id=?1)",
            [id],
            |r| r.get(0),
        )?;
        let occupied = match scripts {
            Some(dir) => match dir.symlink_metadata(format!("{id}.js")) {
                Ok(_) => true,
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => false,
                Err(e) => return Err(e.into()),
            },
            None => false,
        };
        if !referenced && !occupied {
            break;
        }
        id = id.checked_add(1).context("script ID exhausted")?;
    }
    Ok(id)
}

/// Strict namespace reserved for scripts managed by good-cat.
pub fn is_managed_name(name: &str) -> bool {
    let Some(slug) = name.strip_prefix("good-cat-") else {
        return false;
    };
    (1..=80).contains(&slug.chars().count())
        && slug.chars().next().is_some_and(char::is_alphanumeric)
        && slug
            .chars()
            .all(|c| c.is_alphanumeric() || matches!(c, '.' | '_' | '-'))
}

// Keep the private copy alive for the lifetime of its connection.
pub(crate) fn snapshot(support: &Path) -> Result<(tempfile::TempDir, Connection)> {
    let support = root(support)?;
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
    Ok((snapshot, db))
}

/// Read only the selected subscription's display name; never return its URL.
pub fn profile_label(support: &Path, profile_id: i64) -> Result<Option<String>> {
    let (_snapshot, db) = snapshot(support)?;
    let mut statement = db.prepare("PRAGMA table_info(profiles)")?;
    let columns = statement
        .query_map([], |row| row.get::<_, String>(1))?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    if !columns.iter().any(|column| column == "label") {
        return Ok(None);
    }
    let label: Option<String> = db.query_row(
        "SELECT label FROM profiles WHERE id=?1",
        [profile_id],
        |row| row.get(0),
    )?;
    Ok(label.filter(|label| !label.trim().is_empty()))
}

/// Read-only plan, including exact existing metadata and active profile references.
/// Does not create a database, directories, backups, or a journal.
pub fn inspect(support: &Path, name: &str) -> Result<Value> {
    let support = root(support)?;
    let root_dir = Dir::open_ambient_dir(&support, ambient_authority())?;
    let scripts_dir = match root_dir.open_dir_nofollow("scripts") {
        Ok(dir) => Some(dir),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => None,
        Err(e) => return Err(e.into()),
    };
    let (_snapshot, db) = snapshot(&support)?;
    let scripts = metadata(&db, name)?;
    let mut references = Vec::new();
    for script in &scripts {
        let id = script["id"].as_i64().context("invalid script ID")?;
        references.push(json!({"script_id": id, "profile_ids": active(&db, id)?}));
    }
    let id = match scripts.as_slice() {
        [] => Some(next_id(&db, scripts_dir.as_ref())?),
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

fn hash(bytes: &[u8]) -> String {
    format!("{:x}", Sha256::digest(bytes))
}

// Encode SQLite values with type tags and lengths; sort encoded rows, not SQL
// values (whose collation/type coercion can hide distinctions). Include internal
// tables and schema so unrelated changes also prohibit whole-database rollback.
fn fingerprint(db: &Connection) -> Result<String> {
    fn rows(db: &Connection, sql: &str) -> Result<Vec<Vec<u8>>> {
        use rusqlite::types::ValueRef;
        let mut stmt = db.prepare(sql)?;
        let columns = stmt.column_count();
        let mut query = stmt.query([])?;
        let mut result = Vec::new();
        while let Some(row) = query.next()? {
            let mut bytes = Vec::new();
            for i in 0..columns {
                let (tag, value) = match row.get_ref(i)? {
                    ValueRef::Null => (0, Vec::new()),
                    ValueRef::Integer(v) => (1, v.to_be_bytes().to_vec()),
                    ValueRef::Real(v) => (2, v.to_bits().to_be_bytes().to_vec()),
                    ValueRef::Text(v) => (3, v.to_vec()),
                    ValueRef::Blob(v) => (4, Sha256::digest(v).to_vec()),
                };
                bytes.push(tag);
                bytes.extend_from_slice(&(value.len() as u64).to_be_bytes());
                bytes.extend_from_slice(&value);
            }
            result.push(bytes);
        }
        result.sort();
        Ok(result)
    }
    let mut digest = Sha256::new();
    let mut feed = |bytes: &[u8]| {
        digest.update((bytes.len() as u64).to_be_bytes());
        digest.update(bytes);
    };
    for pragma in ["user_version", "application_id", "encoding"] {
        for row in rows(db, &format!("PRAGMA {pragma}"))? {
            feed(&row);
        }
    }
    for row in rows(db, "SELECT rowid,type,name,tbl_name,sql FROM sqlite_schema")? {
        feed(&row);
    }
    let mut stmt = db.prepare("SELECT name FROM sqlite_schema WHERE type='table' ORDER BY name")?;
    let tables = stmt
        .query_map([], |r| r.get::<_, String>(0))?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    for table in tables {
        feed(table.as_bytes());
        let quoted = table.replace('"', "\"\"");
        let without_rowid: bool = db.query_row(
            "SELECT wr FROM pragma_table_list WHERE schema='main' AND name=?1",
            [&table],
            |r| r.get(0),
        )?;
        let mut columns = db.prepare(&format!("PRAGMA table_xinfo(\"{quoted}\")"))?;
        let names = columns
            .query_map([], |r| r.get::<_, String>(1))?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        let projection = if without_rowid {
            "*".to_owned()
        } else {
            let rowid = ["rowid", "_rowid_", "oid"]
                .into_iter()
                .find(|id| !names.iter().any(|name| name.eq_ignore_ascii_case(id)))
                .context("cannot fingerprint shadowed row identity safely")?;
            format!("{rowid},*")
        };
        for row in rows(db, &format!("SELECT {projection} FROM \"{quoted}\""))? {
            feed(&row);
        }
    }
    Ok(format!("{:x}", digest.finalize()))
}

pub(crate) struct ExpectedTarget {
    pub profile_id: i64,
    pub script_id: i64,
    pub label: String,
    pub source_sha256: String,
}

pub(crate) fn register_current(
    support: &Path,
    script: &Path,
    expected: ExpectedTarget,
) -> Result<Value> {
    let name = expected.label.clone();
    register_impl(support, script, &name, None, Some(expected))
}

/// Create a new script, optionally activating one explicitly selected profile.
/// Existing labels are always rejected; the compatibility boolean is obsolete.
/// The caller must ensure FlClash is stopped and lifecycle locks are held.
pub fn register(
    support: &Path,
    script: &Path,
    name: &str,
    _allow_active_update: bool,
    activate_profile: Option<i64>,
) -> Result<Value> {
    // The compatibility boolean never authorizes an overwrite.
    register_impl(support, script, name, activate_profile, None)
}

fn register_impl(
    support: &Path,
    script: &Path,
    name: &str,
    activate_profile: Option<i64>,
    expected: Option<ExpectedTarget>,
) -> Result<Value> {
    let support = root(support)?;
    ensure!(
        is_managed_name(name),
        "script label must be a managed good-cat slug"
    );
    ensure!(
        !exists(&support.join(PENDING))?,
        "pending install journal exists; recover before registering"
    );
    let source = fs::read_to_string(script).context("reading UTF-8 JavaScript")?;
    ensure!(!source.trim().is_empty(), "JavaScript must not be empty");
    let mut db = open(&support, false)?;
    schema(&db)?;
    if expected.is_none() {
        ensure!(
            metadata(&db, name)?.is_empty(),
            "install is create-only; script label already exists"
        );
    }
    let root_dir = Dir::open_ambient_dir(&support, ambient_authority())?;
    let scripts_dir = anchored_directory(&root_dir, "scripts")?;
    let backups_dir = anchored_directory(&root_dir, "skill-backups")?;
    let backup_name = loop {
        let name = format!("good-cat-{:032x}", rand::random::<u128>());
        let mut builder = cap_std::fs::DirBuilder::new();
        #[cfg(unix)]
        {
            use cap_std::fs::DirBuilderExt;
            builder.mode(0o700);
        }
        match backups_dir.create_dir_with(&name, &builder) {
            Ok(()) => break name,
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => continue,
            Err(e) => return Err(e.into()),
        }
    };
    let backup_dir = backups_dir.open_dir_nofollow(&backup_name)?;
    let backup = support.join("skill-backups").join(&backup_name);
    // SQLite needs a pathname. Build its standalone backup in a private temp
    // directory, then publish the closed database through the anchored handle.
    let private_backup = tempfile::tempdir()?;
    let backup_db_path = private_backup.path().join("database.sqlite");
    let mut backup_db = Connection::open(&backup_db_path)?;
    // The backup API must run before acquiring a write transaction.
    copy_database(&db, &mut backup_db)?;
    let before = fingerprint(&backup_db)?;
    drop(backup_db);
    atomic_in(&backup_dir, "database.sqlite", &fs::read(&backup_db_path)?)?;
    let tx = db.transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
    ensure!(
        fingerprint(&tx)? == before,
        "database changed during backup; retry"
    );
    let rows = metadata(&tx, name)?;
    let existing = expected.is_some();
    let id = if let Some(ref expected) = expected {
        ensure!(
            rows.len() == 1 && rows[0]["id"].as_i64() == Some(expected.script_id),
            "expected script ID/label changed"
        );
        let binding: (Option<String>, Option<i64>) = tx.query_row(
            "SELECT extend_type,script_id FROM profiles WHERE id=?1",
            [expected.profile_id],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )?;
        ensure!(
            binding == (Some("script".into()), Some(expected.script_id)),
            "current profile binding changed"
        );
        let mut stmt = tx.prepare("SELECT id FROM profiles WHERE script_id=?1 ORDER BY id")?;
        let refs = stmt
            .query_map([expected.script_id], |r| r.get::<_, i64>(0))?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        ensure!(
            refs == [expected.profile_id],
            "shared script updates are not allowed"
        );
        expected.script_id
    } else {
        ensure!(
            rows.is_empty(),
            "install is create-only; script label already exists"
        );
        next_id(&tx, Some(&scripts_dir))?
    };
    if let Some(profile) = activate_profile {
        let count: i64 = tx.query_row(
            "SELECT COUNT(*) FROM profiles WHERE id=?1",
            [profile],
            |r| r.get(0),
        )?;
        ensure!(count == 1, "activation profile ID does not exist");
    }
    let target = support.join("scripts").join(format!("{id}.js"));
    let leaf = format!("{id}.js");
    let old_source = read_leaf(&scripts_dir, &leaf)?;
    let had_script = old_source.is_some();
    if let Some(ref expected) = expected {
        ensure!(
            old_source.as_ref().map(|s| hash(s)).as_deref()
                == Some(expected.source_sha256.as_str()),
            "current script changed; export again before updating"
        );
    }
    if had_script {
        atomic_in(
            &backup_dir,
            &leaf,
            old_source.as_ref().context("missing old script")?,
        )?;
    }
    sync_anchored(&backup_dir)?;
    sync_anchored(&backups_dir)?;
    let mut journal = Journal {
        version: 2,
        id,
        target: format!("scripts/{id}.js"),
        backup: format!("skill-backups/{backup_name}"),
        had_script,
        phase: Phase::Prepared,
        before,
        after: String::new(),
        script_before: old_source.as_ref().map(|s| hash(s)),
        script_after: hash(source.as_bytes()),
        scripts_identity: Some(identity(&scripts_dir)?),
    };
    let mut replacement_started = false;
    let result = (|| -> Result<()> {
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
        journal.after = fingerprint(&tx)?;
        // Both logical states are pinned before either file replacement or commit.
        write_journal(&support, &journal)?;
        still_attached(&root_dir, "scripts", &scripts_dir)?;
        still_attached(&root_dir, "skill-backups", &backups_dir)?;
        still_attached(&backups_dir, &backup_name, &backup_dir)?;
        if let Some(ref expected) = expected {
            ensure!(
                read_leaf(&scripts_dir, &leaf)?
                    .as_ref()
                    .map(|s| hash(s))
                    .as_deref()
                    == Some(expected.source_sha256.as_str()),
                "current script changed immediately before replacement"
            );
        }
        replacement_started = true;
        atomic_in(&scripts_dir, &leaf, source.as_bytes())?;
        journal.phase = Phase::Replaced;
        write_journal(&support, &journal)?;
        still_attached(&root_dir, "scripts", &scripts_dir)?;
        still_attached(&root_dir, "skill-backups", &backups_dir)?;
        still_attached(&backups_dir, &backup_name, &backup_dir)?;
        tx.commit()?;
        Ok(())
    })();
    // Dropping the connection also rolls back a transaction whose rollback failed.
    drop(db);
    if let Err(original) = result {
        // SQL was rolled back and no file replacement was attempted. Do not
        // restore an old source over a newly detected external filesystem edit.
        if !replacement_started {
            if let Err(recovery) = still_attached(&root_dir, "scripts", &scripts_dir)
                .and_then(|_| still_attached(&root_dir, "skill-backups", &backups_dir))
                .and_then(|_| still_attached(&backups_dir, &backup_name, &backup_dir))
            {
                return Err(RecoveryFailure {
                    operation: format!("install failed: {original:#}"),
                    recovery: format!("{recovery:#}"),
                }
                .into());
            }
            if exists(&support.join(PENDING))? {
                remove_journal(&support).map_err(|e| RecoveryFailure {
                    operation: format!("install failed: {original:#}"),
                    recovery: format!("{e:#}"),
                })?;
            }
            return Err(original);
        }
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

fn validated_journal(support: &Path) -> Result<Journal> {
    regular(&support.join(PENDING), false)?;
    let journal: Journal = serde_json::from_slice(&fs::read(support.join(PENDING))?)
        .context("invalid install journal")?;
    ensure!(
        journal.version == 2
            && journal.before.len() == 64
            && journal.after.len() == 64
            && journal.script_after.len() == 64
            && journal.had_script == journal.script_before.is_some(),
        "unverifiable legacy or invalid journal; manual intervention required"
    );
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
    Ok(journal)
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
        let root_dir = Dir::open_ambient_dir(&support, ambient_authority())?;
        let journal = validated_journal(&support)?;
        let scripts_dir = root_dir.open_dir_nofollow("scripts")?;
        if let Some(expected) = journal.scripts_identity {
            ensure!(
                identity(&scripts_dir)? == expected,
                "scripts directory changed; manual intervention required"
            );
        }
        let backups_dir = root_dir.open_dir_nofollow("skill-backups")?;
        let backup_name = journal
            .backup
            .strip_prefix("skill-backups/")
            .context("invalid backup directory")?;
        let backup_dir = backups_dir.open_dir_nofollow(backup_name)?;
        let backup = support.join(&journal.backup);
        // Never ask SQLite to resolve the mutable backup directory path.
        let private_backup = tempfile::tempdir()?;
        let backup_path = private_backup.path().join("database.sqlite");
        fs::write(
            &backup_path,
            read_leaf(&backup_dir, "database.sqlite")?.context("missing backup database")?,
        )?;
        for suffix in ["-wal", "-shm", "-journal"] {
            let name = format!("database.sqlite{suffix}");
            if let Some(bytes) = read_leaf(&backup_dir, &name)? {
                fs::write(private_backup.path().join(name), bytes)?;
            }
        }
        let source = Connection::open_with_flags(&backup_path, OpenFlags::SQLITE_OPEN_READ_ONLY)?;
        schema(&source)?;
        ensure!(
            fingerprint(&source)? == journal.before,
            "backup database changed; manual intervention required"
        );
        let mut current = open(&support, false)?;
        // Exclusive locking mode retains database-level exclusion after COMMIT,
        // allowing the backup API to restore without an active transaction.
        current.execute_batch("PRAGMA locking_mode=EXCLUSIVE; BEGIN EXCLUSIVE; COMMIT;")?;
        let current_hash = fingerprint(&current)?;
        ensure!(
            current_hash == journal.before || current_hash == journal.after,
            "database drift detected; manual intervention required (journal and backup retained)"
        );
        let backup_script = if journal.had_script {
            Some(
                read_leaf(&backup_dir, &format!("{}.js", journal.id))?
                    .context("missing backup script")?,
            )
        } else {
            None
        };
        ensure!(
            backup_script.as_ref().map(|s| hash(s)) == journal.script_before,
            "backup script changed; manual intervention required"
        );
        let leaf = format!("{}.js", journal.id);
        let target_hash = read_leaf(&scripts_dir, &leaf)?.as_ref().map(|s| hash(s));
        ensure!(
            target_hash == journal.script_before
                || target_hash.as_deref() == Some(journal.script_after.as_str()),
            "script drift detected; manual intervention required"
        );
        still_attached(&root_dir, "scripts", &scripts_dir)?;
        still_attached(&root_dir, "skill-backups", &backups_dir)?;
        still_attached(&backups_dir, backup_name, &backup_dir)?;
        let mut errors = Vec::new();
        let restore_db = copy_database(&source, &mut current);
        if let Err(e) = restore_db {
            errors.push(format!("database: {e:#}"));
        }
        let restore_script = (|| -> Result<()> {
            if journal.had_script {
                atomic_in(
                    &scripts_dir,
                    &leaf,
                    backup_script.as_ref().context("missing backup script")?,
                )?;
            } else if read_leaf(&scripts_dir, &leaf)?.is_some() {
                scripts_dir.remove_file(&leaf)?;
                sync_anchored(&scripts_dir)?;
            }
            still_attached(&root_dir, "scripts", &scripts_dir)?;
            still_attached(&root_dir, "skill-backups", &backups_dir)?;
            still_attached(&backups_dir, backup_name, &backup_dir)?;
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
            "INSERT INTO scripts VALUES(1,'good-cat-cat',10,'https://example.invalid',9);
            UPDATE profiles SET extend_type='script',script_id=1 WHERE id=7;",
        )
        .unwrap();
        fs::create_dir(dir.join("scripts")).unwrap();
        fs::write(dir.join("scripts/1.js"), "// old").unwrap();
    }
    fn update(dir: &Path, js: &Path) -> Result<Value> {
        register_current(
            dir,
            js,
            ExpectedTarget {
                profile_id: 7,
                script_id: 1,
                label: "good-cat-cat".into(),
                source_sha256: hash(b"// old"),
            },
        )
    }
    fn crash_journal(dir: &Path, backup: &Path, had_script: bool) -> Journal {
        Journal {
            version: 2,
            id: 1,
            target: "scripts/1.js".into(),
            backup: format!(
                "skill-backups/{}",
                backup.file_name().unwrap().to_str().unwrap()
            ),
            had_script,
            phase: Phase::Replaced,
            before: fingerprint(&Connection::open(backup.join("database.sqlite")).unwrap())
                .unwrap(),
            after: fingerprint(&open(dir, false).unwrap()).unwrap(),
            script_before: if had_script {
                Some(hash(&fs::read(backup.join("1.js")).unwrap()))
            } else {
                None
            },
            script_after: hash(&fs::read(dir.join("scripts/1.js")).unwrap()),
            scripts_identity: Some(
                identity(&Dir::open_ambient_dir(dir.join("scripts"), ambient_authority()).unwrap())
                    .unwrap(),
            ),
        }
    }
    #[cfg(unix)]
    #[test]
    fn anchored_script_write_cannot_follow_swapped_directory() {
        use std::os::unix::fs::symlink;
        let (dir, _) = fixture();
        seed(dir.path());
        let other = tempfile::tempdir().unwrap();
        fs::write(other.path().join("1.js"), b"unrelated").unwrap();
        let root = Dir::open_ambient_dir(dir.path(), ambient_authority()).unwrap();
        let scripts = root.open_dir_nofollow("scripts").unwrap();
        fs::rename(
            dir.path().join("scripts"),
            dir.path().join("original-scripts"),
        )
        .unwrap();
        symlink(other.path(), dir.path().join("scripts")).unwrap();

        // Simulate the swap after validation but before atomic publication.
        atomic_in(&scripts, "1.js", b"approved replacement").unwrap();
        assert_eq!(fs::read(other.path().join("1.js")).unwrap(), b"unrelated");
        assert_eq!(
            read_leaf(&scripts, "1.js").unwrap().unwrap(),
            b"approved replacement"
        );
        assert!(still_attached(&root, "scripts", &scripts).is_err());
        assert!(read_managed_source(dir.path(), 1).is_err());
    }

    #[cfg(unix)]
    #[test]
    fn anchored_backup_read_cannot_follow_swapped_directory() {
        use std::os::unix::fs::symlink;
        let dir = tempfile::tempdir().unwrap();
        let other = tempfile::tempdir().unwrap();
        let root = Dir::open_ambient_dir(dir.path(), ambient_authority()).unwrap();
        let backups = anchored_directory(&root, "skill-backups").unwrap();
        let backup = anchored_directory(&backups, "good-cat-test").unwrap();
        atomic_in(&backup, "1.js", b"original backup").unwrap();
        fs::write(other.path().join("1.js"), b"unrelated").unwrap();
        fs::rename(
            dir.path().join("skill-backups/good-cat-test"),
            dir.path().join("saved-backup"),
        )
        .unwrap();
        symlink(other.path(), dir.path().join("skill-backups/good-cat-test")).unwrap();
        assert_eq!(
            read_leaf(&backup, "1.js").unwrap().unwrap(),
            b"original backup"
        );
        assert!(still_attached(&backups, "good-cat-test", &backup).is_err());
        assert!(backups.open_dir_nofollow("good-cat-test").is_err());
        assert_eq!(fs::read(other.path().join("1.js")).unwrap(), b"unrelated");
    }

    #[cfg(unix)]
    #[test]
    fn recovery_refuses_swapped_scripts_and_keeps_journal() {
        use std::os::unix::fs::symlink;
        let (dir, js) = fixture();
        seed(dir.path());
        let result = update(dir.path(), &js).unwrap();
        let backup = PathBuf::from(result["backup"].as_str().unwrap());
        let journal = crash_journal(dir.path(), &backup, true);
        write_journal(dir.path(), &journal).unwrap();
        let other = tempfile::tempdir().unwrap();
        // Matching hashes must not authorize restoration through a redirect.
        fs::write(
            other.path().join("1.js"),
            fs::read(dir.path().join("scripts/1.js")).unwrap(),
        )
        .unwrap();
        let untouched = fs::read(other.path().join("1.js")).unwrap();
        fs::rename(dir.path().join("scripts"), dir.path().join("saved-scripts")).unwrap();
        symlink(other.path(), dir.path().join("scripts")).unwrap();
        assert!(recover(dir.path()).unwrap_err().is::<RecoveryFailure>());
        assert!(dir.path().join(PENDING).exists());
        assert_eq!(fs::read(other.path().join("1.js")).unwrap(), untouched);
        fs::remove_file(dir.path().join("scripts")).unwrap();
        fs::create_dir(dir.path().join("scripts")).unwrap();
        fs::write(dir.path().join("scripts/1.js"), &untouched).unwrap();
        // Replacing with a different real directory is also rejected by identity.
        assert!(recover(dir.path()).unwrap_err().is::<RecoveryFailure>());
        assert_eq!(
            fs::read(dir.path().join("scripts/1.js")).unwrap(),
            untouched
        );
        assert!(dir.path().join(PENDING).exists());
    }

    #[test]
    fn managed_names_and_shared_protection() {
        for name in [
            "good-cat",
            "cat",
            "good-cat-",
            "good-cat-.x",
            "good-cat-_x",
            "good-cat-a/b",
            "good-cat-a b",
            "good-cat-名字/后缀",
            "good-cat-a\n",
        ] {
            assert!(!is_managed_name(name));
            let (dir, js) = fixture();
            assert!(register(dir.path(), &js, name, true, None).is_err());
            seed(dir.path());
            open(dir.path(), false)
                .unwrap()
                .execute("UPDATE scripts SET label=?1", [name])
                .unwrap();
            assert!(register(dir.path(), &js, name, true, None).is_err());
            assert!(!dir.path().join("skill-backups").exists());
        }
        assert!(is_managed_name("good-cat-A.z_9-"));
        assert!(is_managed_name("good-cat-é"));
        assert!(is_managed_name("good-cat-良心云"));
        assert!(is_managed_name(&format!("good-cat-{}", "x".repeat(80))));
        assert!(!is_managed_name(&format!("good-cat-{}", "x".repeat(81))));
        let (dir, js) = fixture();
        seed(dir.path());
        open(dir.path(), false)
            .unwrap()
            .execute_batch("INSERT INTO profiles VALUES(8,'script',1);")
            .unwrap();
        assert!(register(dir.path(), &js, "good-cat-cat", true, None).is_err());
        assert!(!dir.path().join("skill-backups").exists());
    }
    #[test]
    fn selected_profile_label_is_read_only_and_never_includes_url() {
        let (dir, _) = fixture();
        assert_eq!(profile_label(dir.path(), 7).unwrap(), None);
        let db = open(dir.path(), false).unwrap();
        db.execute_batch(
            "ALTER TABLE profiles ADD COLUMN label TEXT; ALTER TABLE profiles ADD COLUMN url TEXT;",
        )
        .unwrap();
        db.execute(
            "UPDATE profiles SET label=?1,url=?2 WHERE id=7",
            params![
                "订阅名称 · 全球节点",
                "https://secret.invalid/subscription?token=private"
            ],
        )
        .unwrap();
        let before = fs::read(dir.path().join("database.sqlite")).unwrap();
        assert_eq!(
            profile_label(dir.path(), 7).unwrap().as_deref(),
            Some("订阅名称 · 全球节点")
        );
        assert_eq!(
            fs::read(dir.path().join("database.sqlite")).unwrap(),
            before
        );
    }

    #[test]
    fn inspect_and_active_protection() {
        let (dir, js) = fixture();
        seed(dir.path());
        let plan = inspect(dir.path(), "good-cat-cat").unwrap();
        assert_eq!(plan["scripts"][0]["order"], 9);
        assert_eq!(plan["active_refs"][0]["profile_ids"], json!([7]));
        assert!(register(dir.path(), &js, "good-cat-cat", false, None).is_err());
        assert!(!dir.path().join("skill-backups").exists());
        assert!(register(dir.path(), &js, "good-cat-cat", true, None).is_err());
        update(dir.path(), &js).unwrap();
        let plan = inspect(dir.path(), "good-cat-cat").unwrap();
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
        let result = register(dir.path(), &js, "good-cat-cat", false, Some(7)).unwrap();
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
        let error = update(dir.path(), &js).unwrap_err();
        assert!(format!("{error:#}").contains("injected SQL failure"));
        assert!(error.downcast_ref::<RecoveryFailure>().is_none());
        assert_eq!(
            fs::read_to_string(dir.path().join("scripts/1.js")).unwrap(),
            "// old"
        );
        assert_eq!(
            inspect(dir.path(), "good-cat-cat").unwrap()["scripts"][0]["last_update_time"],
            10
        );
        assert!(!dir.path().join(PENDING).exists());
    }
    #[test]
    fn recover_committed_crash_and_reject_paths() {
        let (dir, js) = fixture();
        seed(dir.path());
        let result = update(dir.path(), &js).unwrap();
        let backup = PathBuf::from(result["backup"].as_str().unwrap());
        let mut journal = crash_journal(dir.path(), &backup, true);
        write_journal(dir.path(), &journal).unwrap();
        assert!(register(dir.path(), &js, "good-cat-cat", true, None).is_err());
        assert_eq!(recover(dir.path()).unwrap()["recovered"], true);
        assert_eq!(
            fs::read_to_string(dir.path().join("scripts/1.js")).unwrap(),
            "// old"
        );
        assert_eq!(
            inspect(dir.path(), "good-cat-cat").unwrap()["scripts"][0]["last_update_time"],
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
    fn recovery_refuses_unrelated_database_and_script_drift() {
        for mutation in [
            "INSERT INTO profiles VALUES(8,'none',NULL)",
            "CREATE TABLE unrelated(payload BLOB); INSERT INTO unrelated VALUES(x'0102')",
            "PRAGMA user_version=42",
            "PRAGMA application_id=99",
            "UPDATE profiles SET rowid=8 WHERE id=7",
        ] {
            let (dir, js) = fixture();
            seed(dir.path());
            let result = update(dir.path(), &js).unwrap();
            let backup = PathBuf::from(result["backup"].as_str().unwrap());
            let journal = crash_journal(dir.path(), &backup, true);
            write_journal(dir.path(), &journal).unwrap();
            let db = open(dir.path(), false).unwrap();
            db.execute_batch(mutation).unwrap();
            let changed = fingerprint(&db).unwrap();
            drop(db);
            let error = recover(dir.path()).unwrap_err();
            assert!(error.downcast_ref::<RecoveryFailure>().is_some());
            assert!(format!("{error:#}").contains("database drift"));
            assert_eq!(
                fingerprint(&open(dir.path(), false).unwrap()).unwrap(),
                changed
            );
            assert_eq!(
                fs::read_to_string(dir.path().join("scripts/1.js")).unwrap(),
                "// new JavaScript\n"
            );
            assert!(dir.path().join(PENDING).exists());
            assert!(backup.exists());
        }
        for change_backup in [false, true] {
            let (dir, js) = fixture();
            seed(dir.path());
            let result = update(dir.path(), &js).unwrap();
            let backup = PathBuf::from(result["backup"].as_str().unwrap());
            let journal = crash_journal(dir.path(), &backup, true);
            write_journal(dir.path(), &journal).unwrap();
            let path = if change_backup {
                backup.join("1.js")
            } else {
                dir.path().join("scripts/1.js")
            };
            fs::write(&path, "// unrelated edit").unwrap();
            assert!(recover(dir.path()).is_err());
            assert_eq!(fs::read_to_string(path).unwrap(), "// unrelated edit");
            assert!(dir.path().join(PENDING).exists());
        }
    }

    #[test]
    fn fingerprints_include_row_identity_types_blobs_and_unknown_tables() {
        let (dir, _) = fixture();
        let db = open(dir.path(), false).unwrap();
        db.execute_batch("CREATE TABLE extra(value); INSERT INTO extra VALUES(x'01'),(1),('1')")
            .unwrap();
        for mutation in [
            "UPDATE extra SET rowid=8 WHERE rowid=1",
            "UPDATE extra SET value=x'02' WHERE rowid=8",
            "UPDATE extra SET value='1' WHERE typeof(value)='integer'",
        ] {
            let before = fingerprint(&db).unwrap();
            db.execute_batch(mutation).unwrap();
            assert_ne!(fingerprint(&db).unwrap(), before);
        }
    }

    #[test]
    fn prepared_and_wal_recovery_and_legacy_refusal() {
        for wal in [false, true] {
            let (dir, js) = fixture();
            seed(dir.path());
            if wal {
                open(dir.path(), false)
                    .unwrap()
                    .execute_batch("PRAGMA journal_mode=WAL")
                    .unwrap();
            }
            let result = update(dir.path(), &js).unwrap();
            let backup = PathBuf::from(result["backup"].as_str().unwrap());
            let mut journal = crash_journal(dir.path(), &backup, true);
            journal.version = 1;
            write_journal(dir.path(), &journal).unwrap();
            assert!(recover(dir.path()).is_err());
            assert!(dir.path().join(PENDING).exists());
            journal.version = 2;
            write_journal(dir.path(), &journal).unwrap();
            recover(dir.path()).unwrap();
            // A prepared journal also permits the before state, without any loss.
            journal.phase = Phase::Prepared;
            write_journal(dir.path(), &journal).unwrap();
            recover(dir.path()).unwrap();
            assert_eq!(
                fs::read_to_string(dir.path().join("scripts/1.js")).unwrap(),
                "// old"
            );
        }
    }

    #[test]
    fn current_update_preserves_unknown_metadata() {
        let (dir, js) = fixture();
        seed(dir.path());
        let db = open(dir.path(), false).unwrap();
        db.execute_batch(
            "ALTER TABLE scripts ADD COLUMN custom BLOB;
            ALTER TABLE profiles ADD COLUMN custom TEXT;
            UPDATE scripts SET custom=x'00ff'; UPDATE profiles SET custom='keep';
            CREATE TABLE unrelated(value TEXT); INSERT INTO unrelated VALUES('keep');",
        )
        .unwrap();
        drop(db);
        update(dir.path(), &js).unwrap();
        let db = open(dir.path(), false).unwrap();
        assert_eq!(
            db.query_row("SELECT custom FROM scripts WHERE id=1", [], |r| r
                .get::<_, Vec<u8>>(0))
                .unwrap(),
            vec![0, 255]
        );
        assert_eq!(
            db.query_row("SELECT custom FROM profiles WHERE id=7", [], |r| r
                .get::<_, String>(0))
                .unwrap(),
            "keep"
        );
        assert_eq!(
            db.query_row("SELECT value FROM unrelated", [], |r| r.get::<_, String>(0))
                .unwrap(),
            "keep"
        );
    }

    #[test]
    fn expected_identity_hash_and_all_references_checked_before_writes() {
        for mutation in [
            "INSERT INTO profiles VALUES(8,'none',1)",
            "UPDATE profiles SET extend_type='none' WHERE id=7",
            "UPDATE scripts SET label='good-cat-other'; INSERT INTO scripts VALUES(2,'good-cat-cat',0,NULL,0)",
        ] {
            let (dir, js) = fixture();
            seed(dir.path());
            open(dir.path(), false).unwrap().execute_batch(mutation).unwrap();
            let before = fingerprint(&open(dir.path(), false).unwrap()).unwrap();
            assert!(update(dir.path(), &js).is_err());
            assert_eq!(fingerprint(&open(dir.path(), false).unwrap()).unwrap(), before);
            assert_eq!(fs::read_to_string(dir.path().join("scripts/1.js")).unwrap(), "// old");
            assert!(!dir.path().join(PENDING).exists());
        }
        let (dir, js) = fixture();
        seed(dir.path());
        fs::write(dir.path().join("scripts/1.js"), "// external edit").unwrap();
        assert!(update(dir.path(), &js).is_err());
        assert_eq!(
            fs::read_to_string(dir.path().join("scripts/1.js")).unwrap(),
            "// external edit"
        );
        // No binding, active or inactive, makes generic install an overwrite API.
        open(dir.path(), false)
            .unwrap()
            .execute_batch("DELETE FROM profiles")
            .unwrap();
        for allow in [false, true] {
            assert!(register(dir.path(), &js, "good-cat-cat", allow, None).is_err());
        }
    }

    #[test]
    fn dangling_refs_overflow_and_failed_new_install() {
        let (dir, js) = fixture();
        let db = open(dir.path(), false).unwrap();
        db.execute_batch("UPDATE profiles SET extend_type='script',script_id=1;")
            .unwrap();
        let result = register(dir.path(), &js, "good-cat-cat", false, None).unwrap();
        assert_eq!(result["id"], 2);
        assert_eq!(active(&db, 1).unwrap(), vec![7]);
        db.execute_batch("CREATE TRIGGER fail BEFORE INSERT ON scripts BEGIN SELECT RAISE(ABORT,'injected insert failure'); END;").unwrap();
        assert!(register(dir.path(), &js, "good-cat-other", false, None).is_err());
        assert!(!dir.path().join("scripts/3.js").exists());
        assert!(metadata(&db, "good-cat-other").unwrap().is_empty());
        db.execute_batch("DROP TRIGGER fail; INSERT INTO scripts(id,label,last_update_time) VALUES(9223372036854775807,'max',0);").unwrap();
        assert!(register(dir.path(), &js, "good-cat-overflow", false, None).is_err());
    }
    #[test]
    fn inspection_preserves_wal_files() {
        let (dir, _) = fixture();
        let db = open(dir.path(), false).unwrap();
        db.execute_batch(
            "PRAGMA journal_mode=WAL; INSERT INTO scripts VALUES(1,'good-cat-wal',1,NULL,NULL);",
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
        assert_eq!(
            inspect(dir.path(), "good-cat-wal").unwrap()["scripts"][0]["id"],
            1
        );
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
        let result = register(dir.path(), &js, "good-cat-cat", false, None).unwrap();
        let backup = PathBuf::from(result["backup"].as_str().unwrap());
        let alias = dir.path().join("skill-backups/good-cat-alias");
        symlink(&backup, &alias).unwrap();
        let journal = Journal {
            backup: "skill-backups/good-cat-alias".into(),
            ..crash_journal(dir.path(), &backup, false)
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
        assert!(inspect(dir.path(), "good-cat-cat").unwrap()["scripts"]
            .as_array()
            .unwrap()
            .is_empty());
    }
    #[test]
    fn missing_database_bad_schema_and_empty_input() {
        let missing = tempfile::tempdir().unwrap();
        assert!(inspect(missing.path(), "good-cat-cat").is_err());
        assert!(!missing.path().join("database.sqlite").exists());
        let (dir, js) = fixture();
        fs::write(&js, " \n\t").unwrap();
        assert!(register(dir.path(), &js, "good-cat-cat", false, None).is_err());
        fs::write(&js, [0xff]).unwrap();
        assert!(register(dir.path(), &js, "good-cat-cat", false, None).is_err());
        fs::write(&js, "// ok").unwrap();
        open(dir.path(), false)
            .unwrap()
            .execute_batch("ALTER TABLE profiles RENAME COLUMN script_id TO wrong;")
            .unwrap();
        assert!(register(dir.path(), &js, "good-cat-cat", false, None).is_err());
        assert!(!dir.path().join("skill-backups").exists());
    }
}
