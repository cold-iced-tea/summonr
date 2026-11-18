#!/usr/bin/env python3
"""Mock-only integration, frozen benchmark integrity, and book link checks.
Never runs a real model or overwrites historical benchmark artifacts.
"""
import hashlib
import http.client
import socket
import time
import json
import os
import pathlib
import re
import runpy
import subprocess
import sys
import tempfile

sys.dont_write_bytecode = True
ROOT = pathlib.Path(__file__).resolve().parents[1]
BINARY = ROOT / 'target/debug/summonr'

def run(*args, env):
    result = subprocess.run([str(BINARY), *args, '--json'], env=env,
                            text=True, capture_output=True, timeout=15)
    if result.returncode:
        raise AssertionError((args, result.returncode, result.stdout, result.stderr))
    return json.loads(result.stdout)['data']

def mcp(messages, env):
    result = subprocess.run([str(BINARY), 'mcp'], env=env,
                            input='\n'.join(json.dumps(m) for m in messages) + '\n',
                            text=True, capture_output=True, timeout=20)
    assert result.returncode == 0, result.stderr
    # Tool calls run concurrently, so replies arrive in completion order;
    # JSON-RPC correlates them by id.
    replies = [json.loads(line) for line in result.stdout.splitlines()]
    return sorted(replies, key=lambda m: (m.get('id') is None, m.get('id') or 0))

def call(i, name, arguments):
    return {'jsonrpc': '2.0', 'id': i, 'method': 'tools/call',
            'params': {'name': name, 'arguments': arguments}}

def main():
    with tempfile.TemporaryDirectory(prefix='summonr-contract-check-') as tmp:
        home = pathlib.Path(tmp)
        env = dict(os.environ, SUMMONR_HOME=str(home))
        definition = {
            'ritual': 'classify', 'model': 'mock/keyword',
            'inputs': {'text': {'type': 'string'}, 'hold': {'type': 'boolean', 'default': True, 'user_only': True}},
            'steps': [{'id': 'answer', 'model': {'prompt': '${inputs.text}',
                       'schema': {'type': 'object', 'required': ['intent'],
                                  'properties': {'intent': {'type': 'string', 'enum': ['receipt', 'oos']}}}}}],
            'output_schema': {'type': 'object', 'required': ['intent'], 'additionalProperties': False,
                              'properties': {'intent': {'type': 'string'}}},
            'return': {'intent': '${steps.answer.output.intent}'},
        }
        file = home / 'classify.yaml'
        file.write_text(json.dumps(definition))
        compiled = run('compile', str(file), env=env)
        assert compiled['valid'] and not (home / 'state').exists(), 'offline compiler started core'
        try:
            pure = run('run', str(ROOT / 'tests/fixtures/rituals/total-receipt.yaml'),
                       '--arg', 'text={"amounts_minor":[100,25]}', '--wait', env=env)
            assert pure['result'] == {'total_minor': 125} and pure['requests'] == 0, pure
            run('save', str(ROOT / 'tests/fixtures/rituals/total-receipt.yaml'), env=env)
            contract = mcp([call(0, 'describe_ritual', {'name': 'total-receipt'})], env)[0]['result']['structuredContent']
            assert contract['execution_class'] == 'pure_deterministic' and contract['model_sites'] == [], contract
            run('save', str(file), env=env)
            args = {'name': 'classify', 'inputs': {'text': 'receipt'},
                    'expected_hash': compiled['hash'], 'idempotency_key': 'job-1', 'limits': {'requests': 1}}
            answers = mcp([call(1, 'describe_ritual', {'name': 'classify'}),
                           call(2, 'run_ritual', args), call(3, 'run_ritual', args),
                           call(4, 'run_ritual', {'name': 'classify', 'inputs': {'text': 'receipt', 'hold': False}})], env)
            first, repeated = (answers[i]['result']['structuredContent'] for i in (1, 2))
            assert first['result'] == {'intent': 'receipt'}, first
            assert first['run_id'] == repeated['run_id']
            assert answers[3]['result']['isError'] is True
            assert answers[0]['result']['structuredContent']['output_schema'] == definition['output_schema']
        finally:
            subprocess.run([str(BINARY), 'core', 'stop', '--json'], env=env, capture_output=True, timeout=15)

    # Real local transport, fake/no task inference. No external endpoint is contacted.
    with tempfile.TemporaryDirectory(prefix='summonr-http-check-') as tmp:
        with socket.socket() as reservation:
            reservation.bind(('127.0.0.1', 0)); port = reservation.getsockname()[1]
        env = dict(os.environ, SUMMONR_HOME=tmp)
        server = subprocess.Popen([str(BINARY), 'mcp', 'http', f'127.0.0.1:{port}'], env=env,
                                  stdout=subprocess.DEVNULL, stderr=subprocess.PIPE)
        try:
            token_file = pathlib.Path(tmp) / 'config/mcp_token'
            deadline = time.monotonic() + 5
            while time.monotonic() < deadline:
                try:
                    with socket.create_connection(('127.0.0.1', port), timeout=.2):
                        break
                except OSError:
                    assert server.poll() is None, 'HTTP server failed to start'
                    time.sleep(.02)
            token = token_file.read_text()
            assert token_file.stat().st_mode & 0o077 == 0
            path = '/mcp/' + token
            body = json.dumps({'jsonrpc': '2.0', 'id': 1, 'method': 'ping'})
            for headers, expected in [({}, 200), ({'Host': 'evil.example'}, 400), ({'Origin': 'https://evil.example'}, 403)]:
                client = http.client.HTTPConnection('127.0.0.1', port, timeout=3)
                client.request('POST', path, body=body, headers=headers)
                response = client.getresponse(); assert response.status == expected, response.status
                response.read(); client.close()
            with socket.create_connection(('127.0.0.1', port), timeout=3) as client:
                client.sendall(f'POST {path} HTTP/1.1\r\nHost: 127.0.0.1:{port}\r\nContent-Length: 0\r\nContent-Length: 1\r\n\r\n'.encode())
                response = http.client.HTTPResponse(client)
                response.begin()
                assert response.status == 400, response.status
                response.read()
        finally:
            server.terminate(); server.wait(timeout=5)

    for source in (ROOT / 'book/src').rglob('*.md'):
        for href in re.findall(r'\]\(([^)]+)\)', source.read_text()):
            if '://' in href or href.startswith('#') or href.startswith('mailto:'):
                continue
            target = href.split('#')[0]
            assert (source.parent / target).exists(), (source, href)
    print('PASS: offline compiler, production MCP handoff, idempotency, user-only policy, HTTP boundaries, book links')

if __name__ == '__main__':
    main()
