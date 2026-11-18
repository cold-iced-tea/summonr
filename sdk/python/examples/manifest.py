from summonr import Ritual, Str, Int, List, Object

r = Ritual('check-manifest', description='Validate a service manifest and summarize it, with no model.',
           essence={'requests': 0, 'tool_calls': 3, 'time': '5s'},
           reagents={'data': ['parse_json', 'project', 'count']})
text = r.input("text", Str)
manifest = r.data.parse_json(text, id="manifest", schema=Object({
    "name": Str(min_length=1, max_length=63), "version": Str(min_length=1),
    "replicas": Int(min=1, max=50), "dependencies": List[Str](max_items=100),
}))
identity = r.data.project(manifest, ["name", "version"], id="identity")
deps = r.data.count(manifest.dependencies, id="deps")
r.returns({"service": identity, "dependency_count": deps},
          schema=Object({"service": Object(), "dependency_count": Int(min=0)}))

if __name__ == "__main__":
    r.emit()
