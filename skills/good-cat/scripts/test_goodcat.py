"""模板继承、测速白名单及只读流程回归测试；不访问真实配置。"""
import copy
import json
import re
import unittest

import flclash as f
import goodcat as g
import test_flclash


class PlanTests(unittest.TestCase):
    def setUp(self):
        self.fixture = test_flclash.Tests()
        self.fixture.setUp()
        self.db = self.fixture.db
        f.insert(self.db, 'profiles', dict(f.defaults('动态订阅'), id=2, type='url'))
        overrides = {'dns': {'enhanced-mode': 'redir-host', 'enable': False},
                     'dns-override-keys': ['enhanced-mode']}
        self.db.execute('UPDATE profiles SET overrides=? WHERE id=1', (f.dump(overrides),))
        for i, (name, url) in enumerate(g.URLS.items()):
            f.insert(self.db, 'proxy_groups', dict(id=10+i, profile_id=1, name='🤖 '+name, type='url-test',
                     proxies=None, use=None, definition=f.dump({'url': url, 'filter': '原地区',
                     'include-all-providers': True}), order=f'a{i}'))
        f.insert(self.db, 'proxy_groups', dict(id=14, profile_id=1, name='🔎节点筛选', type='select',
                 proxies=None, use='["动态订阅"]', definition=f.dump({'hidden': True, 'exclude-filter': '原排除'}), order='a3'))
        f.insert(self.db, 'clash_providers', dict(id=1, label='OpenAI', kind='rule', behavior='classical', format='yaml'))
        f.insert(self.db, 'rules', dict(id=1, profile_id=1, rule_action='RULE_SET', rule_provider='OpenAI',
                 rule_target='🤖 ChatGPT', order='a0'))
        f.insert(self.db, 'rules', dict(id=2, profile_id=1, rule_action='MATCH', rule_target='🤖 默认代理', order='a1'))
        self.db.commit()
        self.state = f.snapshot(self.db)
        self.plan = g.make_plan(self.state, 2, 'GoodCat-AI', 1)

    def tearDown(self):
        self.fixture.tearDown()

    def report(self):
        groups = {}
        for name, url in g.URLS.items():
            rows = []
            for node, samples in [('JP / 01', [100, 200, 300]), ('JP slow', [100, 700, 100]),
                                   ('TW (02)', [600, 600, 600]), ('TW failed', [100, None, 100])]:
                rows.append(dict(name=node, samples_ms=samples, statuses=[200]*3, errors=[None]*3))
            groups[name] = dict(url=url, all_nodes=rows)
        return dict(provider='动态订阅', base=self.plan['base'], rounds=3, groups=groups)

    def selection(self):
        return {name: ['JP', 'TW'] for name in g.URLS}

    def test_clone_preserves_names_order_rules_overrides(self):
        self.assertEqual(self.plan['profile']['overrides'], self.state['profiles'][0]['overrides'])
        self.assertEqual([r['name'] for r in self.plan['groups']], [r['name'] for r in self.state['proxy_groups']])
        self.assertEqual([r['order'] for r in self.plan['groups']], ['a0','a1','a2','a3'])
        self.assertEqual(self.plan['rules'][0]['rule_provider'], 'OpenAI')
        self.assertEqual(len(self.plan['rules']), 2)
        for row in self.plan['groups']:
            self.assertNotIn('id', row)
            self.assertEqual(json.loads(row['use']), ['动态订阅'])
        for row in self.plan['groups'][:3]:
            definition = json.loads(row['definition'])
            self.assertNotIn('include-all-providers', definition)
            self.assertNotIn('exclude-filter', definition)
            self.assertNotIn('interval', definition)
            self.assertEqual(definition['filter'], '')
        self.assertEqual(json.loads(self.plan['groups'][-1]['definition']),
                         {'hidden': True, 'exclude-filter': '原排除'})
        self.assertEqual(f.digest(f.snapshot(self.db)), f.digest(self.state))

    def test_exact_allowlist_excludes_slow_nodes_from_selected_region(self):
        before = copy.deepcopy(self.plan)
        result = g.filter_plan(self.plan, self.selection(), self.report())
        self.assertEqual(before, self.plan)
        for row in result['groups'][:3]:
            regex = json.loads(row['definition'])['filter']
            for node in ('JP / 01', 'TW (02)'):
                self.assertIsNotNone(re.fullmatch(regex, node))
            for node in ('JP slow', 'TW failed', 'JP new', 'TW (02) extra'):
                self.assertIsNone(re.fullmatch(regex, node))
        self.assertEqual(result['rules'], before['rules'])
        self.assertEqual(result['profile'], before['profile'])
        self.assertEqual(result['groups'][-1], before['groups'][-1])

    def test_report_mismatch_and_empty_regions_fail(self):
        for field, value in [('provider', '其他订阅'), ('base', 'stale')]:
            report = self.report()
            report[field] = value
            with self.assertRaises(ValueError):
                g.filter_plan(self.plan, self.selection(), report)
        selection = self.selection()
        selection['ChatGPT'] = ['US']
        with self.assertRaises(ValueError):
            g.filter_plan(self.plan, selection, self.report())

    def test_existing_profile_cannot_be_filter_target(self):
        self.plan['profile_id'] = 1
        with self.assertRaises(ValueError):
            g.filter_plan(self.plan, self.selection(), self.report())

    def test_export_refuses_draft_and_unsupported_rule_provider(self):
        with self.assertRaises(ValueError):
            g.export_config(self.state, self.plan)
        filtered = g.filter_plan(self.plan, self.selection(), self.report())
        with self.assertRaisesRegex(ValueError, '规则集'):
            g.export_config(self.state, filtered)

    def test_bad_template_and_name(self):
        for template, name in [(999, 'GoodCat-AI'), (2, 'GoodCat-AI'), (1, 'GoodCat-')]:
            with self.assertRaises(ValueError):
                g.make_plan(self.state, 2, name, template)


if __name__ == '__main__':
    unittest.main()
