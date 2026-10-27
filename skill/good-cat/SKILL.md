---
name: good-cat
description: 调用 Rust CLI 管理 FlClash External Controller、测试当前订阅节点，由模型选择 ChatGPT/Claude 国家，再从模板生成并按授权安装覆写脚本。适用于节点测速、AI 分流优化和 FlClash 脚本管理。
compatibility: macOS arm64/amd64 或 Windows x64；Agent 从来源仓库 GitHub Release 安装 CLI，无需 Python、Rust 或独立 Mihomo 核心；npx 安装 Skill 为可选方式。
metadata:
  repository: https://github.com/0xlic/good-cat
---

# good-cat

## 入口和边界

- Skill 不包含平台二进制。按下文引导从用户指定的 GitHub 来源仓库安装 CLI，保存安装器返回的 `executable` 绝对路径；不向 Skill 目录写二进制，不自动安装编译环境，不回退到 Python。
- 模板是 `assets/good-cat.js`，生成时显式传 `--template` 指向当前 Skill 的资源，避免缓存 CLI 与 Skill 版本偏差。不要重写分流规则，只修改模板中的两项国家列表。
- 以下命令中的 `<CLI>` 表示该可执行文件的绝对路径。产物在 CLI 创建的系统临时工作区，**不写 Skill 目录或当前工作目录**；`<WORKSPACE>` 表示本次创建后返回的真实绝对路径，不可猜测固定的 `/tmp` 路径。
- 命令输出为 JSON，诊断/断网提示写 stderr；检查退出码和 `status`，不能把计划当作已执行。不要解析人类格式的终端表格。
- 不读取或输出 Secret 给模型。CLI 自行从 FlClash 设置读取认证；不修改 Shell 或系统环境变量，不在参数中传密钥。高级外部 API 模式才使用已有的 `MIHOMO_SECRET` 环境变量。

## 首次运行：识别平台并从 Release 安装 CLI

1. 正式来源是 `https://github.com/0xlic/good-cat`，默认使用 `0xlic/good-cat`。若用户明确指定 fork 或其他可信安装来源，则使用对应 `OWNER/REPO`，不要猜测同名第三方项目。Skill 通过 `npx skills add 0xlic/good-cat --skill good-cat` 安装后仍需下载对应平台 CLI。
2. 确认本机系统、原生架构。macOS 运行下方 Bash 安装器；Windows 运行 PowerShell 安装器。只支持 macOS arm64/amd64、Windows x64，不静默选择其他架构模拟运行。

macOS：

```sh
bash "<SKILL_DIR>/scripts/install.sh" --repo 0xlic/good-cat
```

Windows：

```powershell
powershell.exe -NoProfile -File "<SKILL_DIR>\scripts\install.ps1" -Repo 0xlic/good-cat
```

3. 安装器按平台下载指定仓库的最新稳定 Release 和 `SHA256SUMS`，固定同一 tag、校验后原子写入用户缓存。可用 `--version` / `-Version` 固定版本；Skill 若固定 tag，CLI 也应使用该 tag。版本或平台不可用时报告失败，不绕过校验。
4. 读取 JSON 中的 `executable`、`version`、`target`、`sha256`，把 `executable` 作为下文 `<CLI>`；调用 `<CLI> --version` 核对。路径有空格需正确引用，PowerShell 使用 `& "<CLI>" ...`。同一流程复用路径，不每个子命令都重装。
5. 不使用 `curl | sh`、不修改全局 PATH，不擅自关闭 Gatekeeper 或更改全局执行策略。校验和不是维护者身份签名，来源仓库必须可信。系统安全策略阻止执行时向用户说明。

Skill 文本可由 Agent 按用户给出的仓库链接直接安装，也可通过 npx skills 安装；npx 方式才需要 Node.js/npm，CLI 运行不需要。

## 0. 创建本次流程的临时工作区

```sh
<CLI> workspace create --json
```

保存输出中的 `workspace`、`report`、`selection`、`script` 绝对路径。每次完整流程创建一个独立工作区，之后跨命令、跨用户确认都使用同一个 `--workspace "<WORKSPACE>"`，不要把上次流程的文件混进来。

macOS 跟随 `$TMPDIR`（不一定是 `/tmp`），Windows 跟随系统 TEMP/TMP。目录独立且仅供当前用户访问，命令退出后保留以串联步骤；系统可能清理它。若已被清理，停止并新建工作区重测，不回退到当前目录或旧文件。完成后可按用户意愿清理本次工作区。FlClash 的恢复备份仍保存在应用数据目录，不移到临时目录。

## 1. 检查并准备 API

```sh
<CLI> inspect --json
<CLI> controller enable --json
```

`controller enable` 默认只返回 `confirmation_required` 计划，不操作进程。向用户说明启用 API 会短暂停止/启动 FlClash、可能中断当前模型连接，请用户回复“继续”。收到授权后：

```sh
<CLI> controller enable --apply --json
```

停止、备份、写入设置、恢复启动和 API 就绪检查在一次调用内完成，不能拆成依赖模型断网后继续调用的步骤。只监听当前 FlClash 支持的 `127.0.0.1:9090`，不开放公网，不修改 `general-override-keys`。已有同地址、带密钥且可用的 API 会复用，不重启。端口冲突在断网前失败。

输出保留 `backup` 路径以备恢复，不读取其内容给模型；备份可能含密钥。CLI 不自动恢复关闭 API，用户完成后可按需执行恢复。

## 2. 测速

