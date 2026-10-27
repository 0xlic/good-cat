use anyhow::{bail, ensure, Context, Result};
use fs2::FileExt;
use serde_json::{json, Value};
use std::{
    fs::{self, File, OpenOptions},
    io::{Read, Write},
    path::{Path, PathBuf},
    process::{Command, Stdio},
    thread,
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};

pub fn private_write(path: &Path, bytes: &[u8]) -> Result<()> {
    if let Some(parent) = path.parent().filter(|p| !p.as_os_str().is_empty()) {
        fs::create_dir_all(parent)?;
    }
    let parent = path
        .parent()
        .filter(|p| !p.as_os_str().is_empty())
        .unwrap_or(Path::new("."));
    let mut temporary = tempfile::NamedTempFile::new_in(parent)?;
    temporary.write_all(bytes)?;
    temporary.as_file().sync_all()?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        temporary
            .as_file()
            .set_permissions(fs::Permissions::from_mode(0o600))?;
    }
    temporary.persist(path).map_err(|e| e.error)?;
    sync_dir(parent)?;
    Ok(())
}

pub fn sync_dir(path: &Path) -> Result<()> {
    #[cfg(unix)]
    {
        File::open(path)?.sync_all()?;
    }
    #[cfg(not(unix))]
    {
        let _ = path;
    } // File contents flushed; Windows has no POSIX directory fsync.
    Ok(())
}

pub fn remove_durable(path: &Path) -> Result<()> {
    fs::remove_file(path)?;
    if let Some(parent) = path.parent() {
        sync_dir(parent)?;
    }
    Ok(())
}

pub fn home() -> Result<PathBuf> {
    std::env::var_os(if cfg!(windows) { "USERPROFILE" } else { "HOME" })
        .map(PathBuf::from)
        .context("无法确定用户目录")
}

pub fn default_support() -> Result<PathBuf> {
    if cfg!(target_os = "macos") {
        return Ok(home()?.join("Library/Application Support/com.follow.clash"));
    }
    if cfg!(windows) {
        let base = std::env::var_os("APPDATA")
            .map(PathBuf::from)
            .context("缺少 APPDATA，请用 --support-dir")?;
        let candidates = [
            base.join("com.follow/clash"),
            base.join("com.follow.clash"),
            base.join("FlClash"),
        ];
        let found: Vec<_> = candidates
            .iter()
            .filter(|p| p.join("database.sqlite").is_file())
            .collect();
        if found.len() > 1 {
            bail!("发现多个数据目录，请使用 --support-dir");
        }
        return Ok(found
            .first()
            .map(|p| (*p).clone())
            .unwrap_or(candidates[0].clone()));
    }
    bail!("FlClash 管理仅支持 macOS/Windows；测试可用 --support-dir 指定临时目录")
}

pub struct Lock {
    _file: File,
}
impl Lock {
    pub fn acquire(support: &Path) -> Result<Self> {
        if !support.is_dir() {
            bail!("FlClash 数据目录不存在");
        }
        let path = crate::local::state_file("good-cat.lock", support.join(".good-cat.lock"));
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent)?;
        }
        let file = OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .open(path)?;
        file.try_lock_exclusive()
            .context("另一个 good-cat 正在修改 FlClash")?;
        Ok(Self { _file: file })
    }
}

// Bounded native helper execution; output is private and never included in errors.
pub fn command(program: &str, args: &[&str], timeout: Duration) -> Result<(i32, Vec<u8>)> {
    let output = tempfile::tempfile_in(crate::local::temporary_root())?;
    let error = tempfile::tempfile_in(crate::local::temporary_root())?;
    let mut child = Command::new(program)
        .args(args)
        .stdin(Stdio::null())
        .stdout(output.try_clone()?)
        .stderr(error)
        .spawn()
        .with_context(|| format!("无法启动系统工具 {program}"))?;
    let deadline = Instant::now() + timeout;
    let status = loop {
        if let Some(status) = child.try_wait()? {
            break status;
        }
        if Instant::now() >= deadline {
            let _ = child.kill();
            let _ = child.wait();
            bail!("系统工具 {program} 执行超时");
        }
        thread::sleep(Duration::from_millis(100));
    };
    use std::io::{Seek, SeekFrom};
    let mut output = output;
    output.seek(SeekFrom::Start(0))?;
    let mut bytes = Vec::new();
    output.take(4 * 1024 * 1024).read_to_end(&mut bytes)?;
    Ok((status.code().unwrap_or(-1), bytes))
}

