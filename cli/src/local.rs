//! All tool-owned state stays under the Agent's current directory.
use anyhow::{bail, Result};
use serde_json::{json, Value};
use std::{
    fs,
    path::{Path, PathBuf},
};

pub const DEFAULT_HOME: &str = ".good-cat";
const MARKER: &str = ".good-cat-home.json";

fn private_dir(path: &Path) -> Result<()> {
    fs::create_dir_all(path)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(path, fs::Permissions::from_mode(0o700))?;
    }
    Ok(())
}

/// Create/resolve a project-local home and redirect all process temp files into it.
pub fn configure(raw: &Path) -> Result<PathBuf> {
    let cwd = fs::canonicalize(std::env::current_dir()?)?;
    let requested = if raw.is_absolute() {
        raw.to_owned()
    } else {
        cwd.join(raw)
    };
    private_dir(&requested)?;
    let home = fs::canonicalize(&requested)?;
    if home != cwd && !home.starts_with(&cwd) {
        bail!("--home 必须位于 Agent 当前目录内，不能使用全局或外部目录");
    }
    let marker = home.join(MARKER);
    match fs::symlink_metadata(&marker) {
        Ok(meta) if meta.is_file() && !meta.file_type().is_symlink() => {
            let value: Value = serde_json::from_slice(&fs::read(&marker)?)?;
            if value.get("version").and_then(Value::as_u64) != Some(1) {
                bail!("不支持的 .good-cat 目录版本");
            }
        }
        Ok(_) => bail!(".good-cat 标记不是普通文件"),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            if fs::read_dir(&home)?.next().is_some() {
                bail!(".good-cat 已存在且不是 good-cat 创建，拒绝接管；请改用空的本地目录");
            }
            crate::platform::private_write(
                &marker,
                &serde_json::to_vec(
                    &json!({"version":1,"created_at":crate::platform::timestamp()}),
                )?,
            )?;
        }
        Err(error) => return Err(error.into()),
    }
    for name in ["bin", "workspaces", "tmp", "backups", "state"] {
        private_dir(&home.join(name))?;
    }
    let tmp = home.join("tmp");
    std::env::set_var("GOOD_CAT_HOME", &home);
    std::env::set_var("TMPDIR", &tmp);
    std::env::set_var("TEMP", &tmp);
    std::env::set_var("TMP", &tmp);
    Ok(home)
}

pub fn home() -> Option<PathBuf> {
    std::env::var_os("GOOD_CAT_HOME").map(PathBuf::from)
}

/// Prefer the project-local state path; keep a caller fallback for library use/tests.
pub fn state_file(name: &str, fallback: impl AsRef<Path>) -> PathBuf {
    home()
        .map(|home| home.join("state").join(name))
        .unwrap_or_else(|| fallback.as_ref().to_owned())
}

pub fn backup_root(fallback: impl AsRef<Path>) -> PathBuf {
    home()
        .map(|home| home.join("backups"))
        .unwrap_or_else(|| fallback.as_ref().to_owned())
}

pub fn temporary_root() -> PathBuf {
    home()
        .map(|home| home.join("tmp"))
        .unwrap_or_else(std::env::temp_dir)
}

fn normalize(path: &Path) -> PathBuf {
    #[cfg(windows)]
    {
        let text = path.as_os_str().to_string_lossy();
        if let Some(stripped) = text.strip_prefix(r"\\?\") {
            return PathBuf::from(stripped);
        }
    }
    path.to_owned()
}

fn relative_to(path: &Path, cwd: &Path) -> Option<PathBuf> {
    path.strip_prefix(cwd).ok().map(Path::to_owned)
}

/// Relative to the Agent directory when possible; absolute only outside it.
pub fn display(path: &Path) -> String {
    let path = normalize(path);
    let cwd = std::env::current_dir().ok().map(|cwd| normalize(&cwd));
    let relative = cwd
        .as_deref()
        .and_then(|cwd| relative_to(&path, cwd))
        .or_else(|| {
            let cwd = fs::canonicalize(std::env::current_dir().ok()?).ok()?;
            relative_to(&normalize(&path), &normalize(&cwd))
        });
    match relative {
        Some(relative) if !relative.as_os_str().is_empty() => {
            format!("./{}", relative.display())
        }
        _ => path.display().to_string(),
    }
}

pub fn layout(home: &Path) -> Value {
    json!({
        "home": display(home),
        "home_absolute": home,
        "bin": display(&home.join("bin")),
        "workspaces": display(&home.join("workspaces")),
        "tmp": display(&home.join("tmp")),
        "backups": display(&home.join("backups")),
        "state": display(&home.join("state")),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn display_prefers_relative_paths_under_the_agent_directory() {
        let cwd = std::env::current_dir().unwrap();
        let path = cwd.join(".good-cat/workspaces/example");
        assert_eq!(display(&path), "./.good-cat/workspaces/example");
    }
}
