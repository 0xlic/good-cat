use anyhow::{anyhow, bail, Context, Result};
use clap::{Args, Parser, Subcommand};
use good_cat::{
    api::Api,
    install,
    platform::{self, Preferences},
    render, speedtest, update, workspace,
};
use rand::{distributions::Alphanumeric, Rng};
use serde_json::{json, Value};
use std::{
    fs,
    net::TcpListener,
    path::{Path, PathBuf},
    time::{Duration, Instant},
};

#[derive(Parser)]
#[command(
    name = "good-cat",
    version,
    about = "FlClash API、节点测速、模板生成及安装；默认只预览有副作用的操作"
)]
struct Cli {
    #[arg(long, global = true)]
    json: bool,
    #[arg(long, global = true)]
    support_dir: Option<PathBuf>,
    #[arg(
        long,
        global = true,
        help = "workspace create 返回的系统临时工作区路径"
    )]
    workspace: Option<PathBuf>,
    #[arg(
        long,
        global = true,
        help = "仅用于文件式设置/测试；macOS 默认通过 CFPreferences 读写"
    )]
    preferences: Option<PathBuf>,
    #[arg(long, global = true)]
    app: Option<PathBuf>,
    #[arg(long, global=true, default_value_t=20, value_parser=clap::value_parser!(u64).range(1..=300))]
    lifecycle_timeout: u64,
    #[command(subcommand)]
    command: Commands,
}
#[derive(Subcommand)]
enum Commands {
    Workspace {
        #[command(subcommand)]
        command: WorkspaceCommand,
    },
    Inspect,
    Controller {
        #[command(subcommand)]
        command: ControllerCommand,
    },
    Speedtest(SpeedArgs),
    Render(RenderArgs),
    Install(InstallArgs),
    CurrentScript,
    Update(UpdateArgs),
    Recover {
        #[arg(long)]
        apply: bool,
    },
}
#[derive(Subcommand)]
enum WorkspaceCommand {
    Create,
}

#[derive(Subcommand)]
enum ControllerCommand {
    Status,
    Enable {
        #[arg(long)]
        apply: bool,
        #[arg(long, default_value_t=9090, value_parser=clap::value_parser!(u16).range(9090..=9090), help="FlClash 0.9.2 仅支持枚举端口 9090")]
        port: u16,
    },
    Restore {
        #[arg(long)]
        apply: bool,
        #[arg(long)]
        backup: Option<PathBuf>,
    },
}
#[derive(Args)]
struct SpeedArgs {
    #[arg(long)]
    controller: Option<String>,
    #[arg(long)]
    allow_insecure_controller: bool,
    #[arg(long = "url")]
    urls: Vec<String>,
    #[arg(long,default_value_t=600, value_parser=clap::value_parser!(u64).range(1..=60000))]
    threshold: u64,
    #[arg(long,default_value_t=5000, value_parser=clap::value_parser!(u64).range(1..=300000))]
    timeout: u64,
    #[arg(long,default_value_t=8, value_parser=clap::value_parser!(u8).range(1..=64))]
    concurrency: u8,
    #[arg(long, default_value_t=3, value_parser=clap::value_parser!(u8).range(1..=20))]
    rounds: u8,
    #[arg(long, help = "报告文件；默认写入工作区，未指定工作区时自动创建")]
    output: Option<PathBuf>,
}
#[derive(Args)]
struct RenderArgs {
    #[arg(long, help = "默认工作区内的 speedtest-results.json")]
    report: Option<PathBuf>,
    #[arg(long, help = "默认工作区内的 selection.json")]
    selection: Option<PathBuf>,
    #[arg(long)]
    template: Option<PathBuf>,
    #[arg(long, help = "默认工作区内的 good-cat.generated.js")]
    output: Option<PathBuf>,
    #[arg(long,default_value_t=600,value_parser=clap::value_parser!(u64).range(1..=60000))]
    threshold: u64,
}
#[derive(Args)]
struct InstallArgs {
    #[arg(long, help = "默认工作区内的 good-cat.generated.js")]
    script: Option<PathBuf>,
    #[arg(
        long,
        help = "由 Agent 根据当前订阅名称精简命名，例如 good-cat-订阅简称"
    )]
    name: Option<String>,
    #[arg(long)]
    apply: bool,
    #[arg(long, conflicts_with_all = ["activate_current", "register_only"])]
    activate_profile: Option<i64>,
    #[arg(long, conflicts_with_all = ["activate_profile", "register_only"], help="显式关联当前订阅（默认行为）")]
    activate_current: bool,
    #[arg(long, conflicts_with_all = ["activate_profile", "activate_current"], help="只注册，不关联任何订阅")]
    register_only: bool,
    #[arg(long)]
    start: bool,
}

