"""最终离线操作使用合成数据库和模拟应用控制，不退出本机应用。"""
import json
from pathlib import Path
import tempfile
import unittest
from unittest.mock import patch

import flclash as f
import finalize as z
import test_flclash


class FinalizeTests(unittest.TestCase):
    def setUp(self):
        self.temp = tempfile.TemporaryDirectory()
        self.root = Path(self.temp.name)
        fixture = test_flclash.Tests()
        fixture.setUp()
        self.path = Path(f.backup(fixture.db, self.root))
        fixture.tearDown()
        with f.connect(self.path) as db:
            state = f.snapshot(db)
        groups = [dict(name=name, type='url-test', order=str(i), use=None, proxies=None,
                       definition=json.dumps({'filter': '^(JP 01)$'}))
                  for i, name in enumerate(('ChatGPT', 'Gemini', '默认代理'))]
        self.plan = dict(version=1, base=f.digest(state), profile_id=None,
                         profile={'label': 'GoodCat-test'}, groups=groups,
                         rules=[dict(rule_action='MATCH', rule_target='默认代理', order='a0')])
        self.job = dict(db=str(self.path), plan=self.plan, quit=['fake-quit'], start=['fake-start'])
        self.report = self.root / 'result.json'

    def tearDown(self):
        self.temp.cleanup()

    @patch.object(z.subprocess, 'Popen')
    @patch.object(z, 'run_command')
    @patch.object(z, 'wait_offline')
    @patch.object(f, 'offline')
    def test_success_and_backup(self, offline, wait, run, popen):
        result = z.execute(self.job, self.report)
        self.assertTrue(result['saved'])
        self.assertTrue(result['source_verified'])
        self.assertTrue(Path(result['backup']).is_file())
        self.assertTrue(result['restart_requested'])
        popen.assert_called_once()
        with f.connect(self.path) as db:
            self.assertEqual(db.execute('SELECT COUNT(*) FROM profiles').fetchone()[0], 2)
        self.assertEqual(json.loads(self.report.read_text())['status'], 'saved')

    @patch.object(z.subprocess, 'Popen')
    @patch.object(z, 'run_command')
    @patch.object(z, 'wait_offline', side_effect=ValueError('still running'))
    def test_quit_failure_does_not_write_but_reopens(self, wait, run, popen):
        result = z.execute(self.job, self.report)
        self.assertFalse(result['saved'])
        self.assertTrue(result['restart_requested'])
        with f.connect(self.path) as db:
            self.assertEqual(db.execute('SELECT COUNT(*) FROM profiles').fetchone()[0], 1)

    @patch.object(z.subprocess, 'Popen')
    @patch.object(z, 'run_command')
    def test_preflight_failure_does_not_touch_app(self, run, popen):
        self.plan['base'] = 'stale'
        result = z.execute(self.job, self.report)
        self.assertFalse(result['saved'])
        run.assert_not_called()
        popen.assert_not_called()

    @patch.object(z.subprocess, 'Popen')
    @patch.object(z, 'run_command')
    def test_change_on_quit_stops_write_and_reopens(self, run, popen):
        def change():
            with f.connect(self.path, readonly=False) as db:
                db.execute('UPDATE profiles SET label=? WHERE id=1', ('changed',))
                db.commit()
        with patch.object(z, 'wait_offline', side_effect=change):
            result = z.execute(self.job, self.report)
        self.assertFalse(result['saved'])
        self.assertTrue(result['restart_requested'])
        self.assertIn('数据库已变化', result['error'])

    def test_rejects_existing_target_or_unfiltered_draft(self):
        self.plan['profile_id'] = 1
        with self.assertRaises(ValueError):
            z.preflight(self.path, self.plan)
        self.plan['profile_id'] = None
        self.plan['groups'][0]['definition'] = '{"filter":""}'
        with self.assertRaises(ValueError):
            z.preflight(self.path, self.plan)


if __name__ == '__main__':
    unittest.main()
