---
name: good-cat
description: 调用 Rust CLI 管理 FlClash External Controller、测试当前订阅节点，按固定默认方案生成 ChatGPT/Claude 覆写脚本，并对已有受管脚本做针对性调整。适用于节点测速、AI 分流优化和 FlClash 脚本管理。
compatibility: macOS arm64/amd64 或 Windows x64；用户只需提供仓库链接，由 Agent 安装并运行，无需 Python、Node.js、Rust 或独立 Mihomo 核心。
metadata:
  repository: https://github.com/0xlic/good-cat
---

# good-cat

## 入口和边界

- **禁止全局安装。** Skill 必须放在当前 Agent 目录内的项目本地技能目录，例如 `.agents/skills/good-cat`；不得写入 `~/.agents/skills`、全局 npm 目录或其他用户级位置。CLI、缓存、工作区、临时文件、备份和状态全部放在当前 Agent 目录的 `.good-cat/` 下。
- Skill 不包含平台二进制。按下文从用户指定的 GitHub 来源仓库安装 CLI；不向 Skill 目录写二进制，不自动安装编译环境，不回退到 Python。
- 新安装使用模板 `assets/good-cat.js`，生成时显式传 `--template` 指向当前 Skill 的资源，避免缓存 CLI 与 Skill 版本偏差。已有脚本更新必须先导出当前受管脚本，由 Agent 按用户请求确定最小修改范围；仅调整国家时只改对应数组，不用完整模板覆盖现有脚本。规则或分组结构调整也必须在预览中明确说明并获得确认，不碰无关内容。
- 所有命令都在 `<AGENT_DIR>` 执行。`<CLI>` 使用安装器返回的 `executable_relative`（或 `executable`），位于 `./.good-cat/bin/`；不要加入全局 PATH。`<WORKSPACE>` 使用 `workspace create` 返回的相对 `workspace` 路径。
- 命令输出为 JSON，诊断/断网提示写 stderr；检查退出码和 `status`，不能把计划当作已执行。不要解析人类格式的终端表格。
- 不读取或输出 Secret 给模型。CLI 自行从 FlClash 设置读取认证；不修改 Shell 或系统环境变量，不在参数中传密钥。高级外部 API 模式才使用已有的 `MIHOMO_SECRET` 环境变量。

## 首次运行：识别平台并从 Release 安装 CLI

1. 用户只需提供仓库链接，安装由 Agent 完成，不要求用户执行安装命令。正式来源是 `https://github.com/0xlic/good-cat`，默认使用 `0xlic/good-cat`。读取该来源的 `skill/good-cat/SKILL.md`，把同一版本的 `assets/`、`scripts/` 和说明安装到当前项目本地的 `.agents/skills/good-cat`；若 Agent 直接从仓库工作，也可使用仓库内的 `skill/good-cat`。不得全局安装。再运行项目本地安装脚本下载 CLI。可使用 GitHub 内容接口或可信来源的原始文件，不依赖包管理器。若用户明确指定 fork 或其他可信安装来源，则使用对应 `OWNER/REPO`，不要猜测同名第三方项目。
2. 确认本机系统、原生架构。macOS 运行下方 Bash 安装器；Windows 运行 PowerShell 安装器。只支持 macOS arm64/amd64、Windows x64，不静默选择其他架构模拟运行。

macOS（从 `<AGENT_DIR>` 运行）：

```sh
bash ".agents/skills/good-cat/scripts/install.sh" --repo 0xlic/good-cat
```

Windows（从 `<AGENT_DIR>` 运行）：

```powershell
powershell.exe -NoProfile -File ".agents\skills\good-cat\scripts\install.ps1" -Repo 0xlic/good-cat
```

3. 安装器把指定 Release 和 `SHA256SUMS` 下载到 `./.good-cat/bin/`，固定同一 tag、校验后原子写入。自定义缓存也必须在当前 Agent 目录内。可用 `--version` / `-Version` 固定版本；Skill 若固定 tag，CLI 也应使用该 tag。版本或平台不可用时报告失败，不绕过校验。
4. 读取 JSON 中的 `executable_relative`、`executable`、`version`、`target`、`sha256`，优先把 `executable_relative` 作为下文 `<CLI>`；调用 `<CLI> --version` 核对。路径有空格需正确引用，PowerShell 使用 `& "<CLI>" ...`。同一流程复用路径，不每个子命令都重装。
5. 不使用 `curl | sh`、不修改全局 PATH，不擅自关闭 Gatekeeper 或更改全局执行策略。校验和不是维护者身份签名，来源仓库必须可信。系统安全策略阻止执行时向用户说明。

整个安装和运行流程由 Agent 串联，用户只需查看结果并在涉及断网或配置变更时确认；不向用户暴露平台下载、缓存路径或内部命令细节。