pub fn running() -> Result<bool> {
    if cfg!(target_os = "macos") {
        let (status, _) = command("/usr/bin/pgrep", &["-x", "FlClash"], Duration::from_secs(5))?;
        return match status {
            0 => Ok(true),
            1 => Ok(false),
            _ => bail!("无法检测 FlClash 进程"),
        };
    }
    if cfg!(windows) {
        let (status, bytes) = command(
            "tasklist",
            &["/FI", "IMAGENAME eq FlClash.exe", "/FO", "CSV", "/NH"],
            Duration::from_secs(10),
        )?;
        if status != 0 {
            bail!("无法检测 FlClash 进程");
        }
        return Ok(String::from_utf8_lossy(&bytes).lines().any(|line| {
            line.trim_start_matches('"')
                .to_ascii_lowercase()
                .starts_with("flclash.exe\",")
        }));
    }
    bail!("当前系统不支持 FlClash 进程控制")
}

fn validate_app(path: &Path) -> Result<PathBuf> {
    let path = fs::canonicalize(path).context("FlClash 应用路径不存在")?;
    if cfg!(target_os = "macos") {
        ensure!(path.is_dir(), "macOS 的 --app 必须指向 FlClash.app");
        ensure!(
            path.extension()
                .and_then(|value| value.to_str())
                .is_some_and(|value| value.eq_ignore_ascii_case("app")),
            "macOS 的 --app 必须指向 .app 应用包"
        );
    } else if cfg!(windows) {
        ensure!(path.is_file(), "Windows 的 --app 必须指向 FlClash.exe");
        ensure!(
            path.extension()
                .and_then(|value| value.to_str())
                .is_some_and(|value| value.eq_ignore_ascii_case("exe")),
            "Windows 的 --app 必须指向 .exe"
        );
    } else {
        bail!("当前系统不支持 FlClash 应用路径");
    }
    Ok(path)
}

fn push_candidate(candidates: &mut Vec<PathBuf>, path: impl AsRef<Path>) {
    let path = path.as_ref();
    let Ok(path) = fs::canonicalize(path) else {
        return;
    };
    let valid = if cfg!(target_os = "macos") {
        path.is_dir()
            && path
                .extension()
                .and_then(|value| value.to_str())
                .is_some_and(|value| value.eq_ignore_ascii_case("app"))
            && !path.components().any(|part| part.as_os_str() == ".Trash")
    } else if cfg!(windows) {
        path.is_file()
            && path
                .extension()
                .and_then(|value| value.to_str())
                .is_some_and(|value| value.eq_ignore_ascii_case("exe"))
    } else {
        false
    };
    if valid && !candidates.contains(&path) {
        candidates.push(path);
    }
}

