//! Per-flow artifacts live in a private, persistent OS temporary directory.
use anyhow::{bail, Context, Result};
use serde_json::{json, Value};
use std::{
    fs,
    path::{Path, PathBuf},
};

const MARKER: &str = ".good-cat-workspace.json";
pub const REPORT: &str = "speedtest-results.json";
pub const SELECTION: &str = "selection.json";
pub const SCRIPT: &str = "good-cat.generated.js";

pub fn create() -> Result<PathBuf> {
    let directory = tempfile::Builder::new().prefix("good-cat-").tempdir()?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(directory.path(), fs::Permissions::from_mode(0o700))?;
    }
    let path = directory.keep();
    let result = crate::platform::private_write(
        &path.join(MARKER),
        &serde_json::to_vec(&json!({"version":1,"created_at":crate::platform::timestamp()}))?,
    );
    if let Err(error) = result {
        let _ = fs::remove_dir_all(&path);
        return Err(error);
    }
    match validate(&path) {
        Ok(path) => Ok(path),
        Err(error) => {
            let _ = fs::remove_dir_all(&path);
            Err(error)
        }
    }
}

pub fn validate(path: &Path) -> Result<PathBuf> {
    let meta = fs::symlink_metadata(path)
        .context("工作区不存在；系统可能已清理临时目录，请创建新工作区")?;
    if !meta.is_dir() || meta.file_type().is_symlink() {
        bail!("工作区必须是真实目录，不能是符号链接");
    }
    let path = path.canonicalize()?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        if meta.permissions().mode() & 0o077 != 0 {
            bail!("工作区权限过宽，需要仅当前用户可访问");
        }
    }
    let marker = path.join(MARKER);
    let meta = fs::symlink_metadata(&marker)
        .context("目录不是 good-cat 创建的工作区，请使用 workspace create")?;
    if !meta.is_file() || meta.file_type().is_symlink() {
        bail!("工作区标记无效");
    }
    let value: Value = serde_json::from_slice(&fs::read(marker)?)?;
    if value.get("version").and_then(Value::as_u64) != Some(1) {
        bail!("不支持的工作区版本");
    }
    Ok(path)
}

pub fn describe(path: &Path) -> Value {
    json!({"status":"ok","operation":"workspace.create","workspace":path,
        "report":path.join(REPORT),"selection":path.join(SELECTION),"script":path.join(SCRIPT),
        "temporary":true})
}

pub fn explicit_or_default(
    explicit: Option<&Path>,
    workspace: Option<&Path>,
    filename: &str,
) -> Result<PathBuf> {
    if let Some(path) = explicit {
        return Ok(if path.is_absolute() {
            path.to_owned()
        } else {
            std::env::current_dir()?.join(path)
        });
    }
    let workspace = workspace
        .context("请传入 --workspace <workspace create 返回的路径>，或显式指定文件路径")?;
    Ok(validate(workspace)?.join(filename))
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn unique_private_workspaces_in_os_temp() {
        let a = create().unwrap();
        let b = create().unwrap();
        assert_ne!(a, b);
        assert!(a.starts_with(std::env::temp_dir().canonicalize().unwrap()));
        assert_eq!(validate(&a).unwrap(), a);
        assert_eq!(
            explicit_or_default(None, Some(&a), REPORT).unwrap(),
            a.join(REPORT)
        );
        fs::remove_dir_all(a).unwrap();
        fs::remove_dir_all(b).unwrap();
    }
    #[test]
    fn defaults_never_use_current_directory() {
        assert!(explicit_or_default(None, None, REPORT).is_err());
        let dir = tempfile::tempdir().unwrap();
        assert!(validate(dir.path()).is_err());
        let path = explicit_or_default(Some(Path::new("custom.json")), None, REPORT).unwrap();
        assert_eq!(path, std::env::current_dir().unwrap().join("custom.json"));
    }
}