## 确认 FlClash 安装位置

不要假设 FlClash 安装在 `/Applications`、`Program Files` 等固定目录，也不要自行全盘搜索或执行来源不明的程序。

1. 执行 `inspect --json`，读取 `app`：`path` 是唯一确认的安装路径，`candidates` 是系统发现的候选，`ambiguous` 表示发现多个安装。
2. `path` 唯一时，在方案中明确展示该路径，后续所有可能停止/启动应用的命令都显式传全局 `--app "<path>"`。这既是模型确认，也避免运行时自动选错副本。
3. `path` 为 `null`、`not_found` 或 `ambiguous` 时，先让用户确认实际使用的 FlClash 位置：macOS 提供 `.app` 路径，Windows 提供 `FlClash.exe` 路径。可以用 `candidates` 让用户选择，但不能替用户猜测；确认后再传 `--app`。
4. 用户使用便携版、自定义目录或自定义安装器时，以用户确认和系统进程/LaunchServices/注册表/开始菜单发现结果为准。macOS `--app` 必须是 `.app` 目录，Windows 必须是 `.exe` 文件。
5. `controller enable/restore`、`install`、`recover` 的预览显示 `app.path`；`inspect.layout` 显示所有本地目录。若预览没有可确认路径，停止并先解决路径问题。不要在断网操作后才发现路径错误。

CLI 会优先检查运行中的 FlClash，再使用平台原生索引；发现多个安装不会自动选择，必须由 Agent/用户确认 `--app`。

## 0. 创建本次流程的本地工作区

```sh
<CLI> workspace create --json
```

保存输出中的相对 `workspace`、`report`、`selection`、`script`（`*_absolute` 仅用于工具调试）。每次完整流程创建一个独立工作区，之后跨命令、跨用户确认都使用同一个 `--workspace "<WORKSPACE>"`。

工具自有文件全部在 `<AGENT_DIR>/.good-cat/`：

```text
.good-cat/bin/           CLI 可执行文件
.good-cat/workspaces/    测速、选择、生成的脚本
.good-cat/tmp/           进程临时文件
.good-cat/backups/       安装/API 恢复备份
.good-cat/state/         锁和未完成事务日志
```

不写系统临时目录、用户缓存、全局路径或 Skill 目录。FlClash 自身的数据库、配置和偏好仍由 FlClash 保存在其应用数据目录；`good-cat` 自己生成的备份和日志均在 `.good-cat/state`、`.good-cat/backups`。完成后由 Agent 根据用户要求删除 `.good-cat` 和项目本地 Skill；不要删除 FlClash 自身数据。

## 分支：首次方案还是调整已有方案

先执行 `inspect --json`，根据 `current_script.managed` 判断：

- `false` 或没有受管脚本：按首次使用处理。固定走下方 1–4，使用默认三国家方案。**只展示一个方案，然后请用户回复“继续”**，不要提供 1/2/3 选项菜单，也不要让用户先做国家选择。
- `true`：这是已有 good-cat-<订阅简称> 方案。不要重装或重建工作区之外的配置；先走“更新当前受管脚本”，导出并读取当前脚本，再只调整用户明确要求的范围。

仅名称为 `good-cat-<订阅简称>` 的当前绑定脚本可更新：后缀 1–80 个字符，以文字或数字开头，只允许文字、数字及 `._-`，支持中文。旧的裸名 `good-cat` 和其他用户脚本受保护，不读取或导出其源码，不自动改名接管。首次使用遇到旧的裸名 `good-cat` 时，新脚本会保留旧脚本，并在用户确认后绑定新脚本；不要试图修改旧脚本。

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

> Claude 频繁切换出口 IP，尤其跨国家/地区切换，可能触发账号风控，存在访问受限或封号风险。请先自行检查出口 IP 的信誉/纯净度、共享和滥用情况，再按需要求为 Claude 缩减到一个国家，尽量固定可信节点。低延迟、单一国家或第三方“纯净度”评分都不能保证账号安全。

程序当前不提供 IP 信誉检测，不得声称已完成检测，也不得将 HTTP 测速成功当作 IP 安全证明。调用第三方检测服务前先取得用户授权，不发送账号、密钥等凭据。

首次方案的 ChatGPT 和 Claude 都固定为 `url-test`，分别根据各自测速结果优选排名前三的国家，不预设任何国家，不生成 select 父组或国家子组。用户可以在 FlClash 的 Claude 分组中对具体节点使用原生“首选/pin”固定节点，再用测试网站检查出口 IP；普通点击、再次点击同一节点可取消固定。不要用 select 组模拟 pin。

