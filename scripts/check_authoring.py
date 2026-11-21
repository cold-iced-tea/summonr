#!/usr/bin/env python3
"""Independent structural-schema checks and SDK/YAML runtime parity; no inference.
Requires sdk/python/requirements-dev.txt in the test interpreter.
"""
import json
import os
from pathlib import Path
import subprocess
import sys
import tempfile

from jsonschema import Draft202012Validator
from generate_definition_schema import build

ROOT=Path(__file__).resolve().parents[1]
BIN=ROOT/'target/debug/summonr'
PAIRS=[('manifest.py','examples/01-check-manifest/check-manifest.yaml'),
       ('receipt.py','examples/03-receipt-total/receipt-total.yaml'),
       ('review.py','examples/05-label-reviews/label-review.yaml'),
       ('review_batch.py','examples/05-label-reviews/label-reviews.yaml'),
       ('collection_pipeline.py','examples/06-standard-library/collections.yaml')]


def main():
    schema=json.loads((ROOT/'schemas/ritual-v1.schema.json').read_text())
    assert schema==json.loads((ROOT/'book/src/reference/ritual-v1.schema.json').read_text())
    assert schema==build(),'published schema differs from generator'
    Draft202012Validator.check_schema(schema)
    validator=Draft202012Validator(schema)
    with tempfile.TemporaryDirectory(prefix='summonr-authoring-') as tmp:
        tmp=Path(tmp)
        env=dict(os.environ,SUMMONR_HOME=str(tmp/'home'),PYTHONPATH=str(ROOT/'sdk/python'),PYTHONDONTWRITEBYTECODE='1')
        def cli(*args,ok=True,cwd=ROOT):
            p=subprocess.run([str(BIN),*map(str,args),'--json'],env=env,cwd=cwd,text=True,capture_output=True,timeout=15)
            if ok:assert p.returncode==0,(args,p.stdout,p.stderr)
            else:assert p.returncode!=0,(args,p.stdout)
            return json.loads(p.stdout)
        try:
            for case in json.loads((ROOT/'schemas/fixtures/conformance.json').read_text()):
                assert validator.is_valid(case['definition'])==case['structural'],case['name']
                path=tmp/'case.json';path.write_text(json.dumps(case['definition']))
                cli('compile',path,ok=case['semantic'])
            artifacts={}
            for py,yml in PAIRS:
                builder=ROOT/'sdk/python/examples'/py
                p=subprocess.run([sys.executable,'-B',str(builder)],env=env,capture_output=True,text=True,check=True)
                definition=json.loads(p.stdout)
                validator.validate(definition)
                path=tmp/(builder.stem+'.json');path.write_text(p.stdout)
                left=cli('compile',path)['data'];right=cli('compile',ROOT/yml)['data']
                validator.validate(right['definition'])
                assert left==right,(py,'compiled definition/hash/effect plan differs')
                validator.validate(left['definition'])
                artifacts[py]=path
                if py=='review.py':cli('save',path) # pins the batch's existing child; no model calls
            # Resolve contracts and previews through the actual daemon for both forms.
            for py,yml in PAIRS:
                # Inscribe parity definitions: identical contents must not make a new version.
                cli('save',artifacts[py])
                same=cli('save',ROOT/yml)['data']
                assert same['unchanged'] is True and same['version']==1
                inputs = [] if py=='collection_pipeline.py' else ['--arg', 'reviews=[]'] if py=='review_batch.py' else ['--arg', 'text=sample']
                a = cli('rehearse', artifacts[py], *inputs)['data']
                b = cli('rehearse', ROOT/yml, *inputs)['data']
                assert a == b, (py, 'preview/authority/model/budget mismatch')
            # Bare .json paths must work from their directory, not be mistaken for saved names.
            result=cli('run','collection_pipeline.json','--wait',cwd=tmp)['data']
            assert result['result']=={'positive':[2,6],'total':8} and result['requests']==0
            value=(ROOT/'examples/01-check-manifest/good.json').read_text()
            result=cli('run',artifacts['manifest.py'],'--arg','text='+value,'--wait')['data']
            assert result['requests']==0 and result['outcome']=='returned'
            # A .json extension cannot silently accept YAML.
            bad=tmp/'not-json.json';bad.write_text('ritual: nope\nsteps: []\n')
            cli('compile',bad,ok=False)
            # Preview/save/run/compile never execute a Python builder.
            marker=tmp/'executed';builder=tmp/'untrusted.py'
            builder.write_text(f'from pathlib import Path\nPath({str(marker)!r}).touch()\n')
            for verb in ['compile','save','rehearse','run']:
                response=cli(verb,builder,ok=False)
                assert 'Python builders' in response['error']['message']
                assert not marker.exists()
            print('PASS: independent Draft 2020-12 validation, structural/semantic fixtures, five SDK/YAML definition/hash/effect-plan pairs, child pins, JSON execution, zero-inference results, Python never executed by Summonr')
        finally:
            cli('core','stop')


if __name__=='__main__':main()
