# good-cat

把本仓库链接交给 Agent，即可安装 Skill 并运行 FlClash 配置流程：

**开启当前 FlClash API → 三轮测速 → 大模型选择国家 → 按模板生成 JS → 确认后安装并关联当前订阅。**

Skill 只包含文本说明、模板和安装脚本，不携带平台二进制。Agent 根据操作系统从本仓库 GitHub Release 下载 CLI、校验 SHA256 并缓存到用户目录。运行 CLI 不需要 Python、Node.js、Rust 或另一个 Mihomo 核心。

## 方式一：直接把仓库链接交给 Agent

可以直接这样说（把链接替换成当前仓库）：

```text
请安装并加载这个仓库的 good-cat Skill：https://github.com/0xlic/good-cat。
阅读 README 和 skill/good-cat/SKILL.md，按当前系统从该仓库 Release
下载并校验 CLI，然后执行完整流程。重启 FlClash 或安装配置前先让我确认。
测速结果出来时提醒我 Claude 切换 IP 的风控风险，并建议检测 IP 信誉后只保留一个国家。
```

Agent 应从用户给出的链接/可信安装来源确定 `OWNER/REPO`，不能猜测同名仓库。读取 `skill/good-cat/SKILL.md`，安装其文本资源及 `scripts/`、`assets/` 后调用下方安装器。没有可用 Release 时应说明原因，不回退执行来历不明的二进制，也不自动安装编译环境。

本仓库地址为 **https://github.com/0xlic/good-cat**，下文命令已使用真实地址。若使用 fork，请改为对应来源，工作流内部自动使用 `github.repository`。

## 方式二：用 npx 安装 Skill

需要 Node.js/npm 的**仅是这条可选安装途径**，CLI 本身不需要 Node.js：

```sh
npx skills add 0xlic/good-cat --skill good-cat
```

也可以直接指定 Skill 目录，避免不同 Agent 的目录扫描差异：

```sh
npx skills add https://github.com/0xlic/good-cat/tree/main/skill/good-cat
```

