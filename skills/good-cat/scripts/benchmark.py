"""Python HTTPS 计时 + 独立临时 Mihomo 代理进程；不启用或调用核心 API。"""
import base64
import copy
import hashlib
import http.client
import json
import math
import os
from pathlib import Path
import secrets
import shutil
import socket
import ssl
import statistics
import subprocess
import tempfile
import time
from urllib.parse import urlsplit

import flclash as f


class NodeUnavailable(Exception):
    """节点不支持隔离运行或临时核心启动失败；消息只包含固定错误码。"""


def load_nodes(path, notice_pattern):
    import re
    f.require(path.is_file(), '没有所选订阅的本地节点缓存；不能用运行配置或其他订阅代替')
    f.require(path.stat().st_size <= 32 * 1024 * 1024, '节点缓存超过 32MB，停止解析')
    raw = path.read_bytes()
    try:
        text = raw.decode('utf-8-sig')
        try:
            data = json.loads(text)
        except json.JSONDecodeError:
            try:
                import yaml
            except ImportError:
                raise ValueError('解析 YAML 缓存需要 PyYAML；请先经用户同意在项目虚拟环境安装') from None
            try:
                data = yaml.safe_load(text)
            except yaml.YAMLError:
                raise ValueError('订阅 YAML 无法解析；不输出含凭据的解析上下文') from None
    except UnicodeError:
        raise ValueError('节点缓存不是 UTF-8 文件') from None
    f.require(isinstance(data, dict) and isinstance(data.get('proxies'), list),
              '缓存须为含 proxies 数组的 Clash YAML/JSON；不支持原始订阅 URI/base64 或仅含 provider 引用的配置')
    nodes = []
    for node in data['proxies']:
        f.require(isinstance(node, dict) and isinstance(node.get('name'), str)
                  and node['name'] and isinstance(node.get('type'), str), '缓存含无效节点条目')
        if node['type'].lower() in ('direct', 'reject', 'pass') or re.search(notice_pattern, node['name']):
            continue
        nodes.append(node)
    f.require(nodes and len({n['name'] for n in nodes}) == len(nodes), '缓存没有实际节点或节点重名')
    return nodes, hashlib.sha256(raw).hexdigest()


def find_core(explicit=None):
    candidate = str(explicit) if explicit else (shutil.which('mihomo') or shutil.which('clash-meta'))
    f.require(candidate is not None, '缺少独立 Mihomo 可执行文件；由模型经用户同意安装或通过 --core 指定')
    path = Path(candidate).expanduser().resolve()
    f.require(path.is_file(), '指定核心文件不存在')
    try:
        result = subprocess.run([str(path), '-v'], capture_output=True, text=True, timeout=10, check=True)
    except (OSError, subprocess.SubprocessError):
        raise ValueError('无法执行独立核心；检查操作系统、CPU 架构和执行权限') from None
    version = result.stdout.strip()
    f.require('mihomo' in version.lower() or 'clash meta' in version.lower(), '需要兼容的独立 Mihomo CLI，不是 FlClash 动态库')
    return path, version[:200]


def isolated_config(node, port, username, password):
    # 单节点固定出口，不复制订阅中的 DNS/TUN/controller/规则/脚本。
    if node.get('dialer-proxy') or node.get('type', '').lower() in ('direct', 'reject', 'pass'):
        raise NodeUnavailable('unsupported_chained_or_direct_node')
    if not isinstance(node.get('server'), str) or not node['server'] or not node.get('port'):
        raise NodeUnavailable('missing_server_or_port')
    proxy = copy.deepcopy(node)
    proxy['name'] = 'GOODCAT_NODE'
    return {
        'port': port, 'socks-port': 0, 'mixed-port': 0, 'redir-port': 0, 'tproxy-port': 0,
        'allow-lan': False, 'bind-address': '127.0.0.1',
        'authentication': [username + ':' + password], 'skip-auth-prefixes': [],
        'mode': 'rule', 'log-level': 'silent', 'find-process-mode': 'off',
        'external-controller': '', 'external-controller-unix': '', 'external-controller-pipe': '',
        'tun': {'enable': False}, 'dns': {'enable': False},
        'profile': {'store-selected': False, 'store-fake-ip': False},
        'proxies': [proxy], 'proxy-groups': [], 'rules': ['MATCH,GOODCAT_NODE'],
    }


def probe_https(port, username, password, url, timeout_ms):
    """测量新连接的 CONNECT + TLS + GET 响应头耗时，不读取正文、不跟随重定向。"""
    parsed = urlsplit(url)
    f.require(parsed.scheme == 'https' and parsed.hostname and not parsed.username and not parsed.password
              and not parsed.fragment, '测试地址必须是无凭据、无 fragment 的 HTTPS URL')
    target = parsed.path or '/'
    if parsed.query:
        target += '?' + parsed.query
    token = base64.b64encode((username + ':' + password).encode()).decode('ascii')
    conn = http.client.HTTPSConnection('127.0.0.1', port, timeout=timeout_ms / 1000,
                                       context=ssl.create_default_context())
    # 显式 HTTP CONNECT，不读取 HTTP_PROXY/HTTPS_PROXY，不存在直连回退。
    conn.set_tunnel(parsed.hostname, parsed.port or 443, headers={'Proxy-Authorization': 'Basic ' + token})
    started = time.perf_counter()
    try:
        host = parsed.hostname
        if ':' in host:
            host = '[' + host + ']'
        if parsed.port and parsed.port != 443:
            host += ':' + str(parsed.port)
        conn.request('GET', target, headers={'Host': host, 'Connection': 'close', 'User-Agent': 'good-cat/1'})
        response = conn.getresponse()
        elapsed = (time.perf_counter() - started) * 1000
        status = response.status
        if elapsed > timeout_ms:
            return dict(delay_ms=None, status=status, error='timeout')
        if not 200 <= status < 400:
            return dict(delay_ms=None, status=status, error=f'http_{status}')
        return dict(delay_ms=max(0.001, round(elapsed, 3)), status=status, error=None)
    except ssl.SSLError:
        return dict(delay_ms=None, status=None, error='tls_error')
    except (TimeoutError, socket.timeout):
        return dict(delay_ms=None, status=None, error='timeout')
    except (OSError, http.client.HTTPException):
        return dict(delay_ms=None, status=None, error='proxy_or_connection_error')
    finally:
        conn.close()


