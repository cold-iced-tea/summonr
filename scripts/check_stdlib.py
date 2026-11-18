#!/usr/bin/env python3
"""Standard-library integration: pure computations and isolated local I/O only."""
import http.server
import json
import os
from pathlib import Path
import subprocess
import tempfile
import threading
import time

ROOT = Path(__file__).resolve().parents[1]
BIN = ROOT / 'target/debug/summonr'


def main():
    hits = []

    class Handler(http.server.BaseHTTPRequestHandler):
        def log_message(self, *_):
            pass

        def do_GET(self):
            hits.append(self.path)
            if self.path == '/redirect':
                self.send_response(302)
                self.send_header('Location', '/forbidden-follow')
                self.end_headers()
                return
            if self.path == '/slow':
                time.sleep(.2)
            self.send_response(200)
            self.send_header('Content-Type', 'application/json')
            self.end_headers()
            try:
                self.wfile.write(b'{"answer":42}')
            except (BrokenPipeError, ConnectionResetError):
                pass

    server = http.server.ThreadingHTTPServer(('127.0.0.1', 0), Handler)
    threading.Thread(target=server.serve_forever, daemon=True).start()
    origin = f'http://127.0.0.1:{server.server_port}'
    with tempfile.TemporaryDirectory(prefix='summonr-stdlib-') as tmp:
        home = Path(tmp)
        env = dict(os.environ, SUMMONR_HOME=str(home / 'home'))

        def cli(*args, code=0):
            p = subprocess.run([str(BIN), *args, '--json'], env=env, text=True, capture_output=True, timeout=15)
            assert p.returncode == code, (args, p.returncode, p.stdout, p.stderr)
            return json.loads(p.stdout)

        def save(name, reagents, steps, ret, requires=None, essence=None):
            d = {'ritual': name, 'language_version': 1,
                 'reagents': {k: {'select': v} for k, v in reagents.items()},
                 'requires': requires or [], 'essence': essence or {'requests': 0, 'tool_calls': 20, 'items': 20, 'time': '5s'},
                 'steps': steps, 'return': ret}
            path = home / (name + '.yaml')
            path.write_text(json.dumps(d))
            cli('compile', str(path))
            cli('save', str(path))
            return path

        def run(name, code=0):
            return cli('run', name, '--wait', code=code)['data']

        try:
            steps = [
                {'id': 'mapped', 'operation': 'data.map', 'arguments': {'values': [1, 2, 3], 'operation': 'data.arithmetic', 'arguments': {'left': '${item}', 'right': 2, 'operator': 'multiply'}}},
                {'id': 'filtered', 'operation': 'data.filter', 'arguments': {'values': '${steps.mapped.output}', 'predicate': 'item > 2'}},
                {'id': 'folded', 'operation': 'data.fold', 'arguments': {'values': '${steps.filtered.output}', 'initial': 0, 'operation': 'data.arithmetic', 'arguments': {'left': '${acc}', 'right': '${item}', 'operator': 'add'}}},
                {'if': 'steps.folded.output >= 10', 'then': [{'id': 'ok', 'operation': 'data.assert', 'arguments': {'condition': True}}]},
            ]
            reagents = {'data': ['map', 'filter', 'fold', 'arithmetic', 'assert']}
            ret = {'sum': '${steps.folded.output}'}
            save('collections', reagents, steps, ret)
            record = run('collections')
            assert record['result'] == {'sum': 10} and record['requests'] == 0
            save('limited', reagents, steps, ret, essence={'requests': 0, 'items': 2, 'tool_calls': 20, 'time': '5s'})
            assert run('limited', code=10)['outcome'] == 'exhausted'
            save('tool-limited', reagents, steps, ret, essence={'requests': 0, 'items': 20, 'tool_calls': 2, 'time': '5s'})
            assert run('tool-limited', code=10)['outcome'] == 'exhausted'
            data = home / 'documents'
            data.mkdir()
            (data / 'data.json').write_text('{"amounts":[100,25]}')
            requires = [{'reagent': 'file', 'action': 'read_text', 'on': {'root': str(data)}}]
            save('read-data', {'file': ['read_text'], 'data': ['parse_json', 'sum_integers']}, [
                {'id': 'read', 'operation': 'file.read_text', 'arguments': {'root': str(data), 'path': 'data.json'}},
                {'id': 'parsed', 'operation': 'data.parse_json', 'arguments': {'text': '${steps.read.output.text}'}},
                {'id': 'total', 'operation': 'data.sum_integers', 'arguments': {'values': '${steps.parsed.output.amounts}'}},
            ], {'total': '${steps.total.output}', 'sha256': '${steps.read.output.sha256}'}, requires)
            cli('run', 'read-data', '--wait', code=4)
            cli('allow', 'read-data', '--from', 'file', '--select', 'read_text', code=2)
            cli('allow', 'read-data', '--from', 'file', '--select', 'read_text', '--on', f'root={data}')
            record = run('read-data')
            assert record['result']['total'] == 125 and record['requests'] == 0
            assert len(record['result']['sha256']) == 64
            for name, path, options, exit_code in [('fetch', '/ok', {}, 0), ('redirect', '/redirect', {}, 11), ('large', '/ok', {'max_bytes': 2}, 10), ('slow', '/slow', {'timeout_ms': 20}, 11)]:
                save(name, {'http': ['get_text']}, [{'id': 'get', 'operation': 'http.get_text', 'arguments': {'url': origin + path, **options}}], {'response': '${steps.get.output}'}, [{'reagent': 'http', 'action': 'get_text', 'on': {'origin': origin}}])
                before = len(hits)
                cli('run', name, '--wait', code=4)
                assert len(hits) == before
                cli('allow', name, '--from', 'http', '--select', 'get_text', '--on', f'origin={origin}')
                record = run(name, code=exit_code)
                assert record['requests'] == 0
                if not exit_code:
                    assert json.loads(record['result']['response']['text']) == {'answer': 42}
            assert '/forbidden-follow' not in hits
            print('PASS: compiled collection pipeline, item/tool budgets, scoped file read and hash, HTTP grant refusal, local GET, no redirects, size and timeout bounds; zero inference')
        finally:
            cli('core', 'stop')
            server.shutdown()
            server.server_close()


if __name__ == '__main__':
    main()
