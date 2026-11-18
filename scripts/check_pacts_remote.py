#!/usr/bin/env python3
"""Two Summonr homes play a manufacturer (A, the caller) and a supplier (B, the
callee) over loopback `listen`, using examples/07-pacts. Covers offer, agree,
a small call accepted by checks, an over-threshold call the buyer must approve,
a revision, an undecided call that expires, a forged fork, and offline
verification of both exports. Prints one PASS line per check and exits nonzero
on any failure. Needs `openssl` with Ed25519 for the forged fork."""
import base64, hashlib, json, os, pathlib, socket, subprocess, tempfile, time

ROOT = pathlib.Path(__file__).resolve().parents[1]
BINARY = ROOT / 'target/debug/summonr'
EXAMPLES = ROOT / 'examples/07-pacts'


def env(home):
    return dict(os.environ, SUMMONR_HOME=str(home))


def call(home, *args, ok=True):
    p = subprocess.run([str(BINARY), *args, '--json'], env=env(home), capture_output=True, text=True, timeout=120)
    envelope = json.loads(p.stdout)
    if ok:
        assert envelope['ok'], (args, envelope)
        return envelope['data']
    return envelope, p.returncode


def check(label, condition, detail=''):
    if not condition:
        raise AssertionError(f'{label}: {detail}')
    print(f'PASS: {label}')


def free_port():
    with socket.socket() as s:
        s.bind(('127.0.0.1', 0))
        return s.getsockname()[1]


def wait_port(port):
    for _ in range(100):
        try:
            socket.create_connection(('127.0.0.1', port), timeout=0.1).close()
            return
        except OSError:
            time.sleep(0.05)
    raise AssertionError(f'nothing listening on {port}')


def canonical(value):
    return json.dumps(value, sort_keys=True, separators=(',', ':'), ensure_ascii=False)


def ed25519_sign(key_file, message, tmp):
    msg = pathlib.Path(tmp) / 'msg.bin'
    msg.write_bytes(message)
    out = subprocess.run(['openssl', 'pkeyutl', '-sign', '-inkey', str(key_file), '-rawin', '-in', str(msg)],
                         capture_output=True, check=True)
    return base64.b64encode(out.stdout).decode()


def terms(a_key, b_key, ritual_hash, check_hash, extra=''):
    text = (EXAMPLES / 'order-parts.yaml').read_text()
    source = (EXAMPLES / 'check-quote.yaml').read_text()
    indented = ''.join(f'      {line}\n' for line in source.splitlines())
    text = (text.replace('ed25519:MANUFACTURER_CORE_KEY', a_key).replace('ed25519:SUPPLIER_CORE_KEY', b_key)
                .replace('COMMIT_ORDER_HASH', ritual_hash).replace('CHECK_QUOTE_HASH', check_hash)
                .replace('      CHECK_QUOTE_SOURCE\n', indented))
    return text + extra


def committed(mail):
    return len(list((mail / 'Committed').iterdir()))


