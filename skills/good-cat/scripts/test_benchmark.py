"""仅合成节点、模拟 HTTP 和子进程，不使用用户订阅或外网。"""
import json
from pathlib import Path
import ssl
import tempfile
import unittest
from unittest.mock import MagicMock, patch

import benchmark as b
import goodcat as g


class FakeProxy:
    opened = []
    closed = []

    def __init__(self, node):
        self.name = node['name']
        self.counts = {}

    def __enter__(self):
        self.opened.append(self.name)
        if self.name == 'broken':
            raise b.NodeUnavailable('unsupported')
        return self

    def __exit__(self, *exc):
        self.closed.append(self.name)

    def probe(self, url, timeout_ms):
        count = self.counts.get(url, 0)
        self.counts[url] = count + 1
        delay = {'JP': [100, 200, 300], 'TW': [600, 600, 600], 'US': [601, 601, 601],
                 'SG': [100, 0, 100], 'KR': [float('nan')] * 3}[self.name][count]
        if url == g.URLS['Gemini'] and self.name == 'JP':
            return dict(delay_ms=None, status=403, error='http_403')
        return dict(delay_ms=delay, status=200, error=None)


class BenchmarkTests(unittest.TestCase):
    def test_rounds_threshold_independent_groups_and_cleanup(self):
        FakeProxy.opened.clear()
        FakeProxy.closed.clear()
        nodes = [{'name': name} for name in ('JP', 'TW', 'US', 'SG', 'KR', 'broken')]
        report = b.measure(nodes, '所选订阅', g.URLS, FakeProxy, pause=0)
        self.assertEqual(FakeProxy.opened, [n['name'] for n in nodes])
        self.assertEqual(FakeProxy.closed, [n['name'] for n in nodes[:-1]])
        for name, group in report['groups'].items():
            expected = ['TW'] if name == 'Gemini' else ['JP', 'TW']
            self.assertEqual([n['name'] for n in group['eligible']], expected)
            self.assertEqual(len(group['all_nodes']), 6)
            for row in group['all_nodes']:
                self.assertEqual(len(row['samples_ms']), 3)
            broken = next(r for r in group['all_nodes'] if r['name'] == 'broken')
            self.assertEqual(broken['errors'], ['unsupported'] * 3)

    def test_load_nodes_json_and_duplicates(self):
        with tempfile.TemporaryDirectory() as tmp:
            path = Path(tmp) / 'nodes.json'
            nodes = [{'name': 'JP 01', 'type': 'ss'}, {'name': '剩余流量: 10GB', 'type': 'ss'},
                     {'name': '直连', 'type': 'direct'}]
            path.write_text(json.dumps({'proxies': nodes}), encoding='utf-8')
            result, fingerprint = b.load_nodes(path, g.NOTICE_FILTER)
            self.assertEqual(result, nodes[:1])
            self.assertEqual(len(fingerprint), 64)
            path.write_text(json.dumps({'proxies': [nodes[0], nodes[0]]}), encoding='utf-8')
            with self.assertRaises(ValueError):
                b.load_nodes(path, g.NOTICE_FILTER)

    def test_isolated_config_disables_api_and_forces_node(self):
        node = dict(name='node', type='ss', server='example.com', port=443, password='private')
        config = b.isolated_config(node, 12345, 'local', 'secret')
        self.assertEqual(node['name'], 'node')
        self.assertEqual(config['proxies'][0]['name'], 'GOODCAT_NODE')
        self.assertEqual(config['rules'], ['MATCH,GOODCAT_NODE'])
        self.assertEqual(config['external-controller'], '')
        self.assertFalse(config['tun']['enable'])
        self.assertFalse(config['dns']['enable'])
        self.assertEqual(config['bind-address'], '127.0.0.1')
        self.assertEqual(config['authentication'], ['local:secret'])
        self.assertEqual(config['skip-auth-prefixes'], [])
        with self.assertRaises(b.NodeUnavailable):
            b.isolated_config(dict(node, **{'dialer-proxy': 'other'}), 12345, 'local', 'secret')

    @patch.object(b.http.client, 'HTTPSConnection')
    def test_https_explicit_connect_and_timer(self, connection):
        conn = connection.return_value
        conn.getresponse.return_value.status = 204
        with patch.object(b.time, 'perf_counter', side_effect=[10.0, 10.123]):
            result = b.probe_https(12345, 'u', 'p', 'https://example.com/path?q=1', 5000)
        self.assertEqual(result, dict(delay_ms=123.0, status=204, error=None))
        self.assertEqual(connection.call_args.args, ('127.0.0.1', 12345))
        self.assertEqual(conn.set_tunnel.call_args.args, ('example.com', 443))
        self.assertIn('Proxy-Authorization', conn.set_tunnel.call_args.kwargs['headers'])
        self.assertEqual(conn.request.call_args.args, ('GET', '/path?q=1'))
        self.assertNotIn('Proxy-Authorization', conn.request.call_args.kwargs['headers'])
        conn.close.assert_called_once()

    @patch.object(b.http.client, 'HTTPSConnection')
    def test_http_denial_tls_failure_no_retry_or_direct_fallback(self, connection):
        conn = connection.return_value
        conn.getresponse.return_value.status = 403
        self.assertEqual(b.probe_https(12345, 'u', 'p', 'https://example.com', 5000)['error'], 'http_403')
        conn.request.side_effect = ssl.SSLError('private diagnostic')
        result = b.probe_https(12345, 'u', 'p', 'https://example.com', 5000)
        self.assertEqual(result['error'], 'tls_error')
        self.assertNotIn('private', json.dumps(result))
        self.assertEqual(conn.close.call_count, 2)
        self.assertEqual(connection.call_count, 2)

    @patch.object(b.socket, 'create_connection')
    @patch.object(b.subprocess, 'Popen')
    def test_temp_process_cleanup_on_interrupt(self, popen, connect):
        process = popen.return_value
        process.poll.return_value = None
        node = dict(name='JP', type='ss', server='example.com', port=443)
        with self.assertRaises(KeyboardInterrupt):
            with b.NodeProxy(Path('/fake/mihomo'), node) as proxy:
                directory = Path(proxy.directory.name)
                self.assertTrue((directory / 'config.json').exists())
                raise KeyboardInterrupt()
        process.terminate.assert_called_once()
        process.wait.assert_called_once()
        self.assertFalse(directory.exists())

    @patch.object(b.subprocess, 'Popen')
    def test_failed_start_cleanup(self, popen):
        popen.return_value.poll.return_value = 1
        proxy = b.NodeProxy(Path('/fake/mihomo'), dict(name='JP', type='ss', server='example.com', port=443))
        with self.assertRaises(b.NodeUnavailable):
            proxy.__enter__()
        self.assertIsNone(proxy.directory)
        popen.return_value.terminate.assert_not_called()


if __name__ == '__main__':
    unittest.main()
