#!/usr/bin/env python3
"""Exercise the real terminal workbench through an isolated tmux and pure ritual."""
import argparse
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
    parser = argparse.ArgumentParser()
    parser.add_argument("--capture", type=Path, help="save the 80x24 petition frame with terminal colours")
    capture = parser.parse_args().capture
    with tempfile.TemporaryDirectory(prefix='summonr-workbench-check-') as tmp:
        env = dict(os.environ, SUMMONR_HOME=tmp)
        env.pop('NO_COLOR', None)
        env.pop('SUMMONR_PLAIN', None)
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
            tmux('new-session', '-d', '-s', 'work', '-x', '80', '-y', '24',
                 str(BINARY), 'console')
            wait_for('S U M M O N R', 'At rest', 'Command')
            cli('save', str(ROOT / 'examples/10-bughunt/batch-ready.yaml'))
            send("""run bughunt-batch-ready --arg 'output={"pass":true}' --wait""")
            wait_for('bughunt-batch-ready', 'returned', 'data.get')
            tmux('send-keys', '-t', 'work', '-l', 'inspect bug')
            wait_for('> inspect bug')
            draft_cursor = tmux('display-message', '-p', '-t', 'work', '#{cursor_x},#{cursor_y}')
            # File a real held write in the isolated home.
            dest = Path(tmp) / 'out'
            dest.mkdir()
            definition = {'ritual': 'publish-digest', 'inputs': {'hold': {'type': 'boolean', 'default': True, 'user_only': True}},
                          'requires': [{'reagent':'file', 'action':'write_text', 'on':{'root':str(dest)}}],
                          'steps': [{'id':'title','operation':'data.trim','arguments':{'text':' Workbench check '}},
                                    {'id':'publish','operation':'file.write_text','hold':'${inputs.hold}',
                                     'arguments':{'root':str(dest),'path':'digest.md','text':'${steps.title.output}'}}],
                          'return':{'petition':'${steps.publish.output.petition}'}}
            path = Path(tmp) / 'publish.yaml'
            path.write_text(json.dumps(definition))
            cli('save', str(path))
            cli('allow', 'publish-digest', '--from', 'file', '--select','write_text','--on','root='+str(dest))
            cli('run','publish-digest','--wait')
            screen = wait_for('bughunt-batch-ready','publish-digest','Waiting for you','digest.md','inspect bug')
            assert len(screen.splitlines()) == 24
            assert draft_cursor == tmux('display-message', '-p', '-t', 'work', '#{cursor_x},#{cursor_y}')
            print(screen)
            tmux('send-keys', '-t', 'work', 'F6')
            wait_for('Petition','petition list','digest.md','inspect bug')
            petition = cli('petition')[0]['id']
            cli('petition','deny',petition)
            wait_for('At rest','publish-digest','inspect bug')
            assert not (dest / 'digest.md').exists()
            tmux('send-keys', '-t', 'work', 'C-c')
            tmux('resize-window', '-t', 'work', '-x','140','-y','45')
            tmux('send-keys', '-t', 'work', 'F5')
            wait_for('CONNECTED / live')
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
            send('clear')
            send("""run bughunt-batch-ready --arg 'output={"pass":true}' --wait""")
            wait_for('bughunt-batch-ready', 'ELEGY AT REST', 'exit 0')
            origin = f'http://127.0.0.1:{server.server_port}'
            definition = {'ritual':'waiting-read', 'reagents':{'http':{'select':['get_text']}},
                          'requires':[{'reagent':'http','action':'get_text','on':{'origin':origin}}],
                          'essence':{'requests':0,'tool_calls':3,'time':'20s'},
                          'steps':[{'id':'source','operation':'data.trim','arguments':{'text':origin+'/'}},
                                   {'id':'read','operation':'http.get_text','arguments':{'url':'${steps.source.output}','timeout_ms':15000}}],
                          'return':{'text':'${steps.read.output.text}'}}
            path = Path(tmp) / 'waiting.yaml'
            path.write_text(json.dumps(definition))
            cli('save', str(path))
            cli('allow', 'waiting-read', '--from', 'http', '--select', 'get_text', '--on', 'origin='+origin)
            waiting = cli('run', 'waiting-read')['summon']
            assert entered.wait(5)
            wait_for('waiting-read', 'FOLLOWING')
            tmux('resize-window', '-t', 'work', '-x', '80', '-y', '24')
            tmux('send-keys', '-t', 'work', 'F5')
            wait_for('Working', 'waiting-read')
            cli('run', 'publish-digest', '--wait')
            tmux('send-keys', '-t', 'work', '-l', 'inspect bughunt-')
            wait_for('waiting-read', '1 active   1 held', 'digest.md', '> inspect bughunt-')
            if capture:
                capture.write_text(tmux('capture-pane', '-e', '-p', '-t', 'work'))
            cli('petition', 'deny', cli('petition')[0]['id'])
            wait_for('Working', '0 held', '> inspect bughunt-')
            tmux('send-keys', '-t', 'work', 'C-c', 'F5')
            tmux('resize-window', '-t', 'work', '-x', '140', '-y', '45')
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
            assert cli('core', 'status')['running'] is False
            send('rituals')
            wait_for('CONNECTED / live')
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
            assert len(history) == 6 and all(r['requests'] == 0 for r in history)
            print('PASS: real tmux workbench, readable output, wait timeout, invalid selection feedback, auto-follow, eight waits plus reserved stop, STOPPING, reconnect, daemon survives exit')
        finally:
            release.set()
            server.shutdown()
            server.server_close()
            tmux('kill-server', check=False)
            cli('core', 'stop')


if __name__ == '__main__':
    main()
