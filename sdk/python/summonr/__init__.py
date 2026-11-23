"""Trusted authoring helpers that emit Summonr v1 definitions, never execute them.
Run builders explicitly with Python. Validate the emitted artifact with Summonr.
"""
from __future__ import annotations

import copy
import json
import re
import sys
from contextlib import contextmanager
from dataclasses import dataclass

__all__ = ['Ritual', 'Ref', 'Expr', 'Str', 'Int', 'Number', 'Bool', 'List', 'Object', 'emit']
_MISSING = object()


def _segment(name):
    if not isinstance(name, str) or not re.fullmatch(r'[A-Za-z0-9_-]+', name):
        raise ValueError('reference segments must contain letters, digits, underscores or hyphens; use data.get for other keys')
    return name


def _future_error(*_):
    raise TypeError('This is a future runtime value, not a Python value. Use when/each or an explicit data operation; do not use if, and/or, str, arithmetic, or Python iteration.')


class Expr:
    def __init__(self, source, owner):
        self.source, self.owner = source, owner

    __bool__ = _future_error
    __str__ = _future_error
    __iter__ = _future_error
    __int__ = _future_error
    __float__ = _future_error
    __len__ = _future_error
    __hash__ = None
    __eq__ = _future_error
    __ne__ = _future_error

    def _logic(self, other, operator):
        if not isinstance(other, Expr) or other.owner is not self.owner:
            raise TypeError('logical operands must be expressions from the same builder')
        return Expr(f'({self.source}) {operator} ({other.source})', self.owner)

    def __and__(self, other):
        return self._logic(other, 'and')

    def __or__(self, other):
        return self._logic(other, 'or')

    def __invert__(self):
        return Expr(f'not ({self.source})', self.owner)


class Ref(Expr):
    def __getitem__(self, key):
        if isinstance(key, int) and not isinstance(key, bool) and key >= 0:
            key = str(key)
        return Ref(self.source + '.' + _segment(key), self.owner)

    def __getattr__(self, key):
        if key.startswith('_'):
            raise AttributeError(key)
        return self[key]

    def _compare(self, other, operator):
        if isinstance(other, Ref):
            if other.owner is not self.owner:
                raise ValueError('cannot mix references from different rituals')
            rhs = other.source
        elif other is None or isinstance(other, (str, bool, int, float)):
            rhs = json.dumps(other, ensure_ascii=False, allow_nan=False)
            if isinstance(other, str) and '\\' in rhs:
                raise ValueError('expression string literals do not support escapes; use data.compare')
        else:
            raise TypeError('comparison requires a reference or scalar literal')
        return Expr(f'{self.source} {operator} {rhs}', self.owner)

    def __eq__(self, other): return self._compare(other, '==')
    def __ne__(self, other): return self._compare(other, '!=')
    def __lt__(self, other): return self._compare(other, '<')
    def __le__(self, other): return self._compare(other, '<=')
    def __gt__(self, other): return self._compare(other, '>')
    def __ge__(self, other): return self._compare(other, '>=')
    __hash__ = None


@dataclass(frozen=True)
class Type:
    definition: dict

    def __call__(self, **constraints):
        names = {'min':'minimum', 'max':'maximum', 'min_items':'minItems', 'max_items':'maxItems',
                 'min_length':'minLength', 'max_length':'maxLength'}
        allowed = set(names) | {'enum', 'description', 'default'}
        unknown = constraints.keys() - allowed
        if unknown:
            raise TypeError(f'unknown type constraints: {sorted(unknown)}')
        return Type({**self.definition, **{names.get(k,k):v for k,v in constraints.items()}})

    def __getitem__(self, item):
        if self.definition.get('type') != 'array':
            raise TypeError('only List accepts an item type')
        return Type({**self.definition, 'items': _schema(item)})


Str = Type({'type':'string'})
Int = Type({'type':'integer'})
Number = Type({'type':'number'})
Bool = Type({'type':'boolean'})
List = Type({'type':'array'})


def Object(fields=None, *, closed=True):
    if fields is None:
        return Type({'type':'object'})
    return Type({'type':'object', 'required':list(fields), 'additionalProperties':not closed,
                 'properties':{k:_schema(v) for k,v in fields.items()}})