测速必须用于国家优选，不得仅用来验证模板中的 TW/SG/JP。全量测试完成前不限定国家；两个服务独立排名，结果可以不同。只给出一份基于实测结果的方案，不提供多个候选菜单，不虚构节点。尚未检测 IP 信誉时明确标注“未检测，请用户自行检查”。用户之后可以要求把 Claude 缩减到某个国家；这是对已有方案的有针对性调整，不假造测速结果，也不因风险提示自动改动配置。

## 3. 模型选择国家，程序校验并生成

读取工作区的 `report` 路径，ChatGPT、Claude 各自按对应 URL 的全量实测结果执行以下流程：

1. 仅纳入三轮全部成功、聚合中位延迟满足 `0 < delay < 600 ms` 的节点。按精确 `(name, provider)` 去重；不得把同名不同 provider 节点合并，也不得重复计算样本。
2. 按模板 `COUNTRY_PATTERNS` 识别国家，未知或歧义节点不猜测、不用于国家排名；展示未能归国的节点数。歧义可按下文规则确认并覆盖后重新排名。
3. **国家得分 = 该国全部合格节点的聚合中位延迟的中位数**，偶数个取中间两项平均。得分越低越优；得分相同依次按合格节点数降序、国家代码字典序升序排序。
4. 取国家得分升序的前三名，分别写入 `selection.json` 的 `chatgpt`、`claude` 数组，顺序与国家排名一致。原始节点报告是降序排列，不能直接取报告前三个节点当作三个国家。
5. 展示各服务国家排名、国家得分（ms）、合格节点数和最终选择，让用户能核对依据。若只有一或两个合格国家，只选实际数量并说明不足；某服务没有合格国家则停止生成，不补默认国家、不放宽阈值。

模板中的 `CHATGPT_COUNTRIES` / `CLAUDE_COUNTRIES` 初始数组只是待替换占位，绝不是默认推荐。必须将实测排名写入工作区返回的相对 `selection` 路径，调用 render 生成后才允许安装；不得直接安装模板。

已有方案仅在用户明确要求调整某服务时才进入相应分支。

国家应在模板 `COUNTRY_PATTERNS` 中定义，每个所选国家在对应测速 URL 下必须有严格低于阈值的有效节点。未知名称不猜测；若节点名匹配多个国家，可在选择文件中加 `node_country_overrides`，按精确 `(name, provider)` 指定匹配到的一个国家。示例：

```json
{"chatgpt":["US"],"claude":["JP"],"node_country_overrides":[{"name":"US JP 01","provider":null,"country":"US"}]}
```

覆盖只用于消解名称中已有的国家标记，不能凭推测把无法被模板正则匹配的节点归国，否则生成器会拒绝。完全未知名称需要用户确认命名或调整模板后重新验证。

```sh
<CLI> --workspace "<WORKSPACE>" render --template "<SKILL_DIR>/assets/good-cat.js" --json
```

程序只替换 `CHATGPT_COUNTRIES`、`CLAUDE_COUNTRIES`，其他字节保持不变。`Porxy` 始终是全部节点。任一国家没有达标证据、任一服务选择为空或输入不合法，停止，不注册旧产物、不填默认国家、不自动放宽条件。

国家是范围而非节点白名单：同国其他慢节点也可能进入对应分组。ChatGPT、Claude、Porxy 都是 url-test；Claude 没有子组。要固定某个具体节点时使用 FlClash 原生“首选/pin”，不要去生成 selector。模型应说明这一限制：默认情况下 Claude 仍可能在筛选范围内自动换节点，用户应先 pin 再检查 IP 信誉和稳定性。

## 4. 安装前展示变更并再次确认

```sh
<CLI> --workspace "<WORKSPACE>" install --name "<Agent选定的good-cat-订阅简称>" --json
```

命名由 Agent 自行决定，不单独询问用户。读取 `inspect` 返回的 `current_profile_label`，用易识别的订阅简称命名为 `good-cat-<订阅简称>`，通过 `--name` 显式传入。长名称保留品牌或关键词，去掉套餐、节点数量、日期、网址或凭据等冗余/敏感信息，简称建议不超过 20 个字符；支持中文，例如 `good-cat-良心云`、`good-cat-guga`。名称为空时可参考当前订阅 ID 取简短名称，不使用时间戳或随机串。若新建时重名，Agent 自行添加简短序号（如 `-2`）；已有脚本修改则走更新流程，不覆盖或重命名接管。

预览和应用必须传入同一个名称。命名不需要用户确认；安装、关联订阅和短暂断网仍须向用户说明。首次使用展示固定方案后，**只请用户回复“继续”**；用户如果要调整，先调整候选并重新展示方案，再等待“继续”：

```sh
<CLI> --workspace "<WORKSPACE>" install --name "<预览返回的name>" --apply --json
```