def main():
    with tempfile.TemporaryDirectory(prefix='summonr-pacts-') as tmp:
        tmp = pathlib.Path(tmp)
        a, b = tmp / 'a', tmp / 'b'
        mail = tmp / 'mail'
        for folder in ('Orders', 'Committed'):
            (mail / folder).mkdir(parents=True)
        for n in range(1, 5):
            (mail / 'Orders' / f'00{n}.eml').write_text(f'From: buyer@example.com\nSubject: order {n}\n\nOrder {n}')
        for home in (a, b):
            (home / 'config').mkdir(parents=True)
        (b / 'config/config.yaml').write_text(f'mail_root: "{mail}"\n')
        port = free_port()
        listener = None
        try:
            a_key, b_key = call(a, 'identity')['key'], call(b, 'identity')['key']
            listener = subprocess.Popen([str(BINARY), 'listen', f'127.0.0.1:{port}'], env=env(b),
                                        stdout=subprocess.PIPE, stderr=subprocess.PIPE, text=True)
            wait_port(port)
            call(a, 'peer', 'add', 'supplier', '--url', f'http://127.0.0.1:{port}', '--key', b_key)
            call(b, 'peer', 'add', 'maker', '--url', 'http://127.0.0.1:9', '--key', a_key)
            call(b, 'contract', 'admit', 'maker', '--key', a_key)
            call(b, 'save', str(EXAMPLES / 'commit-order.yaml'))
            call(b, 'allow', 'commit-order', '--from', 'mail', '--all', '--on', 'folder=Orders')
            call(b, 'allow', 'commit-order', '--from', 'mail', '--select', 'locate')
            ritual_hash = call(b, 'compile', str(EXAMPLES / 'commit-order.yaml'))['hash']
            check_hash = call(a, 'compile', str(EXAMPLES / 'check-quote.yaml'))['hash']

            file = tmp / 'order-parts.yaml'
            file.write_text(terms(a_key, b_key, ritual_hash, check_hash))
            offered = call(a, 'pact', 'offer', 'supplier', str(file))
            check('the manufacturer offers the pact', offered['state'] == 'offered', offered)
            shown = call(b, 'pact', 'show', 'order-parts')
            check('the supplier sees the pinned ritual and the buyer\'s acceptance ritual in full',
                  shown['ritual']['ritual'] == 'commit-order' and 'delivery' in shown['acceptance']['source'], shown)
            call(b, 'pact', 'agree', 'order-parts')
            call(a, 'pact', 'poll')
            check('both sides hold an agreed pact', call(a, 'pact', 'show', 'order-parts')['state'] == 'agreed')

            small = call(a, 'pact', 'call', 'order-parts', '--arg', 'amount=10', '--wait')
            check('a small call is delivered and accepted by the checks', small['state'] == 'accepted', small)
            check('the supplier committed the order', committed(mail) == 1)

            big = call(a, 'pact', 'call', 'order-parts', '--arg', 'amount=5000')
            time.sleep(1)
            call(a, 'pact', 'poll')
            held = [p for p in call(a, 'petition') if p['operation'] == 'pact.decide']
            check('an over-threshold order waits for the buyer', len(held) == 1 and committed(mail) == 1, held)
            call(a, 'petition', 'permit', held[0]['id'])
            check('the supplier commits it after the buyer permits', committed(mail) == 2)
            done = call(a, 'pact', 'status', 'order-parts', str(big['call']), '--wait')
            check('a completed run and an accepted deliverable stay separate: the checks reject 5000',
                  done['state'] == 'rejected' and any(e['kind'] == 'settle' for e in done['entries']), done)

            old = call(a, 'pact', 'show', 'order-parts')['id']
            revised = tmp / 'order-parts-2.yaml'
            revised.write_text(terms(a_key, b_key, ritual_hash, check_hash, f'revises: {old}\n').replace('deadline: 48h', 'deadline: 3s'))
            rev = call(a, 'pact', 'offer', 'supplier', str(revised), '--name', 'order-parts-2')
            call(b, 'pact', 'agree', rev['id'])
            call(a, 'pact', 'poll')
            check('the revision supersedes the old pact on both sides',
                  call(a, 'pact', 'show', old)['state'] == 'superseded' and call(b, 'pact', 'show', old)['state'] == 'superseded')
            env_, code = call(a, 'pact', 'call', old, '--arg', 'amount=1', ok=False)
            check('the old pact takes no new calls', not env_['ok'])

            late = call(a, 'pact', 'call', 'order-parts-2', '--arg', 'amount=7000')
            time.sleep(4)
            call(a, 'pact', 'poll')
            call(a, 'pact', 'poll')
            status = call(a, 'pact', 'status', 'order-parts-2', str(late['call']))
            settles = [e for e in status['entries'] if e['kind'] == 'settle']
            check('an undecided hold expires at the deadline and nothing is done',
                  settles and settles[0]['data']['state'] == 'expired' and committed(mail) == 2, status)

            a_export, b_export = tmp / 'a.json', tmp / 'b.json'
            call(a, 'pact', 'export', 'order-parts', str(a_export))
            call(b, 'pact', 'export', 'order-parts', str(b_export))
            ea, eb = json.loads(a_export.read_text()), json.loads(b_export.read_text())
            check('both sides hold the same thread', ea['entries'] == eb['entries'])
            for who, home, f in (('the supplier', b, a_export), ('the manufacturer', a, b_export)):
                verdict = call(home, 'verify', str(f))
                check(f'{who} verifies the other side\'s export offline', verdict['ok'] and verdict['epitaphs_checked'] >= 2, verdict)

            # A fork: the manufacturer signs a second, different entry on the
            # same previous entry as one it already signed.
            mine = [e for e in ea['entries'] if json.loads(e['bytes'])['by'] == hashlib.sha256(base64.urlsafe_b64decode(a_key[8:] + '==')).hexdigest()]
            first = json.loads(mine[1]['bytes'])
            twin = dict(first, at=first['at'] + 1, data={'inputs': {'amount': 999999}})
            twin_bytes = canonical(twin)
            signature = ed25519_sign(a / 'config/core.key', b'summonr-pact-v1\x00' + twin_bytes.encode(), tmp)
            ea['entries'].append({'bytes': twin_bytes, 'sig': signature, 'hash': hashlib.sha256(twin_bytes.encode()).hexdigest()})
            forged = tmp / 'forged.json'
            forged.write_text(json.dumps(ea))
            verdict, code = call(b, 'verify', str(forged), ok=False)
            check('a forged fork is signed evidence of equivocation (exit 15)',
                  code == 15 and len(verdict['data']['forks']) == 1, (code, verdict))
        finally:
            if listener:
                listener.terminate()
                listener.wait(timeout=10)
            for home in (a, b):
                subprocess.run([str(BINARY), 'core', 'stop', '--json'], env=env(home), capture_output=True, timeout=30)


if __name__ == '__main__':
    main()