def _schema(value):
    if isinstance(value, Type):
        return copy.deepcopy(value.definition)
    if isinstance(value, dict):
        return copy.deepcopy(value)
    raise TypeError('schema must be a Type or JSON schema object')


def _encode(value, owner):
    if isinstance(value, Ref):
        if value.owner is not owner:
            raise ValueError('cannot mix references from different builders')
        return '${' + value.source + '}'
    if isinstance(value, Expr):
        raise TypeError('expressions belong in when/fail_if, not argument values; use data.compare for a boolean value')
    if isinstance(value, Type):
        return _schema(value)
    if isinstance(value, dict):
        if any(not isinstance(k, str) for k in value):
            raise TypeError('JSON object keys must be strings')
        return {k:_encode(v,owner) for k,v in value.items()}
    if isinstance(value, (list, tuple)):
        return [_encode(v,owner) for v in value]
    if value is None or isinstance(value, (str, int, float, bool)):
        json.dumps(value, allow_nan=False)
        return value
    raise TypeError(f'not a JSON literal or reference: {type(value).__name__}')


class _Data:
    _positionals = {'parse_json':['text'], 'project':['value','fields'], 'count':['value'],
                    'sum_integers':['values'], 'assert':['condition'], 'get':['value','path'],
                    'keys':['value'], 'concat':['left','right'], 'slice':['values','start','end'],
                    'sort':['values'], 'unique':['values'], 'trim':['text'], 'split':['text','separator'],
                    'join':['values','separator'], 'replace':['text','from','to'],
                    'arithmetic':['left','right','operator'], 'compare':['left','right','operator']}

    def __init__(self, ritual): self.ritual = ritual

    def __getattr__(self, name):
        if name not in self._positionals:
            raise AttributeError(f'unknown data primitive {name}; use map/filter/fold methods for collections')
        def build(*args, id=None, **kwargs):
            keys = self._positionals[name]
            if len(args) > len(keys): raise TypeError('too many positional arguments')
            values = dict(zip(keys,args))
            if values.keys() & kwargs.keys(): raise TypeError('duplicate argument')
            values.update(kwargs)
            return self.ritual.op('data.'+name, id=id, **values)
        return build


