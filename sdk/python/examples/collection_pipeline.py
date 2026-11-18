from summonr import Ritual, Int, List, Object

r = Ritual('collection-basics', description='Multiply, filter, and reduce with pure computation and zero inference.',
           essence={'requests': 0, 'tool_calls': 10, 'items': 10, 'time': '5s'},
           reagents={'data': ['map', 'filter', 'fold', 'arithmetic']})
doubled = r.map([-2, 1, 3], "data.arithmetic",
                lambda item: {"left": item, "right": 2, "operator": "multiply"}, id="doubled")
positive = r.filter(doubled, lambda item: item > 0, id="positive")
total = r.fold(positive, 0, "data.arithmetic",
               lambda acc, item: {"left": acc, "right": item, "operator": "add"}, id="total")
r.returns({"positive": positive, "total": total},
          schema=Object({"positive": List[Int], "total": Int}))

if __name__ == "__main__":
    r.emit()
