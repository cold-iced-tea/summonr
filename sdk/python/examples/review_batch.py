from summonr import Ritual, List

r = Ritual('label-reviews', description='Label a list of reviews, within a fixed budget of model requests.',
           essence={'requests': 10, 'tokens': 40000, 'items': 50, 'time': '5m'})
reviews = r.input("reviews", List)
with r.each(reviews, id="labels", as_="review", on_failure="continue") as review:
    r.call("label-review", id="label", text=review)
r.returns({"labels": r.ref("labels")})

if __name__ == "__main__":
    r.emit()
