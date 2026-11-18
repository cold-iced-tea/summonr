#!/usr/bin/env python3
"""Generate the public structural schema; Rust remains the semantic authority.
Changes require check_authoring.py conformance fixtures and runtime review.
"""
import json
from pathlib import Path


def obj(properties, required=()):
    return dict(type='object', properties=properties, required=list(required), additionalProperties=False)


def arr(items):
    return dict(type='array', items=items)


def nullable(schema):
    return {'anyOf': [schema, {'type': 'null'}]}


def ref(name):
    return {'$ref': '#/$defs/' + name}


def mapping(value):
    return dict(type='object', additionalProperties=value)


def build():
    string = {'type': 'string'}
    boolean = {'type': 'boolean'}
    number = {'type': 'number'}
    u64 = dict(type='integer', minimum=0, maximum=18446744073709551615)
    u32 = dict(type='integer', minimum=0, maximum=4294967295)
    definitions = {}
    definitions['input'] = obj({'type': {'enum': ['string', 'integer', 'decimal', 'boolean', 'list']},
                                'default': {}, 'min': nullable(number), 'max': nullable(number),
                                'enum': nullable(arr({})), 'user_only': boolean}, ['type'])
    definitions['binding'] = obj({k: nullable(string) for k in ['document', 'content', 'hash']})
    definitions['reagent'] = obj({'select': arr(string), 'version': nullable(u32)})
    definitions['requirement'] = obj({'reagent': string, 'action': string, 'on': mapping(string), 'into': mapping({})}, ['reagent', 'action'])
    definitions['essence'] = obj({**{k: nullable(u64) for k in ['requests', 'tokens', 'tool_calls', 'items']}, 'time': nullable(string)})
    definitions['drift'] = obj({'on_mismatch': nullable({'enum': ['note', 'warn', 'refuse']})})
    definitions['valueSchema'] = obj({'type': {'enum': ['object', 'array', 'string', 'boolean', 'integer', 'number', 'null']},
        'enum': {**arr({}), 'minItems': 1}, 'properties': mapping(ref('valueSchema')), 'items': ref('valueSchema'),
        'required': {**arr(string), 'uniqueItems': True}, 'additionalProperties': boolean,
        'minimum': number, 'maximum': number, **{k:u64 for k in ['minLength','maxLength','minItems','maxItems']},
        'title': string, 'description': string, 'default': {}})
    definitions['model'] = obj({'name': nullable(string), 'settings': mapping({}), 'instructions': nullable(string),
        'bind': arr(string), 'prompt': string, 'schema': nullable(ref('valueSchema')), 'abstain_if': nullable(string)}, ['prompt'])
    definitions['verify'] = obj({'observe': string, 'arguments': mapping({}), 'expect': mapping({})}, ['observe', 'expect'])
    properties = {'id': nullable(string), 'operation': nullable(string), 'arguments': nullable(mapping({})),
        'verify': nullable(ref('verify')), 'hold': {'type':['boolean','string','null']}, 'model': nullable(ref('model')), 'ritual': nullable(string),
        'version': nullable(u32), 'hash': nullable(string), 'each': nullable(string), 'steps': nullable(arr(ref('step'))),
        'on_failure': nullable({'enum':['stop','continue']}), 'parallel': nullable({'const':1}),
        'if': nullable(string), 'then': nullable(arr(ref('step'))), 'else': nullable(arr(ref('step')))}
    variants = []
    for kind, fields in [('operation', ['arguments','verify','hold']), ('model', []), ('ritual',['arguments','version','hash']),
                         ('each',['steps','on_failure','parallel']), ('if',['then','else'])]:
        allowed = {'id', kind, *fields}
        branch = {k: ({'type':'null'} if k not in allowed else v) for k,v in properties.items()}
        branch[kind] = ref('model') if kind == 'model' else string
        required = [kind]
        if kind != 'if':
            branch['id'] = string
            required.append('id')
        if kind == 'if':
            branch['then'] = arr(ref('step'))
            required.append('then')
        variants.append({'properties':branch, 'required':required})
    definitions['step'] = {**obj(properties), 'oneOf': variants}
    return {'$schema':'https://json-schema.org/draft/2020-12/schema',
        '$id':'urn:summonr:ritual:1',
        'title':'Summonr language version 1 definition',
        'description':'Structural authoring/interchange contract. Rust compilation remains mandatory for references, operations, scopes, bindings, budgets, and pins. This is not the restricted schema dialect used for model outputs.',
        **obj({'language_version': {'const':1, 'default':1}, 'ritual':dict(type='string',pattern='^[A-Za-z0-9_-]+$'),
            'description':string, 'inputs':mapping(ref('input')), 'bindings':mapping(ref('binding')),
            'model':nullable(string), 'settings':mapping({}), 'reagents':mapping(ref('reagent')),
            'requires':arr(ref('requirement')), 'essence':ref('essence'), 'drift':ref('drift'),
            'output_schema':nullable(ref('valueSchema')), 'steps':arr(ref('step')), 'return':mapping({})}, ['ritual','steps']),
        '$defs':definitions}


if __name__ == '__main__':
    path = Path(__file__).resolve().parents[1] / 'schemas/ritual-v1.schema.json'
    text = json.dumps(build(), indent=2) + '\n'
    path.write_text(text)
    (path.parents[1] / 'book/src/reference/ritual-v1.schema.json').write_text(text)
