"""Parity with the language: every data primitive, keep:, string helpers in
expressions, and roots taken from inputs. Emitted rituals are compiled by the
real `summonr` binary when one is built (SUMMONR_BIN, or the repo's
target/debug/summonr), so these test what Summonr accepts, not just shapes."""
import json
import os
import pathlib
import subprocess
import tempfile
import unittest

from summonr import (Ritual, Str, Int, Number, List, Bool, contains, ends_with, lower,
                     starts_with, trim, upper)

REPO = pathlib.Path(__file__).resolve().parents[3]
BIN = os.environ.get('SUMMONR_BIN') or str(REPO / 'target' / 'debug' / 'summonr')


def compile_ritual(r):
    """Compile with the real binary; return its envelope."""
    with tempfile.TemporaryDirectory() as home:
        path = pathlib.Path(home) / (r.definition()['ritual'] + '.json')
        path.write_text(json.dumps(r.definition()))
        env = {**os.environ, 'SUMMONR_HOME': home, 'OLLAMA_HOST': 'http://127.0.0.1:9'}
        out = subprocess.run([BIN, 'compile', str(path), '--json'], capture_output=True, text=True, env=env, timeout=60)
        return json.loads(out.stdout)


class ParityTests(unittest.TestCase):
    def assertCompiles(self, r):
        if not os.path.exists(BIN):
            self.skipTest('no summonr binary built')
        envelope = compile_ritual(r)
        self.assertTrue(envelope['ok'], json.dumps(envelope.get('error') or envelope, indent=1)[:2000])

    def test_recent_data_primitives_take_positional_arguments(self):
        r = Ritual('prims', essence={'requests': 0})
        names = r.input('names', List[Str])
        want = r.input('want', Str)
        claims = r.input('claims', List)
        at = r.data.index_of(names, want, id='at')
        start = r.data.coalesce([at, 0], id='start')
        nums = r.data.range(start, 10, id='nums')
        uniq = r.data.unique_by(claims, ['file', 'line'], id='uniq')
        flat = r.data.flatten([names, names], id='flat')
        biggest = r.data.max(nums, id='biggest')
        lines = r.data.lines(want, number_from=1, id='lines')
        pick = r.data.select(True, want, 'none', id='pick')
        r.returns({'start': start, 'uniq': uniq, 'flat': flat, 'biggest': biggest, 'lines': lines, 'pick': pick})
        steps = {s['id']: s for s in r.definition()['steps']}
        self.assertEqual(steps['at']['arguments'], {'values': '${inputs.names}', 'needle': '${inputs.want}'})
        self.assertEqual(steps['start']['arguments'], {'values': ['${steps.at.output}', 0]})
        self.assertEqual(steps['nums']['arguments'], {'start': '${steps.start.output}', 'end': 10})
        self.assertEqual(steps['uniq']['arguments']['fields'], ['file', 'line'])
        for op in ['index_of', 'coalesce', 'range', 'unique_by', 'flatten', 'max', 'lines', 'select']:
            self.assertIn(op, r.definition()['reagents']['data']['select'])
        self.assertCompiles(r)

    def test_every_data_primitive_is_known(self):
        r = Ritual('all', essence={'requests': 0})
        for name in ['arithmetic', 'compare', 'get', 'keys', 'concat', 'slice', 'sort', 'unique', 'trim', 'split',
                     'join', 'replace', 'all', 'any', 'logic', 'select', 'min', 'max', 'clamp', 'abs', 'merge', 'set',
                     'omit', 'entries', 'from_entries', 'sha256', 'canonical_json', 'contains', 'starts_with',
                     'ends_with', 'case', 'lines', 'coalesce', 'flatten', 'unique_by', 'range', 'index_of',
                     'parse_json', 'project', 'count', 'sum_integers', 'assert']:
            self.assertTrue(callable(getattr(r.data, name)), name)
        with self.assertRaises(AttributeError):
            r.data.nonesuch
        r.data.assert_(True, message='ok', id='check')
        self.assertEqual(r.definition()['steps'][-1]['operation'], 'data.assert')

    def test_each_keep_names_the_outputs_kept_per_item(self):
        r = Ritual('kept', essence={'requests': 0})
        files = r.input('files', List[Str])
        with r.each(files, id='per', keep=['size']) as f:
            r.data.trim(f, id='size')
        r.returns({'per': r.ref('per')})
        self.assertEqual(r.definition()['steps'][0]['keep'], ['size'])
        with r.each(files, id='none', keep=[]) as f:
            r.data.trim(f, id='t2')
        self.assertEqual(r.definition()['steps'][1]['keep'], [])
        with self.assertRaises(TypeError):
            with r.each(files, id='bad', keep='size'):
                pass
        self.assertCompiles(r)

    def test_number_inputs_are_decimals(self):
        r = Ritual('num', essence={'requests': 0})
        floor = r.input('floor', Number(min=0))
        r.data.compare(floor, 1, 'lt', id='low')
        r.returns({'low': r.ref('low')})
        self.assertEqual(r.definition()['inputs']['floor'], {'type': 'decimal', 'min': 0})
        self.assertCompiles(r)

    def test_string_helpers_build_typed_expressions(self):
        r = Ritual('strings', essence={'requests': 0})
        lines = r.input('lines', List[Str])
        name = r.input('name', Str)
        code = r.filter(lines, lambda item: (trim(item) != '') & ~starts_with(trim(item), '//'), id='code')
        noisy = r.filter(lines, lambda item: contains(lower(item), r.param('word')), params={'word': name}, id='noisy')
        with r.when(ends_with(name, '.rs') | (upper(name) == 'README')):
            r.data.trim(name, id='t')
        r.returns({'code': code, 'noisy': noisy})
        d = r.definition()
        self.assertEqual(d['steps'][0]['arguments']['predicate'],
                         '(trim(item) != "") and (not (starts_with(trim(item), "//")))')
        self.assertEqual(d['steps'][1]['arguments']['predicate'], 'contains(lower(item), params.word)')
        self.assertEqual(d['steps'][2]['if'], '(ends_with(inputs.name, ".rs")) or (upper(inputs.name) == "README")')
        # Helpers are expressions, never Python values, and take strings or references.
        with self.assertRaises(TypeError):
            bool(lower(name))
        with self.assertRaises(TypeError):
            lower(3)
        with self.assertRaises(ValueError):
            contains(name, 'a\\b')
        with self.assertRaises(TypeError):
            r.data.trim(lower(name))
        self.assertCompiles(r)

    def test_roots_can_come_from_inputs(self):
        r = Ritual('scan', essence={'requests': 0, 'tool_calls': 2})
        target = r.input('target', Str)
        r.require('file', 'read_text', on={'root': target})
        readme = r.op('file.read_text', root=target, path='README.md', id='readme')
        r.returns({'text': readme['text']})
        d = r.definition()
        self.assertEqual(d['requires'], [{'reagent': 'file', 'action': 'read_text', 'on': {'root': '${inputs.target}'}}])
        # Only inputs are known before the run, when requires are checked.
        with self.assertRaises(ValueError):
            r.require('file', 'read_text', on={'root': readme['root']})
        self.assertCompiles(r)

    def test_a_model_step_routes_within_a_fixed_set(self):
        r = Ritual('route', essence={'requests': 1, 'tool_calls': 1})
        long = r.input('long', Bool)
        pick = r.data.select(long, 'ollama/big', 'ollama/small', id='pick')
        answer = r.model(prompt='Review', one_of=['ollama/small', 'ollama/big'], choose=pick, id='review')
        r.returns({'answer': answer})
        step = r.definition()['steps'][1]
        self.assertEqual(step['model']['one_of'], ['ollama/small', 'ollama/big'])
        self.assertEqual(step['model']['choose'], '${steps.pick.output}')
        for bad in [dict(one_of=['ollama/a']), dict(choose=pick), dict(one_of='ollama/a', choose=pick),
                    dict(one_of=['ollama/a'], choose='ollama/a'), dict(one_of=['ollama/a'], choose=pick, name='ollama/a')]:
            with self.assertRaises(TypeError):
                r.model(prompt='x', **bad)
        self.assertCompiles(r)


if __name__ == '__main__':
    unittest.main()
