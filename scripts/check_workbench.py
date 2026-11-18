#!/usr/bin/env python3
"""Exercise the real terminal workbench through an isolated tmux and pure ritual."""
import http.server
import threading
import json
import os
from pathlib import Path
import subprocess
import tempfile
import time

ROOT = Path(__file__).resolve().parents[1]
BINARY = ROOT / 'target/debug/summonr'


def main():
    with tempfile.TemporaryDirectory(prefix='summonr-workbench-check-') as tmp:
        env = dict(os.environ, SUMMONR_HOME=tmp)
        socket = str(Path(tmp) / 'tmux.sock')

        def cli(*args):
            p = subprocess.run([str(BINARY), *args, '--json'], env=env, text=True, capture_output=True, timeout=10)
            assert p.returncode == 0, (p.stdout, p.stderr)
            return json.loads(p.stdout)['data']

        def tmux(*args, check=True):
            return subprocess.run(['tmux', '-S', socket, '-f', '/dev/null', *args], env=env,
                                  text=True, capture_output=True, timeout=10, check=check).stdout

        def send(text):
            tmux('send-keys', '-t', 'work', '-l', text)
            tmux('send-keys', '-t', 'work', 'Enter')

        def wait_for(*needles):
            deadline = time.monotonic() + 10
            screen = ''
            while time.monotonic() < deadline:
                screen = tmux('capture-pane', '-p', '-t', 'work')
                if all(n in screen for n in needles):
                    return screen
                time.sleep(.05)
            raise AssertionError((needles, screen))

        release = threading.Event()
        entered = threading.Event()
        class Handler(http.server.BaseHTTPRequestHandler):
            def log_message(self, *_):
                pass
            def do_GET(self):
                entered.set()
                release.wait(15)
                self.send_response(200)
                self.end_headers()
                self.wfile.write(b'ok')
        server = http.server.ThreadingHTTPServer(('127.0.0.1', 0), Handler)
        threading.Thread(target=server.serve_forever, daemon=True).start()
        try:
            cli('save', str(ROOT / 'tests/fixtures/rituals/total-receipt.yaml'))
            tmux('new-session', '-d', '-s', 'work', '-x', '140', '-y', '45',
                 str(BINARY), 'console')
            wait_for('THE WORKBENCH', 'CONNECTED')
            send('inspect total-receipt')
            wait_for('data.sum_integers', 'Static plan')
            send('run total-receipt --arg \'text={"amounts_minor":[100,25]}\' --wait')
            screen = wait_for('total_minor', '125', 'exit 0')
            assert 'schema_version' not in screen, screen
            run_id = cli('history')[0]['id']
            send(f'await {run_id} --timeout 20s')
            wait_for('125', 'exit 0')
            send('scry s-doesnotexist')
            wait_for('no summon `s-doesnotexist`')
            send('follow')
            wait_for('AT REST', 'requests  0 / 0')
            origin = f'http://127.0.0.1:{server.server_port}'
            definition = {'ritual':'waiting-read', 'reagents':{'http':{'select':['get_text']}},
                          'requires':[{'reagent':'http','action':'get_text','on':{'origin':origin}}],
                          'essence':{'requests':0,'tool_calls':1,'time':'20s'},
                          'steps':[{'id':'read','operation':'http.get_text','arguments':{'url':origin+'/','timeout_ms':15000}}],
                          'return':{'text':'${steps.read.output.text}'}}
            path = Path(tmp) / 'waiting.yaml'
            path.write_text(json.dumps(definition))
            cli('save', str(path))
            cli('allow', 'waiting-read', '--from', 'http', '--select', 'get_text', '--on', 'origin='+origin)
            waiting = cli('run', 'waiting-read')['summon']
            assert entered.wait(5)
            wait_for('waiting-read', 'FOLLOWING')
            for _ in range(8):
                send(f'await {waiting} --timeout 12s')
            wait_for('8 commands pending')
            send(f'stop {waiting}')
            wait_for('STOPPING', 'Banishing')
            assert cli('record', waiting)['cancellation_requested'] is True
            release.set()
            wait_for('ready', 'cancelled')
            send('core stop')
            wait_for('DISCONNECTED')
            assert cli('core', 'status') == {'running': False}
            send('rituals')
            wait_for('CONNECTED')
            send('exit')
            deadline = time.monotonic() + 5
            while time.monotonic() < deadline:
                result = subprocess.run(['tmux', '-S', socket, 'has-session', '-t', 'work'], capture_output=True)
                if result.returncode:
                    break
                time.sleep(.05)
            else:
                raise AssertionError('exit did not close workbench')
            assert 'pid' in cli('core', 'status'), 'closing workbench stopped the core'
            history = cli('history')
            assert len(history) == 2 and all(r['requests'] == 0 for r in history)
            print('PASS: real tmux workbench, readable output, wait timeout, invalid selection feedback, auto-follow, eight waits plus reserved stop, STOPPING, reconnect, daemon survives exit')
        finally:
            release.set()
            server.shutdown()
            server.server_close()
            tmux('kill-server', check=False)
            cli('core', 'stop')


if __name__ == '__main__':
    main()