若默认分支不是 `main`，换成实际分支。按安装器提示选择使用的 Agent，之后让 Agent 加载 `good-cat`。Skill 首次运行时再按系统下载 CLI，不会把 macOS 二进制安装到 Windows。参考 [Skills CLI](https://github.com/vercel-labs/skills)。

## Agent 如何获取 CLI

以下 `<SKILL_DIR>` 是已安装 Skill 的真实路径，不要求克隆完整仓库。命令使用 `0xlic/good-cat`；用户明确使用 fork 时应跟随其仓库来源。

macOS（原生 Bash、curl、plutil、shasum，无需 jq/Python）：

```sh
bash "<SKILL_DIR>/scripts/install.sh" --repo 0xlic/good-cat
```

Windows（原生 PowerShell）：

```powershell
powershell.exe -NoProfile -File "<SKILL_DIR>\scripts\install.ps1" -Repo 0xlic/good-cat
```

安装器返回 JSON，其中 `executable` 是后续调用的 CLI 绝对路径，另包含 `version`、`target`、`sha256`。Agent 保存路径并调用 `--version` 检查；同一流程不必反复运行安装器。不修改全局 PATH，不写 Skill 目录，不需要管理员权限。

- 默认解析一次最新稳定版并固定 tag，再下载该版本的二进制和 `SHA256SUMS`，避免更新过程中混用文件。
- 支持固定版本：macOS `--version v0.1.0`，Windows `-Version v0.1.0`；需要一致性时，Skill 来源也固定到同一个 tag。
- 二进制写入版本/架构隔离的缓存；已有文件也会按该 Release 的校验清单验证。缺文件、无校验值或哈希不匹配就失败，不执行下载内容。
- macOS 默认缓存 `~/Library/Caches/good-cat/`；Windows 默认 `%LOCALAPPDATA%\good-cat\`。`--cache-dir` / `-CacheDir` 指定的是父目录，安装器在其中创建专属 `good-cat` 子目录，不修改父目录权限。已有目录必须带有效缓存标记且权限安全，否则拒绝接管。
- 只从用户指定仓库的 HTTPS Release 下载，不使用 `curl | sh`。SHA256 能校验文件完整性，但不是维护者身份签名；仍需信任该仓库及发布者。
- 遇到系统安全策略或 macOS 下载隔离提示，应向用户说明，不擅自关闭 Gatekeeper、修改全局执行策略或请求管理员权限。

### Release 文件约定

| 平台 | Release asset |
|---|---|
| macOS Apple Silicon | `good-cat-aarch64-apple-darwin` |
| macOS Intel | `good-cat-x86_64-apple-darwin` |
| Windows x64 | `good-cat-x86_64-pc-windows-msvc.exe` |
| 校验清单 | `SHA256SUMS` |

暂不发布 Linux 或 Windows ARM64 版本；遇到不支持的系统/架构要明确报告，不猜测或静默使用模拟运行。

## 使用流程

以下 `<CLI>` 替换为安装器返回的完整 `executable` 路径；`<WORKSPACE>` 替换为工作区创建结果。带空格的路径按所在 Shell 正确引用，PowerShell 可用 `& "<CLI>" ...`。

### 0. 创建系统临时工作区

```sh
<CLI> workspace create --json
```

返回 `workspace`、`report`、`selection`、`script` 绝对路径。每次流程创建独立目录，跨命令及用户确认都复用同一个 `--workspace`。macOS 跟随系统 `$TMPDIR`，Windows 使用系统 TEMP/TMP；不向 Skill 或当前目录写产物。

目录在命令退出后保留；系统可能清理它，丢失时需新建并重测，不能猜测上次路径或用旧文件。Unix 工作区权限 0700，CLI 输出文件默认 0600；Windows 继承当前用户临时目录权限。无 `--output`/`--workspace` 的 `speedtest` 也会自动创建工作区并返回路径。

### 1. 开启 API：第一次确认

```sh
<CLI> inspect --json
<CLI> controller enable --json
```

默认仅返回 `confirmation_required` 计划。说明会短暂断网，用户确认后：

```sh
<CLI> controller enable --apply --json
```

程序在一次调用内完成正常退出、备份、写设置、恢复启动及 API 就绪检查，不依赖断网后模型继续发命令；已有同地址、带密钥且可用的 API 会复用，不重启。

只开启 `127.0.0.1:9090`，不开放公网；当前核实的 FlClash v0.9.2 控制地址是枚举，不支持任意端口。端口冲突在断网前失败。只改 `patchClashConfig` 的 `external-controller`、`secret`，保留其他字段及 `general-override-keys`；安全模式可能禁止 API，失败时尝试恢复。

macOS 使用原生 CFPreferences，不直接改 live plist；Windows 保留 `shared_preferences.json` 的外层与 JSON 字符串结构。CLI 内部读取/保存 Secret，不输出给模型、不放进参数或全局环境变量。

### 2. 三轮测速，展示结果并提示风险

```sh
<CLI> --workspace "<WORKSPACE>" speedtest --json
```

测试当前 FlClash 已加载节点及 providers，排除策略组和内置出口；不启动第二个 Mihomo，不切换代理组。默认 URL 为 ChatGPT、Claude 的 `/cdn-cgi/trace` 与 Gstatic 的 `/generate_204`。

**默认每个 URL 完整测试三轮，三轮均成功才计算中位延迟；任一轮失败、超时或 0 ms 就排除该节点。** 每个 URL 独立按中位延迟严格筛选 `0 < delay < 600 ms` 并降序排列。报告保存全部 `samples`、聚合结果和入选列表，stdout 给摘要。`tested`/`failed` 按节点而非请求次数统计。

展示统计表时必须同时提醒：

> **Claude 切换出口 IP，尤其频繁跨国家/地区切换，可能触发账号风控，存在访问受限或封号风险。建议先检查出口 IP 信誉/纯净度、共享及滥用情况，再仅保留一个国家，尽量固定可信节点。低延迟、单一国家或第三方评分都不是免封号保证。**

当前工具不提供 IP 信誉检测，不得声称已经检测，也不把测速成功当作 IP 安全证明。调用第三方检测前需经用户授权，不发送账号或密钥。未检测时明确标注，Claude 默认提出单一国家方案；用户明确要多国时再次说明风险、取得确认。现有配置不因此自动改动。

参数：`--rounds` 默认 3（1–20），`--threshold` 默认 600 ms，`--timeout` 默认 5000 ms，`--concurrency` 默认 8（最大 64），重复 `--url` 可替代默认 URL。不得擅自减少轮数或放宽条件。并发和网络状态仍影响结果，HTTP 延迟不代表带宽或服务解锁。

高级 `--controller` 模式认证来自 `MIHOMO_SECRET`；不支持命令行传 Secret。API 请求绕过环境代理、不跟随重定向，远程 API 默认要求 HTTPS。结果反映当前 FlClash 环境，不是物理线路的隔离基准。

### 3. 模型选国家，程序生成 JS

模型读取 `report`，将选择写到工作区返回的 `selection` 路径，例如（仅格式示例）：

```json
{"chatgpt":["TW","JP"],"claude":["JP"]}
```

```sh
<CLI> --workspace "<WORKSPACE>" render --template "<SKILL_DIR>/assets/good-cat.js" --json
```

显式传 Skill 模板，避免缓存 CLI 与 Skill 版本不同造成模板偏差；CLI 本身也有内嵌副本。程序只替换 `CHATGPT_COUNTRIES`、`CLAUDE_COUNTRIES`，其他字节保持不变。

所选国家必须属于模板，非空、不重复，且在对应服务的完整报告中有达标证据；未知或歧义未解决的名称不猜测。多国名称可用精确 `(name, provider)` 的 `node_country_overrides` 消解到已有标记，不能把模板过滤不到的名称硬映射。失败不覆盖已有产物、不拿旧脚本安装。

国家过滤不是节点白名单，同国慢节点仍可能进入 url-test；节点名不保证真实出口。模板包含 ChatGPT、Claude、Porxy 三组，本地直连用 `GEOSITE,private` 和 `GEOIP,private`，覆盖范围取决于数据源；微软、苹果、中国大陆直连，其他流量走 Porxy，沿用 FlClash 默认 GeoSite 源。

### 4. 安装并关联当前订阅：第二次确认

```sh
<CLI> --workspace "<WORKSPACE>" install --json
```

展示国家选择、目标订阅、新增/覆盖状态和断网影响，用户确认后：

```sh
<CLI> --workspace "<WORKSPACE>" install --apply --json
```

默认在同一事务中注册脚本并将当前订阅设为该脚本的 script 模式。只注册用 `--register-only`，其他订阅用 `--activate-profile <ID>`。更新已启用同名脚本另需 `--allow-active-update` 授权，也可用 `--name good-cat-new` 新建。

程序正常退出、备份、事务写入、恢复启动一次完成，不强杀；不要在中途手动启动。原本关闭则保持关闭，`--start` 可要求启动。仍驻留托盘时需真正退出后重试。

## 恢复与安全

恢复备份和日志保存在 **FlClash 数据目录**，不是临时工作区；含凭据，不上传、不分享。

```sh
<CLI> controller restore --backup /path/from/enable/controller.json --apply
<CLI> controller restore --apply  # 使用未完成的 controller 日志
<CLI> recover --apply             # 恢复未完成安装的 SQLite 和 JS
```

恢复操作也应先说明断网影响并确认。不自动删除待恢复日志或重启不一致配置。通常失败会恢复原本运行的应用；无法保证一致性则保持停止并提示恢复。断电/强杀不能保证自动恢复；Windows 目录落盘保证不同于 Unix，不能宣称跨文件崩溃原子性。

API 默认保持开启，需关闭时使用开启操作返回的备份，只恢复 controller 字段，不覆盖期间其他设置。数据目录/应用自动发现失败用 `--support-dir`、`--app`；`--preferences` 是高级文件式设置/测试选项，macOS 正常使用 CFPreferences。

业务结果/错误为 JSON，帮助和参数错误为 CLI 文本。退出码 0 表示成功或等待确认（检查 `status`），1 为失败，2 为参数错误或测速全部失败。所有显式相对路径基于当前工作目录，Skill 和默认产物不依赖源码目录。

## 维护者：通过 GitHub CI 发布

```text
cli/                         Rust 源码、Cargo.toml / Cargo.lock
skill/good-cat/SKILL.md       跨平台 Skill
skill/good-cat/assets/       模板
skill/good-cat/scripts/      macOS / Windows Release 安装器（文本）
tests/                       集成测试，不打包到 Skill
.github/workflows/ci.yml      普通构建与测试
.github/workflows/release.yml tag 触发、多平台构建、SHA256SUMS、GitHub Release
```

CI 原生 runner 测试代码，再构建 macOS arm64/amd64、Windows amd64；所有平台成功后才发布三份原始二进制和 `SHA256SUMS`。Release 不包含 Skill 内的二进制目录，不覆盖既有 Release asset。

1. 将项目推送到 GitHub，启用 Actions。
2. 更新 `cli/Cargo.toml` 的版本，提交并推送代码。
3. 创建与 Cargo 版本一致的 tag：

```sh
git tag v0.1.0
git push origin v0.1.0
```

`release.yml` 自动发布；预发布 tag 会标为 prerelease，不进入默认 latest。分发说明和仓库链接只有在第一次 Release 成功后才可完成二进制安装；不存在 Release 时不得宣称已发布。

开发构建需要 Rust 1.95+ 和 C 工具链（bundled SQLite），仅开发者需要：

```sh
cargo fmt --manifest-path cli/Cargo.toml --check
cargo clippy --manifest-path cli/Cargo.toml --locked --all-targets -- -D warnings
cargo test --manifest-path cli/Cargo.toml --locked
cargo build --manifest-path cli/Cargo.toml --locked --release
```

本地开发直接调用 `cli/target/release/good-cat`，不要复制到 Skill。测试使用临时 SQLite、设置和假 API，不修改真实 FlClash。macOS 已验证流程，Windows 需以原生 CI/真机验证为准。FlClash 内部设置和数据库并非稳定接口，结构不符合预期时拒绝操作。