class Ritual:
    def __init__(self, name, *, description='', model=None, settings=None, essence=None,
                 reagents=None, requires=None, bindings=None, drift=None):
        _segment(name)
        self._definition = {'language_version':1, 'ritual':name, 'steps':[], 'inputs':{},
                            'reagents':{}, 'return':{}}
        self._stack = [self._definition['steps']]
        self._counter = 0
        self._ids = set()
        self.data = _Data(self)
        for key,value in [('description',description), ('model',model), ('settings',settings),
                          ('essence',essence), ('requires',requires), ('bindings',bindings), ('drift',drift)]:
            if value is not None and value != '':
                self._definition[key] = copy.deepcopy(value)
        for key,value in (reagents or {}).items():
            self._definition['reagents'][key] = {'select':list(value)} if isinstance(value,(list,tuple)) else copy.deepcopy(value)

    def input(self, name, type=Str, *, default=_MISSING, user_only=False):
        _segment(name)
        if name in self._definition['inputs']: raise ValueError('duplicate input '+name)
        definition = _schema(type)
        unsupported = definition.keys() - {'type','minimum','maximum','enum'}
        if unsupported: raise ValueError(f'input declarations cannot enforce {sorted(unsupported)}; validate with data.parse_json or an explicit schema')
        kind = {'array':'list','number':'decimal'}.get(definition['type'],definition['type'])
        if kind not in ('list','decimal','integer','string','boolean'): raise ValueError('unsupported input type')
        out = {'type':kind, **{ {'minimum':'min','maximum':'max'}.get(k,k):v for k,v in definition.items() if k!='type'}}
        if default is not _MISSING: out['default'] = _encode(default,self)
        if user_only: out['user_only'] = True
        self._definition['inputs'][name] = out
        return Ref('inputs.'+name,self)

    def _id(self, id):
        if id is None:
            self._counter += 1
            id = f'step_{self._counter}'
            while id in self._ids:
                self._counter += 1
                id = f'step_{self._counter}'
        id = _segment(id)
        self._ids.add(id)
        return id

    def _select(self, operation):
        reagent, name = operation.split('.',1)
        selected = self._definition['reagents'].setdefault(reagent, {'select':[]})['select']
        if name not in selected: selected.append(name)

    def op(self, tool, *, id=None, verify=None, hold=None, **arguments):
        self._select(tool)
        id = self._id(id)
        step = {'id':id, 'operation':tool, 'arguments':_encode(arguments,self)}
        if verify is not None: step['verify'] = _encode(verify,self)
        if hold is not None: step['hold'] = _encode(hold,self)
        self._stack[-1].append(step)
        return self.ref(id)

    def model(self, *, prompt, instructions=None, returns=None, id=None, name=None,
              settings=None, bind=None, abstain_if=None):
        id = self._id(id)
        body = {'prompt':_encode(prompt,self)}
        for key,value in [('instructions',instructions),('name',name),('settings',settings),('bind',bind)]:
            if value is not None: body[key] = _encode(value,self)
        if returns is not None: body['schema'] = _schema(returns)
        if abstain_if is not None:
            expression = abstain_if(Ref('output',self)) if callable(abstain_if) else abstain_if
            body['abstain_if'] = self._expression(expression)
        self._stack[-1].append({'id':id,'model':body})
        return self.ref(id)

    def call(self, name, *, id=None, version=None, hash=None, **arguments):
        id = self._id(id)
        step = {'id':id,'ritual':name,'arguments':_encode(arguments,self)}
        if version is not None: step['version'] = version
        if hash is not None: step['hash'] = hash
        self._stack[-1].append(step)
        return self.ref(id)

    def ref(self, id): return Ref('steps.'+_segment(id)+'.output',self)

    def _expression(self, expression):
        if not isinstance(expression, Expr) or expression.owner is not self:
            raise TypeError('expected an expression belonging to this ritual')
        return expression.source

    @contextmanager
    def when(self, condition):
        step = {'if':self._expression(condition), 'then':[]}
        self._stack[-1].append(step)
        self._stack.append(step['then'])
        try: yield
        finally: self._stack.pop()

    def fail_if(self, condition, message, *, id=None):
        with self.when(condition):
            self.op('data.assert',id=id,condition=False,message=message)

    @contextmanager
    def each(self, values, *, id=None, as_='item', on_failure=None):
        if not isinstance(values, Ref) or values.owner is not self: raise TypeError('each source must be a reference in this ritual')
        id = self._id(id)
        step = {'id':id,'each':_segment(as_)+' in '+values.source,'steps':[]}
        if on_failure is not None: step['on_failure'] = on_failure
        self._stack[-1].append(step)
        self._stack.append(step['steps'])
        try: yield Ref(as_,self)
        finally: self._stack.pop()

    def map(self, values, operation, arguments, *, id=None):
        self._select(operation)
        body = arguments(Ref('item',self)) if callable(arguments) else arguments
        return self.op('data.map',id=id,values=values,operation=operation,arguments=body)

    def filter(self, values, predicate, *, params=None, id=None):
        # params values are bound as params.NAME, never pasted into the predicate.
        expression = predicate(Ref('item',self)) if callable(predicate) else predicate
        extra = {} if params is None else {'params': params}
        return self.op('data.filter',id=id,values=values,predicate=self._expression(expression),**extra)

    def fold(self, values, initial, operation, arguments, *, id=None):
        self._select(operation)
        body = arguments(Ref('acc',self),Ref('item',self)) if callable(arguments) else arguments
        return self.op('data.fold',id=id,values=values,initial=initial,operation=operation,arguments=body)

    def returns(self, fields, *, schema=None):
        self._definition['return'] = _encode(fields,self)
        if schema is not None: self._definition['output_schema'] = _schema(schema)
        return self

    def definition(self):
        if len(self._stack)!=1: raise ValueError('finish all runtime scopes before emitting')
        return copy.deepcopy(self._definition)

    def emit(self, stream=None): emit(self,stream)


def emit(ritual, stream=None):
    """Write an artifact, not a validation/approval or execution request."""
    json.dump(ritual.definition(),stream or sys.stdout,indent=2,ensure_ascii=False,allow_nan=False)
    (stream or sys.stdout).write('\n')
