# Pacts across three devices

This walks through pacts on three devices you control, each standing in for an organisation:

- **A, the manufacturer.** The caller; your main machine.
- **B, the supplier.** The callee.
- **C, an insurer and a witness.** C has its own pact with B, and witnesses A's and B's chains.

`scripts/check_pacts_remote.py` runs the A/B part on one machine over loopback. This guide is the same test on real devices.

> Without `tls: {cert, key}` in `config.yaml`, pact contents travel over plain HTTP: signed and checked, but readable on the network. Configure `tls` or use a private network.

## 1. Identities and pins

On each device:

```bash
summonr identity
```

Compare every full fingerprint out of band, by reading it aloud or on another screen, before pinning anything.

```bash
# on A
summonr peer add supplier url http://B:7744 key ed25519:…B
summonr peer add witness  url http://C:7744 key ed25519:…C
# on B
summonr peer add maker    url http://A:7744 key ed25519:…A   # optional push target if A listens; A also polls
summonr peer add insurer  url http://C:7744 key ed25519:…C
summonr pact admit maker   key ed25519:…A
summonr pact admit insurer key ed25519:…C
summonr listen 0.0.0.0:7744
# on C
summonr peer add supplier url http://B:7744 key ed25519:…B
summonr peer add maker    url http://A:7744 key ed25519:…A
summonr witness accept maker    key ed25519:…A
summonr witness accept supplier key ed25519:…B
summonr listen 0.0.0.0:7744
```

## 2. The supplier's ritual and the terms

On B, with a mailbox whose `Orders` folder holds a few messages (`mail_root` in `config.yaml`):

```bash
summonr save examples/07-pacts/commit-order.yaml
summonr allow commit-order from mail all on folder=Orders
summonr allow commit-order from mail selecting locate
summonr compile examples/07-pacts/commit-order.yaml    # note the hash
```

On A:

```bash
summonr compile examples/07-pacts/check-quote.yaml      # note the hash
```

Then fill in `order-parts.yaml`:
- both core keys;
- both hashes;
- check-quote's source.

Set `accept: owner` to decide deliveries yourself.

```bash
summonr pact offer supplier order-parts.yaml
```

On B, read the pact, then agree:

```bash
summonr pact show order-parts     # the pinned ritual's effects and A's acceptance ritual, in full
summonr pact agree order-parts
```

## 3. Calls

On A, allow a familiar and give it to an agent:

```bash
summonr familiar new buyer
summonr familiar allow buyer pact order-parts
summonr mcp as buyer
```

The agent calls `call_pact` twice:
- a small order (`amount: 10`), which is delivered and then accepted;
- a large one (`amount: 5000`), which stops at a held action.

The large order waits for you:

```bash
summonr pact poll
summonr petition                  # the held action, as a pact.decide petition
summonr petition permit p-…
summonr pact status order-parts 2 wait
summonr pact accept order-parts 2   # under accept: owner
```

## 4. A decision that never comes

Revise the pact with `deadline: 2m` (terms with `revises: <old id>`), offer it, and agree it on B. Then make a large call and don't decide it. After two minutes, `summonr pact status` on A shows the held action settled as `expired`. B's mailbox is unchanged.

## 5. A second organisation

On B, write terms for a pact with C over another ritual and offer it with `summonr pact offer insurer …`. On C, `summonr pact fetch supplier`, then `pact show` and `pact agree`, then make a call.

Check the separation:
- `summonr pact list` on A doesn't show C's pact;
- A's familiars can't see it either.

## 6. Witnessing and checking

```bash
# on A and on B
summonr witness witness
# on A and on B
summonr pact export order-parts thread.json
```

Copy both `thread.json` files to C. Then:

```bash
summonr verify a-thread.json      # C has both A and B pinned
summonr verify b-thread.json
```

Both verify with the same entries, and every delivery's epitaph checks against B's key.

## 7. Clock skew

Set one device's clock more than two minutes off. Its next pact request gets the generic `not_authenticated` refusal, and the reason appears only in the other core's log. Fix the clock, and `summonr pact poll` catches up.
