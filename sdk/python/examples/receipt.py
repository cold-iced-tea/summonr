from summonr import Ritual, Str, Int, List, Object

r = Ritual("receipt-total",
    description=("Read a pasted receipt, extract the line amounts with a model, "
                 "add them up in code, and check the sum against the printed subtotal."),
    essence={"requests": 1, "tokens": 8000, "tool_calls": 3, "time": "2m"},
    model="ollama/qwen3:4b", settings={"temperature": 0},
    reagents={"data": ["sum_integers", "count", "assert"]})
text = r.input("text", Str)
extracted = r.model(id="extract", prompt=text,
    instructions="""You read receipts. Report two things, in cents, as integers (4.50 is 450):
- line_items_cents: the price of every purchased line item. Do not
  include subtotals, tax, tips, totals, or payments.
- printed_subtotal_cents: the subtotal printed on the receipt.
Copy numbers only. Do not add anything up. Answer with JSON only.
""",
    returns=Object({
        "line_items_cents": List[Int(min=0, max=10000000)](max_items=200),
        "printed_subtotal_cents": Int(min=0),
    }))
lines = r.data.count(extracted.line_items_cents, id="lines")
total = r.data.sum_integers(extracted.line_items_cents, id="total")
r.fail_if(total != extracted.printed_subtotal_cents,
          "the line items do not add up to the printed subtotal; check the receipt by hand",
          id="mismatch")
r.returns({"line_items_cents": extracted.line_items_cents,
           "lines": lines, "subtotal_cents": total},
          schema=Object({"line_items_cents": List[Int], "lines": Int(min=0),
                         "subtotal_cents": Int(min=0)}))

if __name__ == "__main__":
    r.emit()
