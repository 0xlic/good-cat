#!/usr/bin/env python3
"""FlClash 自定义配置的只读检查、离线事务更新与备份。仅使用 Python 标准库。"""
import argparse
import copy
import hashlib
import json
import os
from pathlib import Path
import secrets
import sqlite3
import subprocess
import sys
import time

TABLES = ('profiles', 'proxy_groups', 'rules', 'clash_providers',
          'custom_proxies', 'disabled_rules', 'proxy_dialers')
JSON_FIELDS = {'overrides': dict, 'selected_map': dict, 'unfold_set': list,
               'definition': dict, 'proxies': list, 'use': list}
PROFILE_FIELDS = {'label', 'overrides', 'current_group_name'}
GROUP_FIELDS = {'id', 'name', 'type', 'proxies', 'use', 'definition', 'order'}
RULE_FIELDS = {'id', 'rule_action', 'content', 'rule_target', 'rule_provider',
               'sub_rule', 'no_resolve', 'src', 'order'}


def require(ok, message):
    if not ok:
        raise ValueError(message)


def dump(value):
    return json.dumps(value, ensure_ascii=False, indent=2)


def connect(path, readonly=True):
    db = sqlite3.connect(path.resolve().as_uri() + ('?mode=ro' if readonly else '?mode=rw'), uri=True)
    db.row_factory = sqlite3.Row
    db.execute('PRAGMA foreign_keys=ON')
    db.execute('PRAGMA busy_timeout=5000')
    return db


def snapshot(db):
    result = {'schema': [list(r) for r in db.execute(
        "SELECT name,sql FROM sqlite_master WHERE type='table' ORDER BY name")]}
    for table in TABLES:
        result[table] = [dict(r) for r in db.execute(f'SELECT * FROM "{table}" ORDER BY rowid')]
    return result


def digest(state):
    return hashlib.sha256(json.dumps(state, sort_keys=True, ensure_ascii=False).encode()).hexdigest()


def offline():
    # 失败时不假定进程已退出；不自动终止用户应用。
    if sys.platform == 'win32':
        out = subprocess.run(['tasklist', '/FO', 'CSV', '/NH'], capture_output=True, text=True, check=True).stdout
    else:
        out = subprocess.run(['ps', '-A', '-o', 'comm='], capture_output=True, text=True, check=True).stdout
    require(not any('flclash' in line.lower() for line in out.splitlines()),
            '请完全退出 FlClash（含后台服务）后重试；不要只关闭窗口。')


def private_write(path, text):
    path = Path(path)
    path.parent.mkdir(parents=True, exist_ok=True)
    fd = os.open(path, os.O_CREAT | os.O_EXCL | os.O_WRONLY, 0o600)
    with os.fdopen(fd, 'w', encoding='utf-8') as f:
        f.write(text + '\n')


def backup(db, directory):
    directory.mkdir(parents=True, exist_ok=True, mode=0o700)
    path = directory / (time.strftime('%Y%m%d-%H%M%S-') + secrets.token_hex(4) + '.sqlite')
    fd = os.open(path, os.O_CREAT | os.O_EXCL | os.O_WRONLY, 0o600)
    os.close(fd)
    with sqlite3.connect(path) as target:
        db.backup(target)
    return str(path)


def defaults(label):
    return dict(type='custom', label=label, current_group_name=None, url='', age_secret_key=None,
                last_update_date=None, extend_type='standard', script_id=None, match_target=None,
                auto_update_duration_millis=86400000, subscription_info=None, auto_update=0,
                selected_map='{}', unfold_set='[]', order=None, overrides='{}')


def validate_json(row):
    for key, kind in JSON_FIELDS.items():
        if key in row and row[key] is not None:
            require(isinstance(row[key], str), f'{key} 必须是 JSON 字符串（数据库原始格式）')
            require(isinstance(json.loads(row[key]), kind), f'{key} 的 JSON 类型必须为 {kind.__name__}')


