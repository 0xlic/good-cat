# good-cat

优化小猫咪的使用体验：让模型根据现有机场订阅，制定更合适的分流、节点选择和故障切换方案。

## 环境要求

- 已安装 FlClash，并有可用的订阅。
- 能够执行本地命令的 AI 智能体。
- Python 3.9+，用于运行配置检查、保存和 HTTPS 测速工具。
- 测速需要独立 Mihomo 可执行文件；读取 YAML 节点缓存需要 PyYAML。模型会在一次流程说明中告知安装需求，用户确认继续后自动补齐，不重复询问。

目前针对 macOS FlClash v0.9.1 验证数据库结构；其他平台需要显式提供数据库路径，其他版本需先检查兼容性。

测速由 Python 自动管理临时代理进程并发起 HTTPS 请求，不使用 Clash 核心 API，无需手动切换节点或配置。临时核心只负责节点协议转发，测试后自动清理。

## 安装

直接把 GitHub 地址交给大模型，让它检查环境，并将 Skill 完整复制到项目根目录：

> 帮我安装这个 Skill：https://github.com/0xlic/good-cat

## 使用

告诉模型你的使用习惯和优化目标，例如：

> 帮我优化当前代理

模型只会一次询问要优化的订阅和新配置名称，说明后续依赖安装、联网测速等步骤，再统一确认是否继续。不需要已有 GoodCat 配置，不询问参考模板或重复请求测速许可。

## 安全边界

- 保持 FlClash 和当前代理运行，不退出、重启或切换出口，不在线写数据库。
- 使用 Skill 内置标准配置，不参考本机其他自定义配置；先测速，再生成入选地区中每轮均不超过 600ms 的节点精确白名单。
- 不修改已有配置、机场订阅或运行文件。未完成筛选不创建半成品，不把计划校验通过说成已生效。
- 当前支持标准配置文件导出，但运行中应用的无中断 custom 创建尚未实现；会在继续确认前披露限制，保留最终计划，不通过退出应用写库绕过。

## 文件

- [`SKILL.md`](skills/good-cat/SKILL.md)：模型执行规范。
- [`references/workflow.md`](skills/good-cat/references/workflow.md)：字段约定、限制和恢复。
- [`references/optimization.md`](skills/good-cat/references/optimization.md)：建组、测速与地区筛选约定。
- [`references/filters.json`](skills/good-cat/references/filters.json)：内置地区与公告过滤规则。
- [`references/standard.json`](skills/good-cat/references/standard.json)：不依赖本机模板的标准配置。
- [`scripts/flclash.py`](skills/good-cat/scripts/flclash.py)：配置检查、校验、备份与保存工具。
- [`scripts/goodcat.py`](skills/good-cat/scripts/goodcat.py)：建组计划、节点测速与地区过滤计划。
- [`scripts/benchmark.py`](skills/good-cat/scripts/benchmark.py)：无 API 的隔离代理与 HTTPS 测速。
- [`scripts/test_flclash.py`](skills/good-cat/scripts/test_flclash.py)、[`scripts/test_goodcat.py`](skills/good-cat/scripts/test_goodcat.py)、[`scripts/test_benchmark.py`](skills/good-cat/scripts/test_benchmark.py)：合成数据与模拟网络测试。