/// Discover installed apps without assuming a standard installation directory.
/// Running process, LaunchServices/Spotlight on macOS, and process/PATH/registry/
/// Start Menu on Windows are consulted. No path is guessed from a package manager.
pub fn app_candidates() -> Result<Vec<PathBuf>> {
    let mut candidates = Vec::new();
    if cfg!(target_os = "macos") {
        if let Ok((0, bytes)) = command(
            "/usr/bin/lsappinfo",
            &["find", "bundleID=com.follow.clash"],
            Duration::from_secs(5),
        ) {
            if let Some(asn) = String::from_utf8_lossy(&bytes)
                .lines()
                .find(|line| line.starts_with("ASN:"))
            {
                if let Ok((0, info)) = command(
                    "/usr/bin/lsappinfo",
                    &["info", "-only", "bundlepath", asn.trim()],
                    Duration::from_secs(5),
                ) {
                    if let Some(path) = String::from_utf8_lossy(&info)
                        .split("\"LSBundlePath\"=\"")
                        .nth(1)
                        .and_then(|value| value.strip_suffix('"'))
                    {
                        push_candidate(&mut candidates, path);
                    }
                }
            }
        }
        if let Ok((0, bytes)) = command(
            "/usr/bin/osascript",
            &[
                "-e",
                "POSIX path of (path to application id \"com.follow.clash\")",
            ],
            Duration::from_secs(5),
        ) {
            let path = String::from_utf8_lossy(&bytes);
            push_candidate(&mut candidates, path.trim());
        }
        if let Ok((0, bytes)) = command(
            "/usr/bin/mdfind",
            &["kMDItemCFBundleIdentifier == 'com.follow.clash'"],
            Duration::from_secs(5),
        ) {
            for line in String::from_utf8_lossy(&bytes).lines() {
                push_candidate(&mut candidates, line.trim());
            }
        }
    } else if cfg!(windows) {
        let script = r#"
$ErrorActionPreference = 'SilentlyContinue'
[Console]::OutputEncoding = [System.Text.Encoding]::UTF8
$items = @()
$process = Get-Process -Name FlClash | Select-Object -First 1 -ExpandProperty Path
if ($process) { $items += $process }
$command = Get-Command FlClash.exe
if ($command) { $items += $command.Source }
$appPaths = @(
  'HKCU:\Software\Microsoft\Windows\CurrentVersion\App Paths\FlClash.exe',
  'HKLM:\Software\Microsoft\Windows\CurrentVersion\App Paths\FlClash.exe',
  'HKLM:\Software\WOW6432Node\Microsoft\Windows\CurrentVersion\App Paths\FlClash.exe'
)
foreach ($key in $appPaths) {
  $value = (Get-ItemProperty -LiteralPath $key).'(default)'
  if ($value) { $items += $value }
}
$shell = New-Object -ComObject WScript.Shell
$roots = @(
  (Join-Path $env:APPDATA 'Microsoft\Windows\Start Menu\Programs'),
  (Join-Path $env:ProgramData 'Microsoft\Windows\Start Menu\Programs'),
  (Join-Path $env:USERPROFILE 'Desktop')
)
foreach ($root in $roots) {
  Get-ChildItem -LiteralPath $root -Filter 'FlClash*.lnk' -Recurse |
    ForEach-Object {
      $target = $shell.CreateShortcut($_.FullName).TargetPath
      if ($target) { $items += $target }
    }
}
$items | Where-Object { $_ -and (Test-Path -LiteralPath $_ -PathType Leaf) } | Sort-Object -Unique
"#;
        if let Ok((0, bytes)) = command(
            "powershell.exe",
            &["-NoProfile", "-NonInteractive", "-Command", script],
            Duration::from_secs(15),
        ) {
            for line in String::from_utf8_lossy(&bytes)
                .trim_start_matches('\u{feff}')
                .lines()
            {
                push_candidate(&mut candidates, line.trim());
            }
        }
    } else {
        bail!("当前系统不支持 FlClash 应用发现");
    }
    Ok(candidates)
}

/// Report the selected app and alternatives. Ambiguity is left for the Agent/user
/// to resolve via `--app`; this function never silently chooses between installs.
pub fn app_info(explicit: Option<&Path>) -> Result<Value> {
    if let Some(path) = explicit {
        let path = validate_app(path)?;
        return Ok(
            json!({"path": path, "source": "explicit", "candidates": [path], "ambiguous": false}),
        );
    }
    let candidates = app_candidates()?;
    let path = if candidates.len() == 1 {
        Some(candidates[0].clone())
    } else {
        None
    };
    Ok(json!({
        "path": path,
        "source": if candidates.is_empty() { "not_found" } else if candidates.len() == 1 { "detected" } else { "ambiguous" },
        "candidates": candidates,
        "ambiguous": candidates.len() > 1,
    }))
}

pub fn app_path(explicit: Option<&Path>) -> Result<PathBuf> {
    if let Some(path) = explicit {
        return validate_app(path);
    }
    let candidates = app_candidates()?;
    match candidates.as_slice() {
        [path] => Ok(path.clone()),
        [] => bail!("未自动发现 FlClash 安装位置；请向用户确认安装路径并使用 --app 指定"),
        _ => bail!(
            "发现多个 FlClash 安装位置，必须确认后使用 --app 指定：{}",
            candidates
                .iter()
                .map(|path| path.display().to_string())
                .collect::<Vec<_>>()
                .join(", ")
        ),
    }
}