#[derive(Args)]
struct UpdateArgs {
    #[arg(long, help = "默认工作区内的 good-cat.generated.js")]
    script: Option<PathBuf>,
    #[arg(long)]
    apply: bool,
}

fn current_profile(preferences: &Preferences) -> Result<i64> {
    preferences
        .load()?
        .get("currentProfileId")
        .and_then(Value::as_i64)
        .context("无法读取当前配置 ID")
}

fn output(value: &Value) {
    println!(
        "{}",
        serde_json::to_string_pretty(value).expect("serializable JSON")
    );
}
fn read_json(path: &Path) -> Result<Value> {
    serde_json::from_slice(&fs::read(path).with_context(|| format!("无法读取 {}", path.display()))?)
        .context("JSON 格式无效")
}
fn prefs(cli: &Cli, support: &Path) -> Preferences {
    Preferences {
        path: cli.preferences.clone(),
        support: support.to_owned(),
    }
}
fn same_file(a: &Path, b: &Path) -> bool {
    if a == b {
        return true;
    }
    match (fs::canonicalize(a), fs::canonicalize(b)) {
        (Ok(a), Ok(b)) => a == b,
        _ => false,
    }
}
fn controller_journal(support: &Path) -> PathBuf {
    support.join(".good-cat-pending-controller.json")
}

fn with_stopped<F>(
    cli: &Cli,
    support: &Path,
    start: bool,
    recovering: &str,
    operation: F,
) -> Result<Value>
where
    F: FnOnce() -> Result<Value>,
{
    let _lock = platform::Lock::acquire(support)?;
    if support.join(".good-cat-pending-install.json").exists() && recovering != "install" {
        bail!("存在待恢复的安装，请先 recover --apply；未重启应用");
    }
    if controller_journal(support).exists() && recovering != "controller" {
        bail!("存在待恢复的设置，请先 controller restore --apply；未重启应用");
    }
    let app = platform::app_path(cli.app.as_deref())?;
    let was_running = platform::running()?;
    if let Err(error) = platform::stop(&app, Duration::from_secs(cli.lifecycle_timeout)) {
        let recovery = if was_running {
            platform::start(&app, Duration::from_secs(cli.lifecycle_timeout))
        } else {
            Ok(())
        };
        return Err(match recovery {
            Ok(()) => error,
            Err(restart) => anyhow!("{error:#}; 恢复启动失败：{restart:#}"),
        });
    }
    let result = match platform::running() {
        Ok(false) => operation(),
        Ok(true) => Err(anyhow!("FlClash 意外重新启动，取消修改")),
        Err(error) => Err(error),
    };
    let consistent = result
        .as_ref()
        .err()
        .and_then(|e| e.downcast_ref::<install::RecoveryFailure>())
        .is_none()
        && !support.join(".good-cat-pending-install.json").exists()
        && !controller_journal(support).exists();
    if consistent && (was_running || start) {
        if let Err(error) = platform::start(&app, Duration::from_secs(cli.lifecycle_timeout)) {
            return Err(match result {
                Ok(_) => anyhow!("操作已完成，但恢复启动失败：{error:#}"),
                Err(original) => anyhow!("{original:#}; 恢复启动也失败：{error:#}"),
            });
        }
    }
    result
}

async fn ready(endpoint: &str, secret: &str, timeout: Duration) -> Result<()> {
    let api = Api::new(endpoint, secret, false)?;
    let deadline = Instant::now() + timeout;
    loop {
        if api.version().await.is_ok() {
            return Ok(());
        }
        if Instant::now() >= deadline {
            bail!("API 未在规定时间内就绪");
        }
        tokio::time::sleep(Duration::from_millis(300)).await;
    }
}

