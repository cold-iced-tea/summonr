import io
import json
import unittest
from summonr import Ritual, Str, Int, List, Object


class BuilderTests(unittest.TestCase):
    def test_future_values_cannot_be_used_as_python_values(self):
        r = Ritual('test')
        value = r.input('value', Int)
        for operation in [lambda:bool(value),lambda:bool(value > 2),lambda:str(value),
                          lambda:int(value),lambda:float(value),lambda:iter(value),
                          lambda:list(value),lambda:len(value),lambda:f'{value}',
                          lambda:(value < 1) == True]:
            with self.assertRaises(TypeError): operation()
        with self.assertRaises(TypeError):
            if value > 1:
                self.fail('a future condition executed at authoring time')
        with self.assertRaises(TypeError):
            for _ in value:
                self.fail('runtime iteration became a Python loop')

    def test_filter_params_are_arguments_not_predicate_text(self):
        r=Ritual('test', essence={'requests':0})
        floor=r.input('floor',Int)
        values=r.input('values',List)
        kept=r.filter(values,lambda item: item['score'] >= 1,params={'floor':floor},id='kept')
        r.returns({'kept':kept})
        step=r.definition()['steps'][0]
        self.assertEqual(step['arguments']['params'],{'floor':'${inputs.floor}'})
        self.assertNotIn('${',step['arguments']['predicate'])

    def test_cleanup_steps_go_to_finally_and_can_test_what_ran(self):
        r=Ritual('test', essence={'cleanup_time':'1m'})
        branch=r.input('branch',Str)
        wt=r.op('git.worktree_add',repo='demo',branch=branch,id='wt')
        with r.cleanup():
            with r.when(r.status('wt') == 'returned'):
                r.op('git.remove_worktree',repo='demo',path=wt['path'],id='remove')
        r.returns({'path':wt['path']})
        definition=r.definition()
        self.assertEqual([s['id'] for s in definition['steps']],['wt'])
        guard=definition['finally'][0]
        self.assertEqual(guard['if'],'steps.wt.status == "returned"')
        self.assertEqual(guard['then'][0]['arguments']['path'],'${steps.wt.output.path}')
        self.assertIn('remove_worktree',definition['reagents']['git']['select'])
        with self.assertRaises(ValueError):
            with r.cleanup():
                with r.cleanup(): pass
        self.assertNotIn('finally',Ritual('plain').definition())

    def test_scopes_expressions_and_literal_types(self):
        r=Ritual('test', essence={'requests':0})
        values=r.input('values',List)
        with r.each(values,id='items',as_='entry') as item:
            with r.when((item > 0) & (item < 10)):
                r.data.count([item],id='size')
        r.returns({'items':r.ref('items')})
        definition=r.definition()
        self.assertEqual(definition['steps'][0]['steps'][0]['if'],'(entry > 0) and (entry < 10)')
        out=io.StringIO();r.emit(out)
        self.assertEqual(json.loads(out.getvalue()),definition)
        definition['ritual']='changed'
        self.assertEqual(r.definition()['ritual'],'test')
        with self.assertRaises(ValueError):r.input('values',List)
        with self.assertRaises(ValueError):r.input('typed',List[Int])

    def test_cross_builder_and_implicit_expression_arguments_fail(self):
        a,b=Ritual('a'),Ritual('b')
        x,y=a.input('x',Int),b.input('y',Int)
        with self.assertRaises(ValueError):a.data.count(y)
        with self.assertRaises(ValueError):x == y
        with self.assertRaises(TypeError):a.op('data.assert',condition=x > 0)
        with self.assertRaises(ValueError):a.returns({'not_finite':float('nan')})
        with self.assertRaises(ValueError):a.data.get(x, 'x', id='bad.id')
        with self.assertRaises(ValueError):x == 'a\tb'

    def test_model_schema_and_abstention_are_declarative(self):
        r=Ritual('test')
        text=r.input('text',Str)
        result=r.model(id='label',prompt=text,returns=Object({'label':Str(enum=['yes','abstain'])}),
                       abstain_if=lambda output:output.label == 'abstain')
        r.returns({'label':result.label})
        d=r.definition()
        self.assertEqual(d['steps'][0]['model']['abstain_if'],'output.label == "abstain"')
        self.assertEqual(d['steps'][0]['model']['prompt'],'${inputs.text}')


if __name__=='__main__':unittest.main()