fn wait(expected: bool, timeout: Duration) -> Result<()> {
    let deadline = Instant::now() + timeout;
    loop {
        if running()? == expected {
            return Ok(());
        }
        if Instant::now() >= deadline {
            bail!(
                "等待 FlClash {}超时",
                if expected { "启动" } else { "退出" }
            );
        }
        thread::sleep(Duration::from_millis(200));
    }
}

pub fn stop(app: &Path, timeout: Duration) -> Result<()> {
    if !running()? {
        return Ok(());
    }
    eprintln!("即将退出 FlClash，代理可能短暂中断；同一进程会继续恢复启动。");
    if cfg!(target_os = "macos") {
        let _ = command(
            "/usr/bin/osascript",
            &[
                "-e",
                "on run argv\ntell application (item 1 of argv) to quit\nend run",
                app.to_str().context("应用路径编码不支持")?,
            ],
            timeout,
        );
    } else if cfg!(windows) {
        let _ = command("taskkill", &["/IM", "FlClash.exe"], timeout); // Never /F.
    } else {
        bail!("不支持的系统");
    }
    wait(false, timeout).context("未强制终止，请从托盘/菜单真正退出后重试")
}

pub fn start(app: &Path, timeout: Duration) -> Result<()> {
    if running()? {
        return Ok(());
    }
    if cfg!(target_os = "macos") {
        let (status, _) = command(
            "/usr/bin/open",
            &["-a", app.to_str().context("应用路径编码不支持")?],
            timeout,
        )?;
        if status != 0 {
            bail!("无法启动 FlClash");
        }
    } else if cfg!(windows) {
        Command::new(app)
            .current_dir(app.parent().unwrap_or(Path::new(".")))
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()?;
    } else {
        bail!("不支持的系统");
    }
    wait(true, timeout)
}

pub fn timestamp() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
}

pub fn backup_dir(support: &Path) -> Result<PathBuf> {
    let root = crate::local::backup_root(support.join("skill-backups"));
    fs::create_dir_all(&root)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(&root, fs::Permissions::from_mode(0o700))?;
    }
    let temp = tempfile::Builder::new()
        .prefix(&format!("good-cat-{}-", timestamp()))
        .tempdir_in(&root)?;
    let path = temp.keep();
    sync_dir(&path)?;
    if let Some(parent) = path.parent() {
        sync_dir(parent)?;
    }
    if let Some(parent) = root.parent() {
        sync_dir(parent)?;
    }
    Ok(path)
}

#[derive(Clone)]
pub struct Preferences {
    pub path: Option<PathBuf>,
    pub support: PathBuf,
}
impl Preferences {
    fn file_path(&self) -> PathBuf {
        self.path
            .clone()
            .unwrap_or_else(|| self.support.join("shared_preferences.json"))
    }
    pub fn load(&self) -> Result<Value> {
        #[cfg(target_os = "macos")]
        if self.path.is_none() {
            return serde_json::from_str(&mac_preferences::read()?)
                .context("FlClash flutter.config 不是有效 JSON");
        }
        let path = self.file_path();
        let value = if path.extension().is_some_and(|e| e == "plist") {
            let value = plist::Value::from_file(path)?;
            value
                .as_dictionary()
                .and_then(|d| d.get("flutter.config"))
                .and_then(|v| v.as_string())
                .context("plist 缺少字符串 flutter.config")?
                .to_owned()
        } else {
            let value: Value = serde_json::from_slice(&fs::read(path)?)?;
            value
                .get("flutter.config")
                .and_then(Value::as_str)
                .context("共享设置缺少字符串 flutter.config，拒绝猜测结构")?
                .to_owned()
        };
        let config: Value = serde_json::from_str(&value)?;
        if !config.is_object() {
            bail!("flutter.config 必须为 JSON 对象");
        }
        Ok(config)
    }
    pub fn save(&self, config: &Value) -> Result<()> {
        let text = serde_json::to_string(config)?;
        #[cfg(target_os = "macos")]
        if self.path.is_none() {
            mac_preferences::write(&text)?;
            if self.load()? != *config {
                bail!("CFPreferences 写入回读不一致");
            }
            return Ok(());
        }
        let path = self.file_path();
        if path.extension().is_some_and(|e| e == "plist") {
            let mut value = plist::Value::from_file(&path)?;
            value
                .as_dictionary_mut()
                .context("plist 不是字典")?
                .insert("flutter.config".into(), plist::Value::String(text));
            let mut bytes = Vec::new();
            value.to_writer_binary(&mut bytes)?;
            private_write(&path, &bytes)?;
        } else {
            let mut value: Value = serde_json::from_slice(&fs::read(&path)?)?;
            value
                .as_object_mut()
                .context("共享设置不是对象")?
                .insert("flutter.config".into(), Value::String(text));
            private_write(&path, &serde_json::to_vec_pretty(&value)?)?;
        }
        if self.load()? != *config {
            bail!("设置写入回读不一致");
        }
        Ok(())
    }
    pub fn controller(&self) -> Result<(String, String)> {
        let config = self.load()?;
        let patch = config
            .get("patchClashConfig")
            .and_then(Value::as_object)
            .context("版本不支持：缺少 patchClashConfig")?;
        let endpoint = patch
            .get("external-controller")
            .and_then(Value::as_str)
            .unwrap_or("")
            .to_owned();
        let secret = patch
            .get("secret")
            .and_then(Value::as_str)
            .unwrap_or("")
            .to_owned();
        Ok((endpoint, secret))
    }
}

