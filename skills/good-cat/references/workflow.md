# 数据模型和操作约定

GoodCat 标准优化流程见 `optimization.md`：准备、测速和校验期间保持当前代理运行；全部完成后最后确认短暂断网许可，再由 finalize.py 独立本地任务正常退出应用、备份、离线事务创建新custom、尝试重启并记录报告。不得中途退出、在线写库或修改base。下文底层维护能力本身不构成断网或整库恢复的授权。

## 配置源与生成文件

已在 FlClash v0.9.1 的 macOS SQLite 结构上核对：

- macOS 数据目录：`~/Library/Application Support/com.follow.clash/`。
- `database.sqlite` 是界面配置源；`profiles.type=custom` 是自定义配置，不要求对应 profiles/*.yaml 文件。
- `profiles` 保存名称和 overrides；`proxy_groups`、`rules` 通过 profile_id 归属配置。
- `profile_id IS NULL` 的规则是全局记录，不保证被当前配置编译采用。本脚本不改它们。
- `clash_providers` 保存规则提供者，订阅类型 profiles 可被编译为代理提供者；最终以生成文件为准。
- `config.yaml` 是应用生成产物，不持久编辑。订阅节点可能从 profiles/*.yaml 以 file provider 引入。
- Windows/Linux/便携版路径可能不同，向用户询问或仅在合理应用数据目录中搜索 database.sqlite，再用 `--db` 指定；不要扫描整个磁盘或使用 Clash Verge 的数据库。

## 计划格式（version=1）

所有 ID 保持 Python 整数，勿经过会丢失 64 位精度的 JavaScript 数字处理链。base 是脚本导出的内容指纹，不能手动重置。

```json
{
  "version": 1,
  "base": "由 export 自动生成",
  "profile_id": null,
  "profile": {"label": "GoodCat-日常", "overrides": "{}"},
  "groups": [
    {
      "name": "默认代理",
      "type": "url-test",
      "proxies": null,
      "use": "[\"用户已有订阅名称\"]",
      "definition": "{\"url\":\"https://www.gstatic.com/generate_204\",\"interval\":300}",
      "order": "a0"
    }
  ],
  "rules": [
    {"rule_action": "GEOSITE", "content": "private", "rule_target": "DIRECT", "order": "a0"},
    {"rule_action": "GEOIP", "content": "CN", "rule_target": "DIRECT", "no_resolve": 1, "order": "a1"},
    {"rule_action": "MATCH", "rule_target": "默认代理", "order": "a2"}
  ]
}
```

该示例仅说明格式，不是通用推荐；必须替换订阅名称，并依据本机 geodata 和需求校验。

- `profile_id: null` 新建 custom；现有配置使用导出的 ID。
- profile 只允许 label、overrides、current_group_name；现有配置的其他字段保持不变。新建使用 v0.9.1 的已知默认字段，数据库版本不同会在演练时失败，不能猜测补列。
- groups/rules 新行省略 id；原行保留 id。不得把其他配置的行 ID 搬过来。
- `definition`、`overrides`、`proxies`、`use` 均为**JSON 字符串**（与数据库格式一致），不是嵌套对象。用 Python json.dumps 生成，不手写拼接转义。
- `definition` 放策略组额外字段，如 filter、exclude-filter、url、interval、tolerance、include-all-providers、hidden；结构字段 name/type/proxies/use 不放其中。
- rules 使用数据库枚举 `RULE_SET`，不是核心 YAML 的 `RULE-SET`；rule_provider 是已存在规则提供者名称。其余常见动作是 GEOSITE/GEOIP/DOMAIN/DOMAIN_SUFFIX/DOMAIN_KEYWORD/MATCH；不同版本的数据库枚举必须由同机现有记录或源码确认，不能照搬 YAML 连字符名。
- order 是区分大小写的字典序字符串，不是整数。保留原值，插入时选择相邻间的新值；也可在审阅完整集合后重排为固定宽度 `a000001`、`a000002` 等，避免 `a10` 排在 `a2` 前。数组展示顺序不是生效顺序。
- overlays 示例：修改 DNS enhanced-mode 时，在原 overrides 中改 dns.enhanced-mode，并将 enhanced-mode 加入 dns-override-keys；不要丢弃原有键，也不要把整个 dns 默认对象误当全部启用。
- 脚本有意不管理规则提供者、订阅、脚本扩展、自定义节点、dialer、全局规则和 disabled_rules。高级 sub_rule/src 规则停止自动写入。组间循环、订阅节点名称、完整正则和核心字段兼容性不在脚本校验范围内，需模型审查与核心验证。

## 事务与恢复

check 在内存数据库副本执行完整更新；apply 要求应用退出，先做 SQLite 在线备份，再获取独占事务、比较源数据指纹、写入并检查外键和完整性。脚本不修改运行文件、不启动应用、不强制终止服务。不能完全消除其他进程在检查后启动的竞争，操作期间不要重启应用。

备份保存在数据目录 `skill-backups/`，权限尽可能限制为当前用户；包含订阅等敏感数据，不上传。JSON 计划也默认 0600。Windows 权限仍需依赖目录 ACL。

恢复是**整库恢复**，会影响备份后的其他配置更新，不只是目标配置。先说明这个影响并取得同意，退出应用，再 inspect 取得当前 fingerprint：

```bash
python3 <SCRIPT> inspect
python3 <SCRIPT> restore <备份.sqlite> --expect <当前fingerprint> --confirm
```

恢复前也会备份当前数据库，并校验备份 schema/完整性。恢复后由用户启动应用重新应用配置。不要在应用运行时直接复制数据库文件覆盖，也不要只复制有 WAL 的主文件。不要将旧配置简单覆盖到新版本数据库。
