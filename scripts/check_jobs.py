#!/usr/bin/env python3
"""Offline shared-job CLI smoke test. Isolated state, pure data operations only."""
import concurrent.futures
import json
import os
from pathlib import Path
import subprocess
import tempfile
import time

ROOT = Path(__file__).resolve().parents[1]
BINARY = ROOT / 'target/debug/summonr'


def main():
    with tempfile.TemporaryDirectory(prefix='summonr-job-check-') as tmp:
        env = dict(os.environ, SUMMONR_HOME=tmp)
        process = None

        def run(*args, code=0):
            result = subprocess.run([str(BINARY), *args, '--json'], env=env,
                                    capture_output=True, text=True, timeout=20)
            assert result.returncode == code, (args, result.stdout, result.stderr)
            reply = json.loads(result.stdout)
            return reply['data'] if code == 0 else reply['error']

        def start():
            nonlocal process
            process = subprocess.Popen([str(BINARY), 'core'], env=env,
                                       stdout=subprocess.DEVNULL, stderr=subprocess.PIPE)
            deadline = time.monotonic() + 5
            while time.monotonic() < deadline:
                if process.poll() is not None:
                    raise AssertionError(process.stderr.read().decode())
                status = run('core', 'status')
                if status.get('pid') == process.pid:
                    return
                time.sleep(.025)
            raise AssertionError('test core did not start')

        try:
            start()
            run('save', str(ROOT / 'tests/fixtures/rituals/total-receipt.yaml'))
            run('job', 'create', str(ROOT / 'tests/fixtures/jobs/receipts.json'))
            with concurrent.futures.ThreadPoolExecutor(max_workers=2) as pool:
                futures = [pool.submit(run, 'job', 'work', 'receipt-totals',
                                       '--arg', 'count=2', '--arg', 'lease=1m') for _ in range(2)]
                workers = [f.result() for f in futures]
            assert sum(w['attempted'] for w in workers) == 4
            rows = [item for w in workers for item in w['items']]
            assert len({r['item'] for r in rows}) == 4
            assert all(r['committed'] and r['record']['outcome'] == 'returned' for r in rows)
            status = run('queue', 'status', 'receipt-totals')
            assert status['counts'] == {'done': 4}, status
            assert status['usage'] == {'attempts': 4, 'requests': 0, 'reported_tokens': 0}
            assert [r['record']['result']['total_minor'] for r in status['items']] == [125, 350, 799, 200]
            assert run('job', 'item', 'receipt-totals', 'receipt-001')['latest_attempts'][0]['summon']
            assert run('job', 'work', 'receipt-totals')['attempted'] == 0
            run('job', 'work', 'receipt-totals', '--arg', 'count=101', code=2)
            run('job', 'work', 'receipt-totals', '--arg', 'typo=1', code=2)
            run('job', 'retry', 'receipt-totals', 'receipt-001', code=5)
            run('job', 'create', str(ROOT / 'tests/fixtures/jobs/receipts.json'), code=5)
            run('core', 'stop')
            process.wait(timeout=5)
            start()
            assert run('job', 'status', 'receipt-totals')['counts'] == {'done': 4}
            assert run('job', 'work', 'receipt-totals')['attempted'] == 0
            print('PASS: two CLI workers, four unique results, pinned pure ritual, zero inference, input rejection, durable restart')
        finally:
            if process and process.poll() is None:
                try:
                    run('core', 'stop')
                    process.wait(timeout=5)
                finally:
                    if process.poll() is None:
                        process.kill()
                        process.wait()


if __name__ == '__main__':
    main()