pub fn controller_config(config: &mut Value, endpoint: &str, secret: &str) -> Result<()> {
    let patch = config
        .get_mut("patchClashConfig")
        .and_then(Value::as_object_mut)
        .context("缺少 patchClashConfig，拒绝猜测版本结构")?;
    for key in ["external-controller", "secret"] {
        if patch.get(key).is_some_and(|v| !v.is_string()) {
            bail!("不支持的 controller 设置类型");
        }
    }
    patch.insert("external-controller".into(), json!(endpoint));
    patch.insert("secret".into(), json!(secret));
    Ok(())
}

pub fn enabled_summary(preferences: &Preferences) -> Result<Value> {
    let (endpoint, secret) = preferences.controller()?;
    let config = preferences.load()?;
    Ok(
        json!({"controller": endpoint, "secret_configured": !secret.is_empty(), "current_profile_id": config.get("currentProfileId")}),
    )
}

#[cfg(target_os = "macos")]
mod mac_preferences {
    use super::*;
    use anyhow::anyhow;
    use std::{
        ffi::{c_char, c_void},
        ptr,
    };
    type Ref = *const c_void;
    #[link(name = "CoreFoundation", kind = "framework")]
    unsafe extern "C" {
        fn CFStringCreateWithBytes(
            allocator: Ref,
            bytes: *const u8,
            len: isize,
            encoding: u32,
            external: u8,
        ) -> Ref;
        fn CFPreferencesCopyAppValue(key: Ref, app: Ref) -> Ref;
        fn CFPreferencesSetAppValue(key: Ref, value: Ref, app: Ref);
        fn CFPreferencesAppSynchronize(app: Ref) -> u8;
        fn CFGetTypeID(value: Ref) -> usize;
        fn CFStringGetTypeID() -> usize;
        fn CFStringGetLength(value: Ref) -> isize;
        fn CFStringGetMaximumSizeForEncoding(length: isize, encoding: u32) -> isize;
        fn CFStringGetCString(value: Ref, buffer: *mut c_char, size: isize, encoding: u32) -> u8;
        fn CFRelease(value: Ref);
    }
    const UTF8: u32 = 0x08000100;
    struct Owned(Ref);
    impl Drop for Owned {
        fn drop(&mut self) {
            if !self.0.is_null() {
                unsafe { CFRelease(self.0) }
            }
        }
    }
    fn string(text: &str) -> Result<Owned> {
        let value = unsafe {
            CFStringCreateWithBytes(ptr::null(), text.as_ptr(), text.len() as isize, UTF8, 0)
        };
        if value.is_null() {
            bail!("CFString 创建失败");
        }
        Ok(Owned(value))
    }
    pub fn read() -> Result<String> {
        let key = string("flutter.config")?;
        let domain = string("com.follow.clash")?;
        unsafe {
            CFPreferencesAppSynchronize(domain.0);
        }
        let value = Owned(unsafe { CFPreferencesCopyAppValue(key.0, domain.0) });
        if value.0.is_null() || unsafe { CFGetTypeID(value.0) != CFStringGetTypeID() } {
            bail!("CFPreferences 缺少字符串 flutter.config");
        }
        let size =
            unsafe { CFStringGetMaximumSizeForEncoding(CFStringGetLength(value.0), UTF8) } + 1;
        if !(1..=16 * 1024 * 1024).contains(&size) {
            bail!("设置字符串过大");
        }
        let mut buffer = vec![0u8; size as usize];
        if unsafe { CFStringGetCString(value.0, buffer.as_mut_ptr().cast(), size, UTF8) } == 0 {
            bail!("CFPreferences UTF-8 解码失败");
        }
        let end = buffer.iter().position(|b| *b == 0).unwrap_or(buffer.len());
        String::from_utf8(buffer[..end].to_vec()).map_err(|_| anyhow!("设置字符串不是 UTF-8"))
    }
    pub fn write(text: &str) -> Result<()> {
        let key = string("flutter.config")?;
        let domain = string("com.follow.clash")?;
        let value = string(text)?;
        unsafe {
            CFPreferencesSetAppValue(key.0, value.0, domain.0);
        }
        if unsafe { CFPreferencesAppSynchronize(domain.0) } == 0 {
            bail!("CFPreferences 持久化失败");
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn edits_only_controller_fields() {
        let mut config = json!({"currentProfileId":369025290967453696i64,"patchClashConfig":{"external-controller":"", "secret":"", "general-override-keys":["ipv6"], "unknown":true}});
        controller_config(&mut config, "127.0.0.1:9090", "test").unwrap();
        assert_eq!(config["currentProfileId"], json!(369025290967453696i64));
        assert_eq!(config["patchClashConfig"]["unknown"], true);
        assert_eq!(
            config["patchClashConfig"]["general-override-keys"],
            json!(["ipv6"])
        );
    }
    #[test]
    fn preferences_roundtrip_preserves_outer_keys() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("shared_preferences.json");
        let initial =
            json!({"patchClashConfig":{"external-controller":"", "secret":""},"unknown":123});
        fs::write(
            &path,
            serde_json::to_vec(&json!({"flutter.config":initial.to_string(), "other":"preserved"}))
                .unwrap(),
        )
        .unwrap();
        let prefs = Preferences {
            path: Some(path.clone()),
            support: dir.path().to_owned(),
        };
        let mut config = prefs.load().unwrap();
        controller_config(&mut config, "127.0.0.1:9090", "test").unwrap();
        prefs.save(&config).unwrap();
        assert_eq!(
            prefs.controller().unwrap(),
            ("127.0.0.1:9090".into(), "test".into())
        );
        let outer: Value = serde_json::from_slice(&fs::read(path).unwrap()).unwrap();
        assert_eq!(outer["other"], "preserved");
    }
    #[cfg(any(target_os = "macos", windows))]
    #[test]
    fn explicit_app_path_is_validated_and_reported() {
        let dir = tempfile::tempdir().unwrap();
        #[cfg(target_os = "macos")]
        let app = {
            let app = dir.path().join("FlClash.app");
            fs::create_dir(&app).unwrap();
            app
        };
        #[cfg(windows)]
        let app = {
            let app = dir.path().join("FlClash.exe");
            fs::write(&app, []).unwrap();
            app
        };
        let expected = app.canonicalize().unwrap();
        let info = app_info(Some(&app)).unwrap();
        assert_eq!(info["source"], "explicit");
        assert_eq!(PathBuf::from(info["path"].as_str().unwrap()), expected);
        assert_eq!(app_path(Some(&app)).unwrap(), expected);
        let wrong = dir.path().join(if cfg!(windows) {
            "FlClash.app"
        } else {
            "FlClash.exe"
        });
        if cfg!(windows) {
            fs::create_dir(&wrong).unwrap();
        } else {
            fs::write(&wrong, []).unwrap();
        }
        assert!(app_info(Some(&wrong)).is_err());
        assert!(app_path(Some(&dir.path().join("missing"))).is_err());
    }

    #[test]
    fn locks_release_automatically() {
        let dir = tempfile::tempdir().unwrap();
        let a = Lock::acquire(dir.path()).unwrap();
        assert!(Lock::acquire(dir.path()).is_err());
        drop(a);
        assert!(Lock::acquire(dir.path()).is_ok());
    }
}
