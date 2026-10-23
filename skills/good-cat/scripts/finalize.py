#!/usr/bin/env python3
"""最终确认后脱离模型会话运行：正常退出 FlClash、离线备份写入、重启、记录结果。"""
import argparse
import json
import os
from pathlib import Path
import subprocess
import sys
import time

import flclash as f


def run_command(command, timeout=20):
    f.require(isinstance(command, list) and command and all(isinstance(s, str) and s for s in command),
              '应用控制命令必须是非空字符串参数数组，不使用 shell')
    return subprocess.run(command, stdin=subprocess.DEVNULL, stdout=subprocess.DEVNULL,
                          stderr=subprocess.DEVNULL, timeout=timeout, check=False).returncode


def app_commands(app_path=None, quit_command=None):
    if sys.platform == 'darwin':
        app = str(Path(app_path or '/Applications/FlClash.app').expanduser().resolve())
        f.require(Path(app).is_dir(), '未找到 FlClash.app，请指定 --app-path')
        # 使用 argv 而非把路径拼接进 AppleScript。
        quit_args = ['osascript', '-e', 'on run argv\ntell application (item 1 of argv) to quit\nend run', app]
        start_args = ['open', '-a', app]
    elif sys.platform == 'win32':
        f.require(app_path and Path(app_path).is_file(), 'Windows 需要 --app-path 指定 FlClash.exe')
        executable = str(Path(app_path).resolve())
        quit_args = ['powershell', '-NoProfile', '-NonInteractive', '-Command',
                     '$target=$env:GOODCAT_APP_PATH; Get-Process -ErrorAction SilentlyContinue | '
                     'Where-Object { $_.Path -eq $target } | ForEach-Object { [void]$_.CloseMainWindow() }']
        start_args = [executable]
    else:
        f.require(app_path and Path(app_path).is_file(), 'Linux 需要 --app-path 指定应用可执行文件')
        f.require(quit_command is not None, 'Linux 需指定已验证的正常退出命令，不使用 kill/pkill 代替')
        quit_args = quit_command
        start_args = [str(Path(app_path).resolve())]
    if quit_command is not None:
        quit_args = quit_command
    return quit_args, start_args


def save_report(path, value):
    temporary = path.with_name(path.name + '.tmp')
    f.private_write(temporary, f.dump(value))
    os.replace(temporary, path)


def wait_offline(timeout=15):
    deadline = time.monotonic() + timeout
    while True:
        try:
            f.offline()
            return
        except ValueError:
            if time.monotonic() >= deadline:
                raise ValueError('FlClash 未完全退出，停止写入；不强制终止进程')
            time.sleep(0.3)


def preflight(db_path, plan):
    f.require(plan.get('profile_id') is None, '仅创建新 custom，不覆盖现有配置')
    business = [g for g in plan.get('groups', []) if g.get('type') == 'url-test']
    f.require(len(business) == 3 and all(json.loads(g['definition']).get('filter', '').startswith('^(')
                                      for g in business), '尚未完成三个组的节点白名单')
    with f.connect(db_path) as db:
        db.execute('BEGIN')
        prepared = f.prepare(f.snapshot(db), plan)
        memory = f.sqlite3.connect(':memory:')
        try:
            memory.row_factory = f.sqlite3.Row
            db.backup(memory)
            memory.execute('PRAGMA foreign_keys=ON')
            f.apply(memory, prepared)
        finally:
            memory.close()


