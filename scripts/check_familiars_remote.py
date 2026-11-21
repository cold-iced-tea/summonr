#!/usr/bin/env python3
"""Two Summonr homes on one machine play two devices: A listens for signed
requests, B's agent calls A through `summonr mcp --name partner --via a`.
Prints one PASS line per check and exits nonzero on any failure."""
import json, os, pathlib, socket, subprocess, tempfile, time

ROOT = pathlib.Path(__file__).resolve().parents[1]
BINARY = ROOT / 'target/debug/summonr'


def run(home, *args):
    p = subprocess.run([str(BINARY), *args, '--json'], env=dict(os.environ, SUMMONR_HOME=str(home)),
                       capture_output=True, text=True, timeout=30)
    envelope = json.loads(p.stdout)
    assert envelope['ok'], (args, envelope)
    return envelope['data']


def mcp(home, name, peer, calls):
    messages = [{'jsonrpc': '2.0', 'id': 0, 'method': 'initialize', 'params': {'protocolVersion': '2025-03-26'}}]
    for i, (tool, arguments) in enumerate(calls, start=1):
        messages.append({'jsonrpc': '2.0', 'id': i, 'method': 'tools/call',
                         'params': {'name': tool, 'arguments': arguments}})
    p = subprocess.run([str(BINARY), 'mcp', '--name', name, '--via', peer],
                       env=dict(os.environ, SUMMONR_HOME=str(home)),
                       input='\n'.join(json.dumps(m) for m in messages) + '\n',
                       text=True, capture_output=True, timeout=60)
    assert p.returncode == 0, p.stderr
    replies = sorted((json.loads(l) for l in p.stdout.splitlines()), key=lambda m: m['id'])
    return [r['result'] for r in replies[1:]]


def free_port():
    with socket.socket() as s:
        s.bind(('127.0.0.1', 0))
        return s.getsockname()[1]


def check(label, condition, detail=''):
    if not condition:
        raise AssertionError(f'{label}: {detail}')
    print(f'PASS: {label}')


def main():
    with tempfile.TemporaryDirectory(prefix='summonr-remote-') as tmp:
        a, b = pathlib.Path(tmp) / 'a', pathlib.Path(tmp) / 'b'
        port = free_port()
        listener = None
        try:
            # Device A: the core being called.
            run(a, 'inscribe', str(ROOT / 'tests/fixtures/rituals/total-receipt.yaml'))
            a_identity = run(a, 'identity')
            listener = subprocess.Popen([str(BINARY), 'listen', f'127.0.0.1:{port}'],
                                        env=dict(os.environ, SUMMONR_HOME=str(a)),
                                        stdout=subprocess.PIPE, stderr=subprocess.PIPE, text=True)
            for _ in range(100):
                try:
                    socket.create_connection(('127.0.0.1', port), timeout=0.1).close()
                    break
                except OSError:
                    time.sleep(0.05)

            # Device B makes the agent's key; A registers its public half.
            partner = run(b, 'familiar', 'new', 'partner')
            run(a, 'familiar', 'add', 'partner', '--key', partner['key'])
            run(a, 'familiar', 'allow', 'partner', 'total-receipt')
            # B pins A's core by hand.
            run(b, 'peer', 'add', 'a', '--url', f'http://127.0.0.1:{port}', '--key', a_identity['key'])

            listed, ran = mcp(b, 'partner', 'a', [
                ('list_rituals', {}),
                ('run_ritual', {'name': 'total-receipt', 'inputs': {'text': '{"amounts_minor":[100,25]}'}, 'wait_seconds': 20}),
            ])
            names = [r['name'] for r in listed['structuredContent']['items']]
            check('partner sees exactly the allowed ritual', names == ['total-receipt'], names)
            result = ran['structuredContent']
            check('partner runs it across the network', result.get('result', {}).get('total_minor') == 125, result)
            graveyard = run(a, 'graveyard')
            check('device A records who asked', graveyard[0].get('familiar') == 'familiar:partner', graveyard[0])

            # A key device A never registered.
            run(b, 'familiar', 'new', 'stranger')
            (refused,) = mcp(b, 'stranger', 'a', [('list_rituals', {})])
            check('an unregistered key is refused', refused['isError'] and refused['structuredContent'].get('error') == 'not_authenticated', refused)

            # B pinned the wrong key for A: A's replies must be rejected.
            run(b, 'peer', 'add', 'fake', '--url', f'http://127.0.0.1:{port}', '--key', run(b, 'identity')['key'])
            (forged,) = mcp(b, 'partner', 'fake', [('list_rituals', {})])
            check('replies from an unpinned core are rejected', forged['isError'] and forged['structuredContent'].get('error') == 'not_authenticated', forged)

            # Revocation on A takes effect on the next request.
            run(a, 'familiar', 'revoke', 'partner')
            (after,) = mcp(b, 'partner', 'a', [('list_rituals', {})])
            check('a revoked familiar is refused', after['isError'] and after['structuredContent'].get('error') == 'not_authenticated', after)

            log = (a / 'state' / 'core.log').read_text()
            check('device A logged why it refused', 'unknown or revoked key' in log, log[-500:])
        finally:
            if listener:
                listener.terminate()
                listener.wait(timeout=10)
            for home in (a, b):
                subprocess.run([str(BINARY), 'core', 'stop', '--json'],
                               env=dict(os.environ, SUMMONR_HOME=str(home)), capture_output=True, timeout=30)


if __name__ == '__main__':
    main()
