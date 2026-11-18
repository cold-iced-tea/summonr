from summonr import Ritual, Str, Object

r = Ritual('label-review', description='Label the sentiment of one product review.',
           essence={'requests': 1, 'tokens': 4000, 'time': '1m'}, model='ollama/qwen3:4b', settings={'temperature': 0})
text = r.input("text", Str)
label = r.model(id="label", prompt=text,
    instructions="""Label the overall sentiment of this product review.
- positive: the reviewer is happy with the product.
- negative: the reviewer is unhappy with the product.
- mixed: the review contains both real praise and real complaints.
- abstain: the text is not a review of the product (a question, spam).
Answer with JSON only.
""",
    returns=Object({"sentiment": Str(enum=["positive", "negative", "mixed", "abstain"])}),
    abstain_if=lambda output: output.sentiment == "abstain")
r.returns({"sentiment": label.sentiment})

if __name__ == "__main__":
    r.emit()
