"""内置标准、白名单及只读流程回归测试，不使用本机配置。"""
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
        # 无任何可作为模板的 custom 配置。
        self.db.execute('DELETE FROM profiles')
        f.insert(self.db, 'profiles', dict(f.defaults('动态订阅'), id=2, type='url'))
        self.db.commit()
        self.state = f.snapshot(self.db)
        self.plan = g.make_plan(self.state, 2, 'GoodCat-AI')

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

    def test_standard_without_existing_custom(self):
        self.assertEqual([r['name'] for r in self.plan['groups']],
                         ['🤖 ChatGPT', '✨ Gemini', '🚀 默认代理', '🔎节点筛选'])
        for row in self.plan['groups']:
            self.assertNotIn('id', row)
            self.assertEqual(json.loads(row['use']), ['动态订阅'])
        for row in self.plan['groups'][:3]:
            definition = json.loads(row['definition'])
            self.assertNotIn('include-all-providers', definition)
            self.assertNotIn('exclude-filter', definition)
            self.assertEqual(definition['filter'], '')
        self.assertIn('exclude-filter', json.loads(self.plan['groups'][-1]['definition']))
        self.assertFalse(any(r['rule_action'] == 'RULE_SET' for r in self.plan['rules']))
        self.assertEqual(f.digest(f.snapshot(self.db)), f.digest(self.state))

    def test_existing_custom_is_not_used(self):
        f.insert(self.db, 'profiles', dict(f.defaults('GoodCat'), id=3, overrides='{}'))
        state = f.snapshot(self.db)
        plan = g.make_plan(state, 2, 'GoodCat-AI')
        self.assertEqual(plan['groups'], self.plan['groups'])
        self.assertEqual(plan['rules'], self.plan['rules'])
        self.assertEqual(plan['profile'], self.plan['profile'])

    def test_exact_allowlist_excludes_slow_nodes(self):
        before = copy.deepcopy(self.plan)
        result = g.filter_plan(self.plan, self.selection(), self.report())
        self.assertEqual(before, self.plan)
        for row in result['groups'][:3]:
            regex = json.loads(row['definition'])['filter']
            for node in ('JP / 01', 'TW (02)'):
                self.assertIsNotNone(re.fullmatch(regex, node))
            for node in ('JP slow', 'TW failed', 'JP new'):
                self.assertIsNone(re.fullmatch(regex, node))
        self.assertEqual(result['rules'], before['rules'])
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

    def test_export_requires_filtering_and_preserves_dns(self):
        with self.assertRaises(ValueError):
            g.export_config(self.state, self.plan)
        plan = g.filter_plan(self.plan, self.selection(), self.report())
        config = g.export_config(self.state, plan)
        self.assertEqual(config['dns'], {'enhanced-mode': 'redir-host'})
        self.assertEqual(len(config['proxy-groups']), 4)
        self.assertEqual(len(config['rules']), len(plan['rules']))

    def test_invalid_name_or_subscription(self):
        for sid, name in [(1, 'GoodCat-AI'), (2, 'GoodCat-'), (2, 'Other')]:
            with self.assertRaises(ValueError):
                g.make_plan(self.state, sid, name)


if __name__ == '__main__':
    unittest.main()
