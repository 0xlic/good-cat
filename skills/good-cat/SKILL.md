---
name: good-cat
description: 优化 FlClash 当前代理。只读检查环境并确认订阅和参考自定义配置，以参考配置为模板保留策略组、规则集、测试地址和覆盖项；Python 自动逐节点测速，按每轮不超过 600ms 筛选，由模型选择两个地区并生成合格节点精确白名单。不退出 FlClash，不切换当前代理，不直接写运行中的数据库；未完成测速不交付半成品。
compatibility: Python 3.9+、FlClash 及可用订阅缓存。YAML 解析需 PyYAML，代理协议转发需独立 Mihomo CLI；不调用核心 API。数据库读取结构针对 macOS FlClash v0.9.1，其他版本/平台须核对路径和兼容性。
---

# good-cat：优化当前代理

始终用中文。先读 `references/workflow.md`、`references/optimization.md`。从本文件目录定位 `scripts/flclash.py`（只读 inspect/check）、`scripts/goodcat.py`（草案/测速/白名单/导出）及 `scripts/benchmark.py`（临时代理和 Python HTTPS 测速）。不要假设工作目录就是 Skill 目录。

## 不可破坏的约束

- **现有 FlClash 可能承载当前模型连接，整个会话必须保持现有代理可用。** 不退出、重启、强制结束 FlClash 或其核心，不切换当前配置/节点，不更改系统代理、TUN、DNS 或 controller。
- 不在线写 SQLite，不去掉底层 offline 检查来绕过限制。本流程禁止调用 `flclash.py apply/restore`。没有验证过的无中断创建方式时，保留私有计划并明确报告阻塞，不冒险写库。
- 原有配置和订阅只读。名字以 `GoodCat-XXX` 开头，重名则停止，不能覆盖已有同名配置。
- 默认只读；安装依赖、网络测速先征得同意。不上传订阅 URL、节点凭据、数据库或日志。所有产物位于项目已忽略的 `.flclash-local/`，输出路径必须未占用。
- **先测速，后筛选，再交付；未完成筛选不导入/创建配置。** 私有草案不等于已经创建自定义配置。
- 临时核心只负责节点协议转发，Python 负责请求计时；不是纯 Python 协议实现，不是核心 API 测速。不得用 TCP ping、系统直连或其他服务延迟代替逐节点 HTTPS 测试。

## 1. 检查环境和选择来源

1. 用系统工具确认 FlClash 已安装，定位其 SQLite 数据库。macOS 默认 `~/Library/Application Support/com.follow.clash/database.sqlite`；其他平台仅搜索合理应用数据目录或询问路径。不创建空数据库，未安装则停止。
2. 验证 Python >=3.9 及 sqlite3、ssl、http.client。缺失时先说明安装方案并询问同意；安装后重新验证。YAML 缓存需要 PyYAML，使用项目虚拟环境安装；测速需要可信独立 Mihomo CLI，核对来源、架构和 `-v`，缺失时停止测速或经同意安装，不用 FlClash 动态库冒充 CLI。
3. 执行 `python3 <SKILL_DIR>/scripts/flclash.py inspect`。列出订阅 ID/label/type，询问优化哪个订阅（即使只有一个也确认）；用户已明确选择则不重复询问。
4. 同时列出 custom 配置，请用户选择参考配置。不得假设叫 GoodCat。只读核对参考配置的四组结构、顺序、规则提供者、测试 URL、DNS/TUN 覆盖，不自行添加通用模板或替换规则集。
5. 确认所选订阅缓存来源，默认数据库同级 `profiles/<订阅ID>.yaml`。路径不同可指定 `--nodes-file`；不把其他订阅/运行配置作为节点缓存，不静默下载刷新订阅。
6. 确认优化目标和名称 `GoodCat-XXX`。订阅名超过约20字符或包含冗余套餐信息时，给出2–3个简短建议并询问命名。确认后生成草案：

```bash
python3 <GC_SCRIPT> create-plan --subscription-id <订阅ID> --template-id <参考custom ID> --name 'GoodCat-XXX' --out .flclash-local/draft.json
```

非默认数据库：`flclash.py --db <DB> inspect`；goodcat 子命令后加 `--db <DB>`。

## 2. 审阅草案，不写数据库

