---
name: good-cat
description: 优化 FlClash 当前代理。只读检查后，一次询问要优化的订阅和新配置名称，说明安装独立 Mihomo、联网测速及后续流程，统一确认继续。使用 Skill 内置标准，不参考本机其他自定义配置；自动逐节点测速、选择地区并生成每轮不超过600ms的精确节点白名单。测速期间保持当前代理运行；最后明确询问短暂断网许可，确认后由独立脚本退出应用、备份创建custom并重启，用户自行验证。不重复询问模板、安装或测速许可。
compatibility: Python 3.9+、FlClash及本地订阅缓存；YAML解析需PyYAML，节点协议转发需独立Mihomo。已核对macOS FlClash v0.9.1数据库结构。最终写入使用独立本地任务，不依赖模型保持联网。macOS支持正常退出/重启；Windows需指定exe路径，Linux另需验证过的正常退出命令。不强制杀进程。
---

# good-cat：优化当前代理

始终用中文。先读 `references/workflow.md`、`references/optimization.md`。从 Skill 目录定位脚本，不假设工作目录就是 Skill 目录。

## 交互约定：只问两项，一次确认

先只读检查FlClash安装、数据库路径、Python/PyYAML、独立Mihomo和订阅缓存。未安装FlClash或无订阅则说明阻碍，不能继续。不要让用户提供数据库ID或技术方案。

正常流程只需收集：
1. 要优化哪个订阅（列出名称）。
2. 新自定义配置叫什么（格式 `GoodCat-XXX`，给出对应订阅的简短建议）。

一次性列出以上两项、说明后续工作，并统一询问是否继续。例如：

> 检查完成，可选订阅：咕嘎云、良心云。
> 请一次回复：要优化的订阅、新配置名称（建议 GoodCat-咕嘎 / GoodCat-良心），以及是否继续。
> 继续后我会补齐运行依赖；测速需要独立 Mihomo，我会安装官方对应系统/架构版本，不启动后台服务。随后按内置标准自动生成草案，对该订阅全部节点执行三轮 ChatGPT、Gemini 和 Google HTTPS 测试（约节点数×9次请求，单次默认5秒超时），逐组选择两个低延迟地区，保留每轮均≤600ms的节点，校验并准备新配置。测速期间保持当前代理不变；全部就绪后，我会最后询问是否允许短暂断网，再通过独立脚本退出FlClash、备份创建custom并重启，之后由你自行验证。

用户已提供的订阅/名称不重复询问；名称太长可在这一次询问中给建议。统一“继续”即覆盖已说明的常规依赖安装和联网测速，不再逐项询问安装Mihomo、是否联网、测试地址、轮数、地区、参考模板或偏好。模型根据实际排序自动选择地区并报告理由。只有出现未覆盖的管理员权限、破坏性操作、无效命名/冲突或确实阻塞的问题才说明并请求必要输入，不能为了减少提问隐瞒失败。

## 标准来源

**仅使用 Skill 自带 `references/standard.json`。** 不询问参考配置，不依赖用户电脑存在 GoodCat，不读取其他custom作为模板，不使用 `--template-id`。

内置四组依次为 🤖 ChatGPT、✨ Gemini、🚀 默认代理、🔎节点筛选。全部明确绑定所选订阅数据库label；三个业务组初始filter为空，不设置include-all系列，不额外加入interval/tolerance或公告排除。公告排除仅在隐藏的节点筛选组。测试地址分别为ChatGPT trace、Gemini首页和Google generate_204。

内置私网/LAN/Apple/Microsoft/国内直连、ChatGPT/Gemini常见域名及MATCH规则，不依赖用户已添加OpenAI/Gemini/China规则提供者。域名清单属于版本化标准，不冒充完整服务规则集；本机geodata须校验兼容性。内置DNS模式为redir-host，不复制其他配置覆盖项，不修改当前运行环境。

## 自动执行顺序

1. 根据用户统一确认，补齐缺少的依赖。Python>=3.9，标准库sqlite3/ssl/http.client；PyYAML安装在项目虚拟环境；独立Mihomo核实官方来源、系统/架构和发布校验信息，使用 `-v` 验证。不得使用FlClash动态库冒充CLI，不擅自提权或安装后台服务。
2. 确认数据库订阅label、ID和缓存归属。macOS数据库默认 `~/Library/Application Support/com.follow.clash/database.sqlite`，缓存默认同级 `profiles/<订阅ID>.yaml`。其他路径核实后指定 `--db` / `--nodes-file`；不替换成其他订阅或运行配置，不上传节点凭据。
3. 在项目已忽略的 `.flclash-local/` 生成草案（文件名不得已存在）：