class NodeProxy:
    """只管理本次启动的进程；成功、异常及 KeyboardInterrupt 均清理。"""
    def __init__(self, core, node):
        self.core, self.node = core, node
        self.process = None
        self.directory = None

    def __enter__(self):
        self.directory = tempfile.TemporaryDirectory(prefix='goodcat-')
        try:
            with socket.socket() as reservation:
                reservation.bind(('127.0.0.1', 0))
                self.port = reservation.getsockname()[1]
            self.username, self.password = secrets.token_hex(12), secrets.token_hex(24)
            config = isolated_config(self.node, self.port, self.username, self.password)
            path = Path(self.directory.name) / 'config.json'
            try:
                f.private_write(path, f.dump(config))
            except (TypeError, ValueError, RecursionError):
                raise NodeUnavailable('unsupported_node_encoding') from None
            # 清理继承代理变量，避免核心内部下载等意外使用系统 HTTP 代理。
            env = {k: v for k, v in os.environ.items() if k.lower() not in
                   ('http_proxy', 'https_proxy', 'all_proxy', 'no_proxy')}
            try:
                self.process = subprocess.Popen(
                    [str(self.core), '-d', self.directory.name, '-f', str(path)],
                    cwd=self.directory.name, env=env, stdin=subprocess.DEVNULL,
                    stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)
            except OSError:
                raise NodeUnavailable('core_launch_failed') from None
            deadline = time.monotonic() + 10
            while time.monotonic() < deadline:
                if self.process.poll() is not None:
                    raise NodeUnavailable('core_rejected_node_or_port')
                try:
                    with socket.create_connection(('127.0.0.1', self.port), timeout=0.2):
                        pass
                    time.sleep(0.1)
                    if self.process.poll() is not None:
                        raise NodeUnavailable('core_rejected_node_or_port')
                    return self
                except OSError:
                    time.sleep(0.05)
            raise NodeUnavailable('core_start_timeout')
        except BaseException:
            self.close()
            raise

    def probe(self, url, timeout_ms):
        if self.process.poll() is not None:
            return dict(delay_ms=None, status=None, error='core_exited')
        return probe_https(self.port, self.username, self.password, url, timeout_ms)

    def close(self):
        try:
            if self.process is not None and self.process.poll() is None:
                self.process.terminate()
                try:
                    self.process.wait(timeout=3)
                except subprocess.TimeoutExpired:
                    self.process.kill()
                    self.process.wait(timeout=3)
        finally:
            if self.directory is not None:
                self.directory.cleanup()
                self.directory = None

    def __exit__(self, *exc):
        self.close()


def measure(nodes, provider, urls, proxy_factory, rounds=3, timeout_ms=5000, pause=1):
    f.require(2 <= rounds <= 10, '测速轮数必须为 2–10')
    f.require(600 < timeout_ms <= 30000, '超时须大于 600 且不超过 30000ms')
    readings = {group: {} for group in urls}
    for node in nodes:
        name = node['name']
        for group in urls:
            readings[group][name] = []
        try:
            with proxy_factory(node) as proxy:
                for turn in range(rounds):
                    for group, url in urls.items():
                        sample = proxy.probe(url, timeout_ms)
                        delay = sample.get('delay_ms')
                        if not (type(delay) in (int, float) and math.isfinite(delay) and delay > 0):
                            sample = dict(sample, delay_ms=None, error=sample.get('error') or 'invalid_delay')
                        readings[group][name].append(sample)
                    if turn < rounds - 1:
                        time.sleep(pause)
        except NodeUnavailable as exc:
            for group in urls:
                readings[group][name] = [dict(delay_ms=None, status=None, error=str(exc)) for _ in range(rounds)]
    groups = {}
    for group, per_node in readings.items():
        rows = []
        for name, samples in per_node.items():
            valid = [s['delay_ms'] for s in samples if s['delay_ms'] is not None and not s.get('error')]
            median = statistics.median(valid) if valid else None
            rows.append(dict(name=name, samples_ms=[s['delay_ms'] for s in samples],
                             statuses=[s.get('status') for s in samples], errors=[s.get('error') for s in samples],
                             median_ms=median, successes=len(valid), eligible=len(valid) == rounds and all(v <= 600 for v in valid)))
        rows.sort(key=lambda row: (row['median_ms'] is None, row['median_ms'] or float('inf'), row['name']))
        groups[group] = dict(url=urls[group], eligible=[r for r in rows if r['eligible']], all_nodes=rows)
    return dict(provider=provider, rounds=rounds, threshold_ms=600,
                metric='新连接 HTTP CONNECT + TLS + HTTPS GET 至响应头；不含核心启动时间',
                criterion='所有轮次 HTTP 2xx/3xx 成功且每轮延迟 <= 600ms；中位数排序；不跟随重定向', groups=groups)
