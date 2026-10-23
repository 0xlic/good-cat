"""纯合成数据库测试，不访问本机 FlClash。运行：python3 -m unittest discover -s <scripts目录>"""
import copy
import importlib.util
from pathlib import Path
import sqlite3
import tempfile
import unittest

spec = importlib.util.spec_from_file_location('flclash', Path(__file__).with_name('flclash.py'))
f = importlib.util.module_from_spec(spec)
spec.loader.exec_module(f)


class Tests(unittest.TestCase):
    def setUp(self):
        self.db = sqlite3.connect(':memory:')
        self.db.row_factory = sqlite3.Row
        self.db.execute('PRAGMA foreign_keys=ON')
        defaults = f.defaults('测试')
        cols = ','.join('"' + k + '" ' + ('INTEGER' if isinstance(v, int) else 'TEXT') for k, v in defaults.items())
        self.db.executescript('''
            CREATE TABLE profiles(id INTEGER PRIMARY KEY, %s);
            CREATE TABLE proxy_groups(id INTEGER PRIMARY KEY, profile_id INTEGER REFERENCES profiles(id),
              name TEXT NOT NULL, type TEXT NOT NULL, proxies TEXT, use TEXT, definition TEXT NOT NULL DEFAULT '{}', "order" TEXT);
            CREATE TABLE rules(id INTEGER PRIMARY KEY, profile_id INTEGER REFERENCES profiles(id),
              rule_action TEXT NOT NULL, content TEXT, rule_target TEXT, rule_provider TEXT, sub_rule TEXT,
              no_resolve INTEGER DEFAULT 0, src INTEGER DEFAULT 0, "order" TEXT);
            CREATE TABLE clash_providers(id INTEGER PRIMARY KEY, kind TEXT, label TEXT, behavior TEXT, format TEXT);
            CREATE TABLE custom_proxies(id INTEGER PRIMARY KEY, definition TEXT);
            CREATE TABLE disabled_rules(profile_id INTEGER, rule_id INTEGER REFERENCES rules(id));
            CREATE TABLE proxy_dialers(profile_id INTEGER, proxy_id INTEGER, target TEXT);
        ''' % cols)
        f.insert(self.db, 'profiles', dict(defaults, id=1))
        self.db.commit()
        self.state = f.snapshot(self.db)
        self.plan = dict(version=1, base=f.digest(self.state), profile_id=1,
                         profile={'label': '优化后'}, groups=[], rules=[
                             dict(rule_action='MATCH', rule_target='DIRECT', order='a0')])

    def tearDown(self):
        self.db.close()

    def test_update(self):
        f.apply(self.db, f.prepare(self.state, self.plan))
        self.assertEqual(self.db.execute('SELECT label FROM profiles').fetchone()[0], '优化后')
        self.assertEqual(self.db.execute('SELECT COUNT(*) FROM rules').fetchone()[0], 1)

    def test_create(self):
        self.plan['profile_id'] = None
        pid = f.apply(self.db, f.prepare(self.state, self.plan))
        self.assertNotEqual(pid, 1)
        self.assertEqual(self.db.execute('SELECT type FROM profiles WHERE id=?', (pid,)).fetchone()[0], 'custom')

    def test_stale(self):
        self.plan['base'] = 'stale'
        with self.assertRaises(ValueError):
            f.prepare(self.state, self.plan)

    def test_bad_target(self):
        self.plan['rules'][0]['rule_target'] = '不存在'
        with self.assertRaises(ValueError):
            f.prepare(self.state, self.plan)

    def test_subscription_rejected(self):
        state = copy.deepcopy(self.state)
        state['profiles'][0]['type'] = 'url'
        self.plan['base'] = f.digest(state)
        with self.assertRaises(ValueError):
            f.prepare(state, self.plan)

    def test_backup(self):
        with tempfile.TemporaryDirectory() as tmp:
            path = f.backup(self.db, Path(tmp))
            with f.connect(Path(path)) as db:
                self.assertEqual(f.digest(f.snapshot(db)), f.digest(self.state))

    def test_match_not_last(self):
        self.plan['rules'].append(dict(rule_action='GEOSITE', content='private', rule_target='DIRECT', order='a1'))
        with self.assertRaises(ValueError):
            f.prepare(self.state, self.plan)

    def test_omit_preserves(self):
        f.apply(self.db, f.prepare(self.state, self.plan))
        state = f.snapshot(self.db)
        plan = dict(version=1, base=f.digest(state), profile_id=1, profile={})
        f.apply(self.db, f.prepare(state, plan))
        self.assertEqual(self.db.execute('SELECT COUNT(*) FROM rules').fetchone()[0], 1)


if __name__ == '__main__':
    unittest.main()