fn saved_fields(config: &Value) -> Result<Value> {
    let patch = config
        .get("patchClashConfig")
        .and_then(Value::as_object)
        .context("缺少 patchClashConfig")?;
    Ok(json!({"external-controller":patch.get("external-controller"),"secret":patch.get("secret")}))
}
fn restore_fields(config: &mut Value, fields: &Value) -> Result<()> {
    let patch = config
        .get_mut("patchClashConfig")
        .and_then(Value::as_object_mut)
        .context("缺少 patchClashConfig")?;
    for key in ["external-controller", "secret"] {
        let value = fields.get(key).context("恢复记录缺少 controller 字段")?;
        if value.is_null() {
            patch.remove(key);
        } else if value.is_string() {
            patch.insert(key.into(), value.clone());
        } else {
            bail!("恢复字段类型无效");
        }
    }
    Ok(())
}
fn pending_backup(support: &Path) -> Result<PathBuf> {
    let journal = read_json(&controller_journal(support))?;
    let backup = PathBuf::from(
        journal
            .get("backup")
            .and_then(Value::as_str)
            .context("恢复记录缺少路径")?,
    );
    let actual = fs::canonicalize(&backup)?;
    let root = fs::canonicalize(support.join("skill-backups"))?;
    if !actual.starts_with(root) {
        bail!("恢复记录的备份不在安全目录中");
    }
    Ok(actual)
}

async fn enable_controller(cli: &Cli, support: &Path, port: u16, apply: bool) -> Result<Value> {
    let preferences = prefs(cli, support);
    let (previous, old_secret) = preferences.controller()?;
    let endpoint = format!("127.0.0.1:{port}");
    if !apply {
        return Ok(
            json!({"status":"confirmation_required","operation":"controller.enable","endpoint":endpoint,"may_disconnect":true,"starts_flclash":true,"existing_controller":previous,"secret_configured":!old_secret.is_empty(),"next":"用户确认后添加 --apply"}),
        );
    }
    let _lock = platform::Lock::acquire(support)?;
    if controller_journal(support).exists() {
        bail!("存在未完成的 controller 修改，请先 controller restore --apply");
    }
    if support.join(".good-cat-pending-install.json").exists() {
        bail!("存在未完成的脚本安装，请先 recover --apply");
    }
    let existing_ready = previous == endpoint
        && Api::new(&endpoint, &old_secret, false)?
            .version()
            .await
            .is_ok();
    if existing_ready && !old_secret.is_empty() {
        return Ok(
            json!({"status":"ok","operation":"controller.enable","controller":format!("http://{endpoint}"),"reused":true}),
        );
    }
    // Check conflicts before disconnecting the user's network.
    let reservation = if existing_ready {
        None
    } else {
        Some(
            TcpListener::bind(&endpoint)
                .context("FlClash 固定的 9090 端口被占用；未停止应用或修改设置")?,
        )
    };
    let app = platform::app_path(cli.app.as_deref())?;
    let was_running = platform::running()?;
    let timeout = Duration::from_secs(cli.lifecycle_timeout);
    if let Err(error) = platform::stop(&app, timeout) {
        if was_running {
            if let Err(restart) = platform::start(&app, timeout) {
                return Err(anyhow!("{error:#}; 恢复启动失败：{restart:#}"));
            }
        }
        return Err(error);
    }
    drop(reservation);
    let mut rollback: Option<Value> = None;
    let mut backup_path: Option<PathBuf> = None;
    let secret: String = rand::thread_rng()
        .sample_iter(&Alphanumeric)
        .take(48)
        .map(char::from)
        .collect();
    let mutation = (|| -> Result<()> {
        if platform::running()? {
            bail!("FlClash 意外重新启动，取消修改");
        }
        let mut config = preferences.load()?;
        let fields = saved_fields(&config)?;
        let backup = platform::backup_dir(support)?.join("controller.json");
        platform::private_write(
            &backup,
            &serde_json::to_vec_pretty(&json!({"fields":fields}))?,
        )?;
        platform::private_write(
            &controller_journal(support),
            &serde_json::to_vec(&json!({"backup":backup}))?,
        )?;
        backup_path = Some(backup);
        rollback = Some(fields);
        platform::controller_config(&mut config, &endpoint, &secret)?;
        preferences.save(&config)
    })();
    let attempt = match mutation {
        Ok(()) => match platform::start(&app, timeout) {
            Ok(()) => ready(&endpoint, &secret, timeout).await,
            Err(e) => Err(e),
        },
        Err(e) => Err(e),
    };
    match attempt {
        Ok(()) => {
            platform::remove_durable(&controller_journal(support))?;
            // Environment is process-local; never print it or mutate the user's shell.
            Ok(
                json!({"status":"ok","operation":"controller.enable","controller":format!("http://{endpoint}"),"secret_configured":true,"backup":backup_path,"ready":true,"reused":false}),
            )
        }
        Err(error) => {
            let restore = (|| -> Result<()> {
                if let Some(fields) = rollback {
                    platform::stop(&app, timeout)?;
                    let mut config = preferences.load()?;
                    restore_fields(&mut config, &fields)?;
                    preferences.save(&config)?;
                    platform::remove_durable(&controller_journal(support))?;
                }
                if was_running {
                    platform::start(&app, timeout)?;
                }
                Ok(())
            })();
            match restore {
                Ok(()) => Err(anyhow!("开启 API 失败，已恢复原设置：{error:#}")),
                Err(recovery) => Err(anyhow!(
                    "开启 API 失败：{error:#}; 恢复失败：{recovery:#}; 备份：{}",
                    backup_path
                        .map(|p| p.display().to_string())
                        .unwrap_or_default()
                )),
            }
        }
    }
}