```bash
python3 <SKILL_DIR>/scripts/goodcat.py create-plan --subscription-id <订阅ID> --name 'GoodCat-XXX' --out .flclash-local/draft.json
```

4. 自动测速，无需先写数据库，无需用户切换配置/节点、controller或secret：

```bash
python3 <SKILL_DIR>/scripts/goodcat.py benchmark --plan .flclash-local/draft.json --core <独立Mihomo路径> --rounds 3 --confirm-network --out .flclash-local/benchmark.json
```

Python启动仅回环认证端口的临时独立核心，固定逐节点出口；Python发起CONNECT+TLS+HTTPS GET并计时至响应头，不跟随重定向。所有轮次HTTP2xx/3xx且每轮≤600ms才合格，按中位数排序。每个服务独立测速，保留全部结果，不用直连或TCP ping冒充。失败记错误，不记0ms。完成/异常时清理本次进程和临时凭据，不碰现有FlClash。当前VPN/TUN可能影响测量，应记录而非关闭。
5. 模型逐组从最快合格节点选择最先出现的两个不同地区，只有一个时选一个并说明；无合格节点则报告阻塞，不放宽阈值或生成空白半成品。按 `filters.json` 识别地区，生成地区代码文件，不再次征求选择：

```json
{"ChatGPT":["TW","JP"],"Gemini":["SG","US"],"默认代理":["SG","JP"]}
```

此示例不是预定选择，必须依据实际结果。

```bash
python3 <SKILL_DIR>/scripts/goodcat.py filter-plan --plan .flclash-local/draft.json --selection .flclash-local/regions.json --report .flclash-local/benchmark.json --out .flclash-local/final-plan.json
python3 <SKILL_DIR>/scripts/flclash.py check .flclash-local/final-plan.json
python3 <SKILL_DIR>/scripts/goodcat.py export-config --plan .flclash-local/final-plan.json --out .flclash-local/GoodCat-XXX.json
```

脚本重查provider/base/URL及每轮结果，生成“入选地区∩实际合格节点”的精确名称正则，不放入同地区未达标或新增节点。计划JSON不是Clash配置，不能混用。

6. 核对导出配置的file provider路径相对于应用工作目录能解析，并用兼容核心 `-t -f` 校验。数据库check不能替代核心校验；失败应在退出应用前解决。最终计划已校验后，展示名称、入选节点/地区和将写入的差异。

## 最后确认：允许短暂断网后再执行

此确认必须在全部测速和校验完成后进行，与开始时的继续确认不同：

> 已完成测速和筛选，即将退出FlClash、备份并创建新配置，然后重新启动应用。期间会短暂断网，模型连接可能中断，但独立本地任务会继续执行并保存结果。是否执行？执行后请自行选择GoodCat-XXX验证，无需等待模型恢复。

没有明确同意不能启动。提前告知结果路径；获得同意后使用一次新的任务目录：

```bash
python3 <SKILL_DIR>/scripts/finalize.py --plan .flclash-local/final-plan.json --out-dir .flclash-local/finalize-<时间戳> --confirm-disconnect
```

非默认数据库加 `--db`，macOS非默认应用位置加 `--app-path`；Windows传exe路径；Linux传应用路径及 `--quit-command '["已验证的正常退出程序","参数"]'`。模型应预先检查平台退出/启动方式，不临时猜测，更不能使用kill/pkill/taskkill作为正常退出命令。

启动器先校验计划，再创建脱离会话的本地进程，重定向标准输入输出，不依赖后续模型调用。任务按顺序：再次检查 → 正常退出 → 确认含核心完全退出 → 重新校验原base → 备份 → 独占事务创建新custom → 读回确认 → 尝试重新打开应用 → 保存result.json。退出超时不强杀；退出导致数据指纹变化时停止写入，不改base绕过检查；只要尝试过退出，即使失败也尝试重启。事务失败不自动整库恢复，避免覆盖其他数据。

启动后不反复重试，不重新创建同名配置，不等待用户现场验收。结果见任务目录result.json，包含saved、profile_id、backup、source_verified、restart_requested及error。restart_requested仅表示启动请求已发出，不证明应用启动或内核生效。系统关机或强制杀死任务不保证完成，重试前必须先查报告与数据库是否已有同名配置。

## 完成报告

断网前告知产物/报告路径和“任务完成后请自行选择新配置验证；如应用未打开请手动启动”。若模型之后能读取报告，再分别报告创建、重启请求、生成配置、核心校验与网络验证状态。没有结果报告不能宣称创建成功，用户自行验证，不自动选择新出口。