默认注册后在同一事务中把当前订阅关联到该脚本并启用 script 模式，预览中必须向用户说明目标订阅和启用影响。`--activate-current` 是显式同义写法；用户只要求注册不启用时使用 `--register-only`，指定其他订阅时使用 `--activate-profile <ID>`。安装仅允许新建，不接受重名覆盖；已有受管脚本的调整只能走下方更新流程，没有绕过保护的强制覆盖选项。

首次方案展示一份实测优选方案：三个 `url-test` 分组、ChatGPT/Claude 各自国家排名与前三名（不足三个如实说明）、Claude 无子组、原生 pin 可在应用后手动使用、备份并关联当前订阅。不要把它包装成多个选择让用户挑选。

不得自己拼 SQL、修改 plist、强杀进程或单独发出停止命令。程序负责事务、备份、日志和恢复启动，不能在断网后依赖模型来重启。

## 更新当前受管脚本（局部修改）

1. 创建独立工作区后，必须先导出当前绑定脚本：

```sh
<CLI> --workspace "<WORKSPACE>" current-script --json
```

只读取返回的 `source_path`（`current-good-cat.js`），保留 `.good-cat-current-script.json` 基线。导出拒绝非受管名称、裸名 `good-cat`、共享引用或无有效绑定时，停止，不读取应用脚本文件、不改名、不改绑来绕过。不得自动转为新安装。

2. 只有在用户明确提出调整时才修改候选。将导出的源码复制到独立候选 `good-cat.generated.js`，不要编辑导出原件或应用内文件。模型按请求决定最小修改范围，其他字节保持原样；如果用户只是查询当前方案，不生成变更、不重启。若用户要求规则或分组结构调整，在差异中明确展示；常见国家调整可走以下分支：
   - 锁定/缩减 Claude 国家：只修改 `CLAUDE_COUNTRIES`，例如减少为用户已检查 IP 的一个原有国家；不生成子组，不要求完整重测，也不改 ChatGPT。若原有 pinned 节点不属于新国家，告知用户需要在新国家组内重新用原生“首选”固定节点。
   - 仅重测 ChatGPT：准备 API 后仅对 ChatGPT URL 测三轮，检查对应报告、按上述国家得分规则选择排名前三的国家（不足三个按实际数量），只修改 `CHATGPT_COUNTRIES`。不要求 Claude 报告，不调用要求两项报告的全模板 render。

```sh
<CLI> --workspace "<WORKSPACE>" speedtest --url https://chatgpt.com/cdn-cgi/trace --rounds 3 --json
```

3. 先预览，再读取返回的 `diff_path`（工作区 `update.diff`），核实只有授权范围的变化。已有方案先读取、再针对性调整；用户没有提出变更时不要自动重测或重写：

```sh
<CLI> --workspace "<WORKSPACE>" update --json
```

可用 `--script` 指定独立候选，默认工作区 `good-cat.generated.js`。预览仅写私有工作区，不停止应用或更改绑定。向用户展示变更摘要、目标标签和短暂断网影响，**必须在预览之后取得第二次明确确认**；初始调整请求或 API 开启授权不能代替更新应用确认。

```sh
<CLI> --workspace "<WORKSPACE>" update --apply --json
```

预览会锁定本次候选的哈希；候选或导出基线发生变化后必须重新预览并再次确认，`--apply` 不会自动重做预览。应用在生命周期锁内停止应用，再校验当前订阅、绑定、标签和源码哈希，只更新同一 ID、同一标签的脚本，不写其他订阅绑定。共享脚本拒绝更新；基线过期、绑定变化或待恢复日志均须停止，不能自动覆盖。重新导出后重新制作候选、预览并确认；成功更新后旧基线不可复用。

## 恢复与异常

- `controller restore --backup <开启时返回的备份路径>` 默认仅预览，确认后加 `--apply` 恢复 API 的原设置。
- 有未完成的 controller 操作：`controller restore --apply` 使用本地恢复日志。
- 有未完成的安装：`recover --apply` 恢复数据库和脚本。日志和 `SQLite`/脚本备份都在 `.good-cat/state/`、`.good-cat/backups/`，不在 FlClash 应用数据目录。先向用户说明可能断网并获得确认。
- 发现未完成事务时，不自动绕过、删除日志或重启不一致配置。断电/强杀不能保证自动恢复；Windows 的目录落盘保证与 Unix 不同。
- 自动发现失败可使用 `--support-dir`、`--app`；`--preferences` 仅用于明确文件格式的高级/测试场景，macOS 正常使用原生 CFPreferences，不直接改 live plist。
- 不展示订阅链接、节点凭据、数据库内容或 Secret；报告和备份不上传、不提交版本库。
- 所有参数通过 `<CLI> --help` 和相应子命令的 `--help` 查询。