def prepare(state, plan):
    require(plan.get('version') == 1, '不支持的计划版本')
    require(set(plan) <= {'version', 'base', 'profile_id', 'profile', 'groups', 'rules'}, '未知计划字段')
    require(plan.get('base') == digest(state), '数据库已变化，请重新导出计划，勿覆盖界面更新')
    pid = plan.get('profile_id')
    matches = [p for p in state['profiles'] if p['id'] == pid]
    new = pid is None
    require(new or len(matches) == 1, '找不到配置 ID')
    original = None if new else matches[0]
    require(new or original['type'] == 'custom', '只允许修改自定义配置，禁止修改订阅')
    patch = plan.get('profile', {})
    require(isinstance(patch, dict) and set(patch) <= PROFILE_FIELDS, '不允许的 profile 字段')
    profile = defaults(patch.get('label', '')) if new else copy.deepcopy(original)
    profile.update(patch)
    require(isinstance(profile['label'], str) and profile['label'].strip(), '配置名不能为空')
    require(not any(p['label'] == profile['label'] and p['id'] != pid for p in state['profiles']), '配置名重复')
    validate_json(profile)
    result = {'profile': profile, 'new': new, 'profile_id': pid}
    for key, table, allowed in [('groups', 'proxy_groups', GROUP_FIELDS), ('rules', 'rules', RULE_FIELDS)]:
        old = [r for r in state[table] if r['profile_id'] == pid] if not new else []
        desired = plan.get(key, [{k: v for k, v in r.items() if k != 'profile_id'} for r in old])
        require(isinstance(desired, list), f'{key} 必须是数组')
        ids, orders = set(), set()
        oldids = {r['id'] for r in old}
        for row in desired:
            require(isinstance(row, dict) and set(row) <= allowed, f'{key} 含未知字段')
            if row.get('id') is not None:
                require(row['id'] in oldids and row['id'] not in ids, 'ID 必须来自该配置且不可重复；新行省略 id')
                ids.add(row['id'])
            order = row.get('order')
            require(isinstance(order, str) and order.isascii() and order and order not in orders,
                    '每行须有唯一 ASCII order，按字典序排序')
            orders.add(order)
            validate_json(row)
        removed = oldids - ids
        if key == 'rules':
            require(not any(r['rule_id'] in removed for r in state['disabled_rules']),
                    '待删除规则被 disabled_rules 引用，需先在界面处理')
        result[key] = desired
    groups = result['groups']
    names = [g.get('name') for g in groups]
    require(all(isinstance(n, str) and n for n in names) and len(names) == len(set(names)), '策略组名为空或重复')
    proxy_names = {json.loads(p['definition']).get('name') for p in state['custom_proxies']}
    targets = set(names) | proxy_names | {'DIRECT', 'REJECT', 'REJECT-DROP', 'PASS', 'COMPATIBLE'}
    providers = {p['label'] for p in state['profiles'] if p['type'] != 'custom'}
    ruleproviders = {p['label'] for p in state['clash_providers'] if p['kind'] == 'rule'}
    for g in groups:
        require(g.get('type') in {'select', 'url-test', 'fallback', 'load-balance', 'relay'}, '未知策略组类型')
        definition = json.loads(g.get('definition', '{}'))
        require(not ({'name', 'type', 'proxies', 'use'} & set(definition)), 'definition 不得重复结构字段')
        require(set(json.loads(g.get('use') or '[]')) <= providers, 'use 引用了不存在的订阅提供者')
        require(g['name'] not in json.loads(g.get('proxies') or '[]'), '策略组不可引用自身')
        # 订阅中的节点名称不在数据库中，交给核心测试，不能以数据库缺失为由拒绝。
    rules = sorted(result['rules'], key=lambda r: r['order'])
    for i, r in enumerate(rules):
        require(r.get('rule_action') and r.get('rule_target') in targets, '规则动作为空或目标不存在')
        require(not r.get('sub_rule') and not r.get('src', 0), '此版本脚本不支持 sub_rule/src 高级规则')
        require(r.get('no_resolve', 0) in (0, 1), 'no_resolve 必须为 0/1')
        if r['rule_action'] == 'RULE_SET':
            require(r.get('rule_provider') in ruleproviders, '规则集不存在，请先在界面添加规则提供者')
        elif r['rule_action'] == 'MATCH':
            require(i == len(rules) - 1, 'MATCH 必须最后，避免后续规则不可达')
        else:
            require(bool(r.get('content')), '非 MATCH / RULE_SET 规则须有 content')
    return result


def insert(db, table, row):
    columns = list(row)
    db.execute(f'INSERT INTO "{table}" (' + ','.join(f'"{c}"' for c in columns) +
               ') VALUES (' + ','.join('?' for _ in columns) + ')', [row[c] for c in columns])


def new_id(db, table):
    # 正整数、SQLite int64 范围；不依赖 Dart Snowflake 的生成器状态。
    while True:
        value = secrets.randbelow(2**62 - 1) + 1
        if not db.execute(f'SELECT 1 FROM "{table}" WHERE id=?', (value,)).fetchone():
            return value


def apply(db, prepared):
    profile = prepared['profile']
    if prepared['new']:
        pid = new_id(db, 'profiles')
        insert(db, 'profiles', dict(profile, id=pid))
    else:
        pid = prepared['profile_id']
        fields = sorted(PROFILE_FIELDS)
        db.execute('UPDATE profiles SET ' + ','.join(f'"{f}"=?' for f in fields) + ' WHERE id=?',
                   [profile[f] for f in fields] + [pid])
    for key, table in [('groups', 'proxy_groups'), ('rules', 'rules')]:
        desired = prepared[key]
        keep = {r['id'] for r in desired if r.get('id') is not None}
        for row in db.execute(f'SELECT id FROM "{table}" WHERE profile_id=?', (pid,)).fetchall():
            if row['id'] not in keep:
                db.execute(f'DELETE FROM "{table}" WHERE id=?', (row['id'],))
        for row in desired:
            row = dict(row)
            rid = row.pop('id', None)
            base = (dict(proxies=None, use=None, definition='{}') if key == 'groups' else
                    dict(content=None, rule_provider=None, sub_rule=None, no_resolve=0, src=0))
            base.update(row)
            if rid is None:
                insert(db, table, dict(base, id=new_id(db, table), profile_id=pid))
            else:
                db.execute(f'UPDATE "{table}" SET ' + ','.join(f'"{c}"=?' for c in base) +
                           ' WHERE id=? AND profile_id=?', list(base.values()) + [rid, pid])
    require(not db.execute('PRAGMA foreign_key_check').fetchall(), '外键检查失败')
    require(db.execute('PRAGMA integrity_check').fetchone()[0] == 'ok', '数据库完整性检查失败')
    return pid


