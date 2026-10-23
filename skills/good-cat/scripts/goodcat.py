#!/usr/bin/env python3
"""使用内置标准生成配置，先测速再导出；不停止 FlClash、不调用核心 API。"""
import argparse
import copy
import json
from pathlib import Path
import re
import sys
import time
from urllib.parse import urlsplit

import flclash as f
import benchmark as b

URLS = {
    'ChatGPT': 'https://chatgpt.com/cdn-cgi/trace',
    'Gemini': 'https://gemini.google.com/',
    '默认代理': 'https://www.gstatic.com/generate_204',
}
PRESETS = json.loads((Path(__file__).resolve().parent.parent / 'references/filters.json').read_text(encoding='utf-8'))
NOTICE_FILTER = '(?i)(' + '|'.join(PRESETS['notices'].values()) + ')'


def role(name):
    for key in (*URLS, '节点筛选'):
        if name.endswith(key):
            return key
    raise ValueError('模板包含未支持的额外策略组，请先审阅结构')


def make_plan(state, subscription_id, name):
    f.require(name.startswith('GoodCat-') and name[8:].strip(), '名称必须为 GoodCat-XXX')
    subscriptions = [p for p in state['profiles'] if p['id'] == subscription_id and p['type'] != 'custom']
    f.require(len(subscriptions) == 1, '请选择存在的订阅 ID')
    label = subscriptions[0]['label']
    f.require(sum(p['label'] == label for p in state['profiles']) == 1, '订阅名称不唯一')
    standard = json.loads((Path(__file__).resolve().parent.parent / 'references/standard.json').read_text(encoding='utf-8'))
    profile = dict(standard['profile'], label=name)
    groups = []
    for i, item in enumerate(standard['groups']):
        group = copy.deepcopy(item)
        if role(group['name']) == '节点筛选':
            group['definition']['exclude-filter'] = NOTICE_FILTER
        group.update(use=f.dump([label]), proxies=None, order=f'a{i:06d}')
        group['definition'] = f.dump(group['definition'])
        groups.append(group)
    rules = copy.deepcopy(standard['rules'])
    for i, rule in enumerate(rules):
        rule['order'] = f'a{i:06d}'
    plan = dict(version=1, base=f.digest(state), profile_id=None, profile=profile, groups=groups, rules=rules)
    f.prepare(state, plan)
    targets(plan)
    return plan


def targets(plan):
    result, providers = {}, set()
    for group in plan['groups']:
        use = json.loads(group.get('use') or '[]')
        f.require(len(use) == 1, '所有组须绑定同一个所选订阅')
        providers.add(use[0])
        kind = role(group['name'])
        if kind in URLS:
            url = json.loads(group['definition']).get('url', '')
            parsed = urlsplit(url)
            f.require(parsed.scheme == 'https' and parsed.hostname and not parsed.username
                      and not parsed.password and not parsed.fragment, '测试地址必须为无凭据的 HTTPS URL')
            result[kind] = url
    f.require(len(providers) == 1 and set(result) == set(URLS), '草案组结构或订阅不一致')
    return providers.pop(), result


def filter_plan(plan, selection, report):
    f.require(plan.get('profile_id') is None, '只允许生成新配置，不更新已有配置')
    provider, urls = targets(plan)
    f.require(report.get('provider') == provider and report.get('base') == plan['base'], '测速报告与草案不匹配')
    f.require(set(selection) == set(URLS), '须分别指定三个业务组的地区')
    result = copy.deepcopy(plan)
    for group in result['groups']:
        kind = role(group['name'])
        if kind not in URLS:
            continue
        codes = selection[kind]
        f.require(isinstance(codes, list) and 1 <= len(codes) <= 2 and len(set(codes)) == len(codes)
                  and all(c in PRESETS['regions'] for c in codes), '每组须选择 1–2 个不同的内置地区')
        measured = report['groups'][kind]
        f.require(measured['url'] == urls[kind], '测试地址发生变化，需要重新测速')
        patterns = [re.compile(PRESETS['regions'][c]['pattern'], re.I) for c in codes]
        names = []
        for row in measured['all_nodes']:
            samples = row['samples_ms']
            statuses = row['statuses']
            errors = row['errors']
            good = (len(samples) == report['rounds'] and len(statuses) == len(samples)
                    and len(errors) == len(samples) and not any(errors)
                    and all(type(v) in (int, float) and 0 < v <= 600 for v in samples)
                    and all(type(s) is int and 200 <= s < 400 for s in statuses))
            if good and any(p.search(row['name']) for p in patterns):
                names.append(row['name'])
        f.require(names and all(any(p.search(n) for n in names) for p in patterns),
                  '所选地区没有每轮均 <=600ms 的合格节点，停止导出')
        definition = json.loads(group['definition'])
        # 用精确名称白名单表达“地区 ∩ 合格节点”，不会放入同地区未达标节点。
        # 不使用 Python re.escape：其对空格/标点的转义不一定兼容 Go RE2。
        escaped = [re.sub(r'([\\.^$|?*+()\[\]{}])', r'\\\1', n) for n in dict.fromkeys(names)]
        definition['filter'] = '^(' + '|'.join(escaped) + ')$'
        group['definition'] = f.dump(definition)
    return result