def execute(job, report_path):
    report = dict(status='running', saved=False, restart_requested=False,
                  name=job['plan']['profile']['label'], backup=None, profile_id=None,
                  runtime_verified=False)
    save_report(report_path, report)
    restart_needed = False
    try:
        db_path = Path(job['db'])
        plan = job['plan']
        preflight(db_path, plan)  # 退出前也检查；不合格时完全不动应用。
        restart_needed = True
        try:
            run_command(job['quit'])
        except subprocess.TimeoutExpired:
            pass  # 只看最终进程状态，不把退出命令返回码当成已退出。
        wait_offline()
        # 不重新设置 base：退出产生数据变化时停止写入，仍重新打开应用。
        preflight(db_path, plan)
        with f.connect(db_path) as db:
            report['backup'] = f.backup(db, db_path.parent / 'skill-backups')
        save_report(report_path, report)
        f.offline()
        with f.connect(db_path, readonly=False) as db:
            db.execute('BEGIN EXCLUSIVE')
            prepared = f.prepare(f.snapshot(db), plan)
            pid = f.apply(db, prepared)
            db.commit()
        report.update(saved=True, profile_id=pid, status='saved')
        with f.connect(db_path) as db:
            row = db.execute('SELECT label,type FROM profiles WHERE id=?', (pid,)).fetchone()
            f.require(row is not None and row['label'] == report['name'] and row['type'] == 'custom',
                      '写入后读取验证失败')
        report['source_verified'] = True
    except Exception as exc:
        report.update(status='failed', error=str(exc))
    finally:
        if restart_needed:
            try:
                # Windows/Linux GUI 应用可能持续运行，不能等待其退出。
                subprocess.Popen(job['start'], stdin=subprocess.DEVNULL, stdout=subprocess.DEVNULL,
                                 stderr=subprocess.DEVNULL, close_fds=True)
                report['restart_requested'] = True
            except OSError as exc:
                report['restart_error'] = type(exc).__name__
        report['finished_at'] = time.strftime('%Y-%m-%dT%H:%M:%S%z')
        save_report(report_path, report)
    return report


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--worker', type=Path, help=argparse.SUPPRESS)
    parser.add_argument('--db', type=Path, default=f.default_path())
    parser.add_argument('--plan', type=Path)
    parser.add_argument('--out-dir', type=Path)
    parser.add_argument('--app-path')
    parser.add_argument('--quit-command', help='已验证正常退出命令的 JSON 参数数组；不执行 shell')
    parser.add_argument('--confirm-disconnect', action='store_true', help='仅在用户最终确认短暂断网后使用')
    args = parser.parse_args()
    if args.worker:
        job = json.loads(args.worker.read_text(encoding='utf-8'))
        execute(job, args.worker.parent / 'result.json')
        return
    f.require(args.confirm_disconnect and args.plan and args.out_dir, '需要最终断网确认、--plan 和 --out-dir')
    f.require(args.db and args.db.is_file(), '数据库不存在')
    plan = json.loads(args.plan.read_text(encoding='utf-8'))
    preflight(args.db, plan)
    quit_args, start_args = app_commands(args.app_path, json.loads(args.quit_command) if args.quit_command else None)
    args.out_dir.mkdir(parents=True, exist_ok=False, mode=0o700)
    path = args.out_dir.resolve() / 'job.json'
    f.private_write(path, f.dump(dict(db=str(args.db.resolve()), plan=plan, quit=quit_args, start=start_args)))
    env = dict(os.environ)
    if sys.platform == 'win32':
        env['GOODCAT_APP_PATH'] = str(Path(args.app_path).resolve())
    options = {'creationflags': subprocess.DETACHED_PROCESS | subprocess.CREATE_NEW_PROCESS_GROUP} if sys.platform == 'win32' else {'start_new_session': True}
    process = subprocess.Popen([sys.executable, str(Path(__file__).resolve()), '--worker', str(path)],
                               stdin=subprocess.DEVNULL, stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL,
                               close_fds=True, env=env, **options)
    print(f.dump(dict(worker_pid=process.pid, report=str(path.parent / 'result.json'),
                      message='已启动独立任务；即将短暂断网。稍后查看报告并自行选择新配置验证，勿重复执行。')))


if __name__ == '__main__':
    try:
        main()
    except (ValueError, OSError, KeyError, TypeError, f.sqlite3.Error) as exc:
        print('错误：' + str(exc), file=sys.stderr)
        sys.exit(1)