async fn dispatch(cli: &Cli) -> Result<Value> {
    match &cli.command {
        Commands::Workspace {
            command: WorkspaceCommand::Create,
        } => {
            if cli.workspace.is_some() {
                bail!("创建工作区不接受 --workspace；请保留返回的新路径");
            }
            Ok(workspace::describe(&workspace::create()?))
        }
        Commands::Render(args) => {
            let work = cli
                .workspace
                .as_deref()
                .map(workspace::validate)
                .transpose()?;
            let report_path = workspace::explicit_or_default(
                args.report.as_deref(),
                work.as_deref(),
                workspace::REPORT,
            )?;
            let selection_path = workspace::explicit_or_default(
                args.selection.as_deref(),
                work.as_deref(),
                workspace::SELECTION,
            )?;
            let work = if work.is_none() && args.output.is_none() {
                Some(workspace::create()?)
            } else {
                work
            };
            let output_path = workspace::explicit_or_default(
                args.output.as_deref(),
                work.as_deref(),
                workspace::SCRIPT,
            )?;
            let bundled_source =
                Path::new(env!("CARGO_MANIFEST_DIR")).join("../skill/good-cat/assets/good-cat.js");
            if bundled_source.exists() && same_file(&output_path, &bundled_source) {
                bail!("输出不能覆盖内置模板资源");
            }
            let report = read_json(&report_path)?;
            let selection = read_json(&selection_path)?;
            let bundled = std::env::current_exe()?
                .parent()
                .and_then(Path::parent)
                .map(|p| p.join("assets/good-cat.js"));
            let template_path = args
                .template
                .clone()
                .or_else(|| bundled.filter(|p| p.is_file()));
            if same_file(&output_path, &report_path)
                || same_file(&output_path, &selection_path)
                || template_path
                    .as_ref()
                    .is_some_and(|p| same_file(&output_path, p))
            {
                bail!("输出不能覆盖模板、报告或选择文件");
            }
            let template = match template_path {
                Some(p) => fs::read_to_string(p)?,
                None => include_str!("../../skill/good-cat/assets/good-cat.js").to_owned(),
            };
            let rendered = render::render(&template, &report, &selection, args.threshold)?;
            platform::private_write(&output_path, rendered.javascript.as_bytes())?;
            Ok(
                json!({"status":"ok","operation":"render","workspace":work,"output":output_path,"selection":rendered.selection,"installed":false}),
            )
        }
        Commands::Speedtest(args) => {
            let (controller, secret) = match &args.controller {
                Some(endpoint) => (
                    endpoint.clone(),
                    std::env::var("MIHOMO_SECRET").unwrap_or_default(),
                ),
                None => {
                    let support = cli
                        .support_dir
                        .clone()
                        .map(Ok)
                        .unwrap_or_else(platform::default_support)?;
                    let pair = prefs(cli, &support).controller()?;
                    if pair.0.is_empty() {
                        bail!("FlClash API 未开启；先 controller enable，用户确认后 controller enable --apply");
                    }
                    pair
                }
            };
            let api = Api::new(&controller, &secret, args.allow_insecure_controller)?;
            api.version()
                .await
                .context("API 不可用或认证失败，请先检查 controller status")?;
            let work = cli
                .workspace
                .as_deref()
                .map(workspace::validate)
                .transpose()?;
            let work = if work.is_none() && args.output.is_none() {
                Some(workspace::create()?)
            } else {
                work
            };
            let output_path = workspace::explicit_or_default(
                args.output.as_deref(),
                work.as_deref(),
                workspace::REPORT,
            )?;
            let options = speedtest::SpeedOptions {
                urls: if args.urls.is_empty() {
                    speedtest::DEFAULT_URLS
                        .iter()
                        .map(|s| s.to_string())
                        .collect()
                } else {
                    args.urls.clone()
                },
                threshold_ms: args.threshold,
                timeout_ms: args.timeout,
                concurrency: args.concurrency as usize,
                rounds: args.rounds as usize,
            };
            let report = tokio::select! {result=speedtest::run(&api,&options)=>result?,_ = tokio::signal::ctrl_c()=>bail!("测速已取消")};
            let failed_all = report
                .reports
                .iter()
                .all(|r| r.results.iter().all(|n| n.delay_ms.is_none()));
            platform::private_write(&output_path, &serde_json::to_vec_pretty(&report)?)?;
            Ok(
                json!({"status":if failed_all{"all_failed"}else{"ok"},"operation":"speedtest","workspace":work,"output":output_path,"rounds":report.rounds,"aggregation":report.aggregation,"summary":report.reports.iter().map(|r|json!({"url":r.url,"tested":r.tested,"eligible":r.eligible.len(),"failed":r.failed})).collect::<Vec<_>>(),"warnings":report.warnings}),
            )
        }
        _ => {
            let support = cli
                .support_dir
                .clone()
                .map(Ok)
                .unwrap_or_else(platform::default_support)?;
            let preferences = prefs(cli, &support);
            match &cli.command {
                Commands::Inspect
                | Commands::Controller {
                    command: ControllerCommand::Status,
                } => {
                    let mut summary = platform::enabled_summary(&preferences)?;
                    if matches!(&cli.command, Commands::Inspect) {
                        let label = match summary.get("current_profile_id").and_then(Value::as_i64)
                        {
                            Some(id) => install::profile_label(&support, id)?,
                            None => None,
                        };
                        summary["current_profile_label"] = json!(label);
                    }
                    summary["status"] = json!("ok");
                    summary["support_dir"] = json!(support);
                    summary["pending_controller_recovery"] =
                        json!(controller_journal(&support).exists());
                    summary["pending_install_recovery"] =
                        json!(support.join(".good-cat-pending-install.json").exists());
                    Ok(summary)
                }
                Commands::Controller {
                    command: ControllerCommand::Enable { apply, port },
                } => enable_controller(cli, &support, *port, *apply).await,
                Commands::Controller {
                    command: ControllerCommand::Restore { apply, backup },
                } => {
                    let backup = match backup {
                        Some(path) => path.clone(),
                        None => pending_backup(&support)?,
                    };
                    let fields = read_json(&backup)?
                        .get("fields")
                        .cloned()
                        .context("controller 备份格式无效")?;
                    restore_fields(&mut json!({"patchClashConfig":{}}), &fields)?;
                    if !apply {
                        return Ok(
                            json!({"status":"confirmation_required","operation":"controller.restore","backup":backup,"may_disconnect":true}),
                        );
                    }
                    with_stopped(cli, &support, false, "controller", || {
                        let mut config = preferences.load()?;
                        if !controller_journal(&support).exists() {
                            let before = saved_fields(&config)?;
                            let recovery_backup =
                                platform::backup_dir(&support)?.join("controller.json");
                            platform::private_write(
                                &recovery_backup,
                                &serde_json::to_vec_pretty(&json!({"fields":before}))?,
                            )?;
                            platform::private_write(
                                &controller_journal(&support),
                                &serde_json::to_vec(&json!({"backup":recovery_backup}))?,
                            )?;
                        }
                        restore_fields(&mut config, &fields)?;
                        preferences
                            .save(&config)
                            .map_err(|error| install::RecoveryFailure {
                                operation: "controller restore failed".into(),
                                recovery: format!(
                                    "{error:#}; journal retained, retry controller restore --apply"
                                ),
                            })?;
                        platform::remove_durable(&controller_journal(&support))?;
                        Ok(json!({"status":"ok","operation":"controller.restore","restored":true}))
                    })
                }
                Commands::CurrentScript | Commands::Update(_) => {
                    let work =
                        workspace::validate(cli.workspace.as_deref().context("需要 --workspace")?)?;
                    if support.join(".good-cat-pending-install.json").exists() {
                        bail!("存在待恢复的安装，请先 recover --apply");
                    }
                    if controller_journal(&support).exists() {
                        bail!("存在待恢复的设置，请先 controller restore --apply");
                    }
                    let profile_id = current_profile(&preferences)?;
                    match &cli.command {
                        Commands::CurrentScript => {
                            update::export_current(&support, profile_id, &work)
                        }
                        Commands::Update(args) => {
                            let script = workspace::explicit_or_default(
                                args.script.as_deref(),
                                Some(&work),
                                workspace::SCRIPT,
                            )?;
                            if !args.apply {
                                return update::plan(&support, profile_id, &work, &script);
                            }
                            let mut verified =
                                update::check_plan(&support, profile_id, &work, &script)?;
                            let unchanged = verified
                                .get("source_sha256")
                                .and_then(Value::as_str)
                                .zip(verified.get("destination_sha256").and_then(Value::as_str))
                                .is_some_and(|(source, destination)| source == destination);
                            if unchanged {
                                verified["changed"] = json!(false);
                                verified["status"] = json!("ok");
                                verified["operation"] = json!("update");
                                verified["restarted"] = json!(false);
                                return Ok(verified);
                            }
                            with_stopped(cli, &support, false, "", || {
                                if current_profile(&preferences)? != profile_id {
                                    bail!("退出时当前订阅发生变化，请重新导出并预览后再更新");
                                }
                                update::apply(&support, profile_id, &work, &script)
                            })
                        }
                        _ => unreachable!(),
                    }
                }
                Commands::Install(args) => {
                    let name = args.name.clone().context(
                        "需要 Agent 根据当前订阅名称通过 --name 命名，无需向用户单独确认名称",
                    )?;
                    if !install::is_managed_name(&name) {
                        bail!("脚本名称必须为 good-cat-订阅简称，后缀以文字或数字开头，最多 80 字符，仅允许文字、数字、点、下划线和连字符；已有用户脚本受保护");
                    }
                    let script_path = workspace::explicit_or_default(
                        args.script.as_deref(),
                        cli.workspace.as_deref(),
                        workspace::SCRIPT,
                    )?;
                    if !script_path.is_file() || fs::read(&script_path)?.is_empty() {
                        bail!("安装脚本不存在或为空");
                    }
                    let plan = install::inspect(&support, &name)?;
                    if plan
                        .get("scripts")
                        .and_then(Value::as_array)
                        .is_none_or(|scripts| !scripts.is_empty())
                    {
                        bail!("安装仅允许新建脚本；已有脚本必须通过 current-script 和 update 按当前订阅更新");
                    }
                    let activate = if !args.register_only
                        && (args.activate_current || args.activate_profile.is_none())
                    {
                        Some(
                            preferences
                                .load()?
                                .get("currentProfileId")
                                .and_then(Value::as_i64)
                                .context("无法读取当前配置 ID")?,
                        )
                    } else {
                        args.activate_profile
                    };
                    if !args.apply {
                        return Ok(
                            json!({"status":"confirmation_required","operation":"install","script":script_path,"name":name,"may_disconnect":true,"activate_profile":activate,"plan":plan}),
                        );
                    }
                    if controller_journal(&support).exists() {
                        bail!("存在未完成的 controller 修改，请先恢复");
                    }
                    with_stopped(cli, &support, args.start, "", || {
                        if !args.register_only && args.activate_profile.is_none() {
                            let latest = preferences
                                .load()?
                                .get("currentProfileId")
                                .and_then(Value::as_i64);
                            if latest != activate {
                                bail!("退出时当前订阅发生变化，请重新预览后再安装");
                            }
                        }
                        let mut result =
                            install::register(&support, &script_path, &name, false, activate)?;
                        result["status"] = json!("ok");
                        result["operation"] = json!("install");
                        Ok(result)
                    })
                }
                Commands::Recover { apply } => {
                    if !apply {
                        return Ok(
                            json!({"status":"confirmation_required","operation":"recover","may_disconnect":true}),
                        );
                    }
                    with_stopped(cli, &support, false, "install", || {
                        let mut result = install::recover(&support)?;
                        result["status"] = json!("ok");
                        result["operation"] = json!("recover");
                        Ok(result)
                    })
                }
                _ => unreachable!(),
            }
        }
    }
}

#[tokio::main]
async fn main() {
    let cli = Cli::parse();
    match dispatch(&cli).await {
        Ok(value) => {
            output(&value);
            if value.get("status").and_then(Value::as_str) == Some("all_failed") {
                std::process::exit(2);
            }
        }
        Err(error) => {
            output(&json!({"status":"error","error":format!("{error:#}")}));
            std::process::exit(1);
        }
    }
}