def export_config(state, plan):
    """导出标准 Clash 配置 JSON（也是 YAML），不复制数据库专用覆盖键。"""
    f.prepare(state, plan)
    targets(plan)
    providers = sorted({n for g in plan['groups'] for n in json.loads(g['use'])})
    config = {'proxy-providers': {}, 'proxy-groups': [], 'rule-providers': {}, 'rules': []}
    for label in providers:
        p = next(p for p in state['profiles'] if p['label'] == label and p['type'] != 'custom')
        config['proxy-providers'][label] = {'type': 'file', 'path': f'./profiles/{p["id"]}.yaml'}
    for group in plan['groups']:
        definition = json.loads(group['definition'])
        if role(group['name']) in URLS:
            f.require(definition.get('filter', '').startswith('^('), '未完成节点白名单筛选，不导出半成品')
        config['proxy-groups'].append(dict(definition, name=group['name'], type=group['type'],
                                           use=json.loads(group['use'])))
    for rule in sorted(plan['rules'], key=lambda r: r['order']):
        action = rule['rule_action']
        if action == 'RULE_SET':
            # 数据库 provider 编译涉及应用转换，不猜测定义或静默降级域名规则。
            raise ValueError('模板使用应用规则集；尚未实现无损导出规则提供者，请保留计划，勿导入不完整配置')
        parts = [action.replace('_', '-')]
        if action != 'MATCH':
            parts.append(rule['content'])
        parts.append(rule['rule_target'])
        if rule.get('no_resolve'):
            parts.append('no-resolve')
        config['rules'].append(','.join(parts))
    overrides = json.loads(plan['profile'].get('overrides') or '{}')
    for key, value in overrides.items():
        if key.endswith('-override-keys'):
            section = key[:-len('-override-keys')]
            config[section] = {k: overrides[section][k] for k in value}
        elif key not in ('dns', 'tun', 'ntp', 'sniffer', 'proxy-providers'):
            raise ValueError('存在尚未支持导出的覆盖字段，停止导出以避免丢失')
    return config


def read_state(path):
    f.require(path is not None and path.is_file(), '未找到数据库；请指定 --db')
    with f.connect(path) as db:
        db.execute('BEGIN')
        return f.snapshot(db)


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    sub = parser.add_subparsers(dest='cmd', required=True)
    create = sub.add_parser('create-plan')
    create.add_argument('--subscription-id', type=int, required=True)
    create.add_argument('--name', required=True)
    bench = sub.add_parser('benchmark')
    bench.add_argument('--core', type=Path)
    bench.add_argument('--nodes-file', type=Path)
    bench.add_argument('--rounds', type=int, default=3)
    bench.add_argument('--timeout-ms', type=int, default=5000)
    bench.add_argument('--confirm-network', action='store_true', required=True)
    filters = sub.add_parser('filter-plan')
    filters.add_argument('--selection', type=Path, required=True)
    filters.add_argument('--report', type=Path, required=True)
    export = sub.add_parser('export-config')
    for p in (create, bench, filters, export):
        p.add_argument('--db', type=Path, default=f.default_path())
        p.add_argument('--out', required=True)
        if p is not create:
            p.add_argument('--plan', type=Path, required=True)
    args = parser.parse_args()
    state = read_state(args.db)
    if args.cmd == 'create-plan':
        output = make_plan(state, args.subscription_id, args.name)
    else:
        plan = json.loads(args.plan.read_text(encoding='utf-8'))
        f.prepare(state, plan)
        if args.cmd == 'filter-plan':
            output = filter_plan(plan, json.loads(args.selection.read_text(encoding='utf-8')),
                                 json.loads(args.report.read_text(encoding='utf-8')))
        elif args.cmd == 'export-config':
            output = export_config(state, plan)
        else:
            f.require(not Path(args.out).exists(), '输出文件已存在')
            provider, urls = targets(plan)
            p = next(p for p in state['profiles'] if p['label'] == provider and p['type'] != 'custom')
            nodes_path = args.nodes_file or args.db.parent / 'profiles' / f'{p["id"]}.yaml'
            nodes, fingerprint = b.load_nodes(nodes_path, NOTICE_FILTER)
            core, version = b.find_core(args.core)
            output = b.measure(nodes, provider, urls, lambda node: b.NodeProxy(core, node), args.rounds, args.timeout_ms)
            output.update(subscription_id=p['id'], base=plan['base'], nodes_sha256=fingerprint,
                          core_version=version, measured_at=time.strftime('%Y-%m-%dT%H:%M:%S%z'))
    f.private_write(args.out, f.dump(output))
    print('已保存私有文件：' + args.out)


if __name__ == '__main__':
    try:
        main()
    except KeyboardInterrupt:
        print('已取消；临时代理已清理。', file=sys.stderr)
        sys.exit(130)
    except (ValueError, OSError, KeyError, TypeError, f.sqlite3.Error, f.subprocess.SubprocessError) as exc:
        print('错误：' + str(exc), file=sys.stderr)
        sys.exit(1)