- 从参考配置复制四个组的名称（含图标）、顺序、类型、测试地址和额外参数；复制规则顺序、规则集引用、直连策略、覆盖项。创建时不复制旧行 ID 或界面选中/展开状态。
- 唯一明确的来源变更：四组均绑定所选订阅的原始数据库 label，并取消 `include-all-providers/include-all/include-all-proxies`，避免其他机场混入。向用户明确展示这一变化，不能默默继承跨机场来源。
- 仅三个业务 url-test 组的地区 filter 在草案阶段置空，之后必须以实际测速白名单填充；“节点筛选”的 exclude-filter/hidden 原样保留，不把公告排除项复制到其他组。
- 不添加 interval/tolerance，不更换 ChatGPT 测试地址，不用零散域名替代 OpenAI/Gemini/China 等已有规则集，不丢弃 LAN、Apple/Microsoft 或 DNS 模式覆盖。模板原来没有的规则也不能无条件新增。
- 显式说明私有草案尚未保存到 FlClash。

## 3. 保持当前代理运行，自动测速

说明测试地址、节点数×三个地址×轮数的请求量和超时，获得网络测速同意后执行：

```bash
python3 <GC_SCRIPT> benchmark --plan .flclash-local/draft.json --core <独立Mihomo路径> --rounds 3 --confirm-network --out .flclash-local/benchmark.json
```

不需要新建配置 ID、controller、secret、退出应用或手动切换节点。脚本自动管理回环认证代理和单节点核心，Python 每次新建 CONNECT/TLS/GET 连接，测试完成或异常时清理本次临时进程和凭据文件。不碰现有应用进程。

当前 TUN/VPN 可能影响测量路径，需记录环境并如实说明，不能为了消除影响关闭当前代理；无法保证隔离则将结果标为受当前网络影响，不承诺跨平台数字完全一致。

默认3轮。每组保留所有**每轮均成功、HTTP 2xx/3xx 且每轮 <=600ms**的节点，按中位数升序展示所有合格节点。失败保留错误和状态，不能记为0ms；无合格节点则停止交付。耗时包含 CONNECT+TLS+GET至响应头，不含核心启动，不跟随重定向，不证明AI服务解锁。

## 4. 选择地区，生成“地区 ∩ 合格节点”白名单

模型从各组排序结果依次识别两个不同国家地区，展示对应节点与延迟，结合用户偏好说明取舍；只有一个地区时明确说明，不能凑两个。地区只按节点名推断，含混时询问。

`regions.json` 格式示例（不是默认选择）：

```json
{"ChatGPT":["TW","JP"],"Gemini":["SG","US"],"默认代理":["SG","JP"]}
```

```bash
python3 <GC_SCRIPT> filter-plan --plan .flclash-local/draft.json --selection .flclash-local/regions.json --report .flclash-local/benchmark.json --out .flclash-local/final-plan.json
```

脚本核对来源、数据库指纹、URL，并重新检查每轮状态和延迟，不直接相信 eligible 标记。过滤项使用锚定的节点名称精确白名单，只有入选地区中的实际合格节点可进入；不会因为同地区而放入未达标或未来新增节点。未来刷新订阅需重新测速。

当前内置地区及公告模式见 `references/filters.json`。原模板的反引号、lookbehind 等语义不在本流程中擅自改写；最终白名单需在实际兼容核心中校验。

## 5. 导出及交付门槛

先检查最终计划：

```bash
python3 <DB_SCRIPT> check .flclash-local/final-plan.json
python3 <GC_SCRIPT> export-config --plan .flclash-local/final-plan.json --out .flclash-local/GoodCat-XXX.json
```

**当前限制：** `export-config` 尚不支持数据库规则提供者的无损编译；遇到 RULE_SET 会明确拒绝导出，而不是删除规则或降级成零散域名。所选模板含规则集时，只能交付已校验计划并报告“尚未创建，导出受阻”。不要把计划 JSON 当成可导入的 Clash 配置，也不要为了完成任务回退到退出应用写库。

无规则集模板的导出文件仍需核对 file provider 相对路径（从应用工作目录解析）、启用的覆盖键，并用可信兼容核心 `-t -f` 校验，再确认应用的导入方式与源数据是否可编辑；不把任意文件导入等同于创建 custom 数据库配置。只有经过验证的无中断创建方式才可以另行经用户同意执行。

最终分别报告：草案/测速/筛选/计划校验/配置导出/应用创建/生成文件/网络验证。未执行或阻塞明确列出，不把结构测试通过说成已经生效。用户稍后手动验证时，仅提供可确认的名称、产物路径和待验证事项，不擅自切换当前出口。