```sh
<CLI> --workspace "<WORKSPACE>" speedtest --json
```

- 使用正在运行的 FlClash 核心 API，而不是独立核心，不切换策略组。
- 默认测试 ChatGPT、Claude、Gstatic 三个 URL；当前已加载的节点及 providers 都纳入测试，排除策略组与 DIRECT/REJECT 等内置出口。
- 默认每个 URL 对全部节点完整测试三轮；三轮均成功才计算中位延迟，任一轮失败/超时/0 ms 就排除该节点。报告保留每轮 samples。按聚合后的中位延迟严格筛选 `0 < delay < 600 ms`，每个 URL 独立降序排列。不要擅自减少轮数、改变阈值或 URL。
- `--concurrency` 默认 8、最大 64，`--timeout` 单位 ms，默认 5000；并发会影响当前流量和测量值，必要时经用户同意降低并发重测。
- 测速退出码 2 表示全部失败，不能据此生成配置。退出码 0 也不保证每个服务有达标节点，必须检查报告。
- 测速是当前 FlClash 环境下的 HTTP 延迟，不是独立物理线路基准，不代表带宽或服务解锁。

### 测速结果展示时的 Claude 风险提示（必需）

每次展示测速结果或国家建议时，在统计表附近明确告知用户：

> Claude 频繁切换出口 IP，尤其跨国家/地区切换，可能触发账号风控，存在访问受限或封号风险。建议先检查出口 IP 的信誉/纯净度、共享和滥用情况，再为 Claude 仅保留一个国家，尽量固定可信节点。低延迟、单一国家或第三方“纯净度”评分都不能保证账号安全。

程序当前不提供 IP 信誉检测，不得声称已完成检测，也不得将 HTTP 测速成功当作 IP 安全证明。调用第三方检测服务前先取得用户授权，不发送账号、密钥等凭据。

Claude 默认提出单一国家方案；尚未检测 IP 信誉时明确标注“未检测，建议先检查”。若用户明确要求多国，重申风险并取得确认后再生成，不擅自扩大国家范围。不得因这条建议直接改动已启用的配置。

## 3. 模型选择国家，程序校验并生成

读取工作区的 `report` 路径，分别判断 ChatGPT、Claude 的节点国家和延迟，将结构化选择写到工作区返回的 `selection` 绝对路径（`selection.json`），例如：

```json
{"chatgpt":["US","JP"],"claude":["JP"]}
```

国家应在模板 `COUNTRY_PATTERNS` 中定义，每个所选国家在对应测速 URL 下必须有严格低于阈值的有效节点。未知名称不猜测；若节点名匹配多个国家，可在选择文件中加 `node_country_overrides`，按精确 `(name, provider)` 指定匹配到的一个国家。示例：

```json
{"chatgpt":["US"],"claude":["JP"],"node_country_overrides":[{"name":"US JP 01","provider":null,"country":"US"}]}
```

覆盖只用于消解名称中已有的国家标记，不能凭推测把无法被模板正则匹配的节点归国，否则生成器会拒绝。完全未知名称需要用户确认命名或调整模板后重新验证。

```sh
<CLI> --workspace "<WORKSPACE>" render --template "<SKILL_DIR>/assets/good-cat.js" --json
```

程序只替换 `CHATGPT_COUNTRIES`、`CLAUDE_COUNTRIES`，其他字节保持不变。`Porxy` 始终是全部节点。任一国家没有达标证据、任一服务选择为空或输入不合法，停止，不注册旧产物、不填默认国家、不自动放宽条件。

国家是范围而非节点白名单：同国其他慢节点仍可能进入 url-test 组。模型应说明这种限制以及选择理由。

## 4. 安装前展示变更并再次确认

```sh
<CLI> --workspace "<WORKSPACE>" install --json
```

向用户说明所选国家、将新增还是覆盖脚本、是否已被启用、是否要切换当前配置及短暂断网影响。用户确认后：

```sh
<CLI> --workspace "<WORKSPACE>" install --apply --json
```

默认注册后在同一事务中把当前订阅关联到该脚本并启用 script 模式，预览中必须向用户说明目标订阅和启用影响。`--activate-current` 是显式同义写法；用户只要求注册不启用时使用 `--register-only`，指定其他订阅时使用 `--activate-profile <ID>`。已启用同名脚本更新需要额外的 `--allow-active-update` 授权；也可用 `--name good-cat-new` 新建。

不得自己拼 SQL、修改 plist、强杀进程或单独发出停止命令。程序负责事务、备份、日志和恢复启动，不能在断网后依赖模型来重启。

## 恢复与异常

- `controller restore --backup <开启时返回的备份路径>` 默认仅预览，确认后加 `--apply` 恢复 API 的原设置。
- 有未完成的 controller 操作：`controller restore --apply` 使用本地恢复日志。
- 有未完成的安装：`recover --apply` 恢复数据库和脚本。先向用户说明可能断网并获得确认。
- 发现未完成事务时，不自动绕过、删除日志或重启不一致配置。断电/强杀不能保证自动恢复；Windows 的目录落盘保证与 Unix 不同。
- 自动发现失败可使用 `--support-dir`、`--app`；`--preferences` 仅用于明确文件格式的高级/测试场景，macOS 正常使用原生 CFPreferences，不直接改 live plist。
- 不展示订阅链接、节点凭据、数据库内容或 Secret；报告和备份不上传、不提交版本库。
- 所有参数通过 `<CLI> --help` 和相应子命令的 `--help` 查询。