def default_path():
    if sys.platform == 'darwin':
        return Path.home() / 'Library/Application Support/com.follow.clash/database.sqlite'
    return None


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--db', type=Path, default=default_path(), help='非 macOS 或自定义安装必须指定数据库路径')
    sub = parser.add_subparsers(dest='cmd', required=True)
    sub.add_parser('inspect')
    export = sub.add_parser('export')
    choice = export.add_mutually_exclusive_group(required=True)
    choice.add_argument('--profile-id', type=int)
    choice.add_argument('--new', metavar='NAME')
    export.add_argument('--out', required=True)
    for command in ('check', 'apply'):
        p = sub.add_parser(command)
        p.add_argument('plan', type=Path)
        if command == 'apply':
            p.add_argument('--confirm', action='store_true', required=True)
    restore = sub.add_parser('restore')
    restore.add_argument('backup', type=Path)
    restore.add_argument('--expect', required=True, help='inspect 输出的当前 fingerprint')
    restore.add_argument('--confirm', action='store_true', required=True)
    args = parser.parse_args()
    require(args.db is not None and args.db.is_file(), '未找到数据库，请通过 --db 显式指定；不会自动创建')
    if args.cmd in ('apply', 'restore'):
        offline()
    with connect(args.db) as db:
        db.execute('BEGIN')
        state = snapshot(db)
        if args.cmd == 'inspect':
            print(dump({'fingerprint': digest(state), 'profiles': [
                {k: r[k] for k in ('id', 'type', 'label')} for r in state['profiles']],
                'rule_providers': [{k: r[k] for k in ('label', 'kind', 'behavior', 'format')}
                                   for r in state['clash_providers']]}))
            return
        if args.cmd == 'export':
            plan = dict(version=1, base=digest(state), profile_id=args.profile_id)
            if args.new:
                plan.update(profile={'label': args.new}, groups=[], rules=[])
            else:
                found = [p for p in state['profiles'] if p['id'] == args.profile_id and p['type'] == 'custom']
                require(len(found) == 1, '找不到该自定义配置')
                plan['profile'] = {k: found[0][k] for k in PROFILE_FIELDS}
                for key, table in [('groups', 'proxy_groups'), ('rules', 'rules')]:
                    plan[key] = [{k: v for k, v in r.items() if k != 'profile_id'} for r in state[table]
                                 if r['profile_id'] == args.profile_id]
            private_write(args.out, dump(plan))
            print('已导出私有计划：' + args.out)
            return
        if args.cmd == 'restore':
            require(args.backup.resolve() != args.db.resolve(), '备份不得指向当前数据库')
            require(args.expect == digest(state), '当前数据库已变化，停止恢复')
            with connect(args.backup) as source:
                require(source.execute('PRAGMA integrity_check').fetchone()[0] == 'ok', '备份损坏')
                require(snapshot(source)['schema'] == state['schema'], '备份 schema 不兼容')
            prepared = None
        else:
            plan = json.loads(args.plan.read_text(encoding='utf-8'))
            prepared = prepare(state, plan)
            # 在内存副本中演练 SQL、约束及完整性检查，check 不触碰真实数据库。
            memory = sqlite3.connect(':memory:')
            memory.row_factory = sqlite3.Row
            db.backup(memory)
            memory.execute('PRAGMA foreign_keys=ON')
            apply(memory, prepared)
            memory.close()
            if args.cmd == 'check':
                print('检查通过：数据库结构、引用和事务演练；不代表核心语法/网络可用性验证通过。')
                return
        db.rollback()
        saved = backup(db, args.db.parent / 'skill-backups')
        print('备份：' + saved, flush=True)
    offline()
    with connect(args.db, readonly=False) as db:
        db.execute('BEGIN EXCLUSIVE')
        require(digest(snapshot(db)) == digest(state), '备份后数据变化，停止写入')
        if args.cmd == 'restore':
            # backup API 在目标有活动事务时不可用；离线且重复检查后再恢复。
            db.rollback()
            with connect(args.backup) as source:
                source.backup(db)
            print('整库已恢复（包含其他配置）；请启动 FlClash 并重新应用配置。')
        else:
            pid = apply(db, prepared)
            db.commit()
            print(f'已保存自定义配置 {pid}；尚未生效，请启动 FlClash 并在界面选择/重新应用配置。')


if __name__ == '__main__':
    try:
        main()
    except (ValueError, sqlite3.Error, OSError, KeyError, TypeError, subprocess.SubprocessError) as exc:
        print('错误：' + str(exc), file=sys.stderr)
        sys.exit(1)
