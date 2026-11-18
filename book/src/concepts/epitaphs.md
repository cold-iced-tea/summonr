# Epitaphs, chain marks and witnesses

When a run ends, its core signs a compact, permanent account of it: an **epitaph** (plain spelling: `receipt`). A run's full record is its [elegy](evidence.md); its epitaph is the signed fingerprint of that elegy. Epitaphs form one chain per core, so someone holding an epitaph, part of the chain, and the core's public key can check offline that the record is genuine and unaltered, without seeing what the run contained.

## What an epitaph holds

An epitaph has two parts:

- **The body** says what happened, in fingerprints:
  - the run and its parent;
  - the ritual's name, version and content hash;
  - who asked (you, the anonymous `agent`, or `familiar:NAME`);
  - start and end times, and the outcome;
  - salted fingerprints of the inputs, result, error, limits and cleanup;
  - one combined fingerprint over all of the run's events (every model request and response, every effect) with their count;
  - the epitaphs of its child runs.
- **The link** puts the body in the chain: its position, the hash of the previous link, the core's key id and the body's hash. The link is what the core signs.

Nothing in an epitaph is the content itself. Each piece's fingerprint is mixed with its own random salt, kept on this machine beside the elegy, so even a small value like `apply: true` can't be guessed from its fingerprint. Fingerprints cover the bytes exactly as stored, so a genuine record stays verifiable whatever later versions of Summonr do to formatting.

Every run that ends gets one, including child runs and runs the core recovers as `interrupted` after a crash. The epitaph is written in the same database step that records the run's end, so no run can finish without one. Runs that ended before your core had epitaphs have none; the chain starts with the first run after the upgrade.

## From the console

```console
$ summonr epitaph s-4f2a…            # plain: summonr receipt s-4f2a…
❦ EPITAPH · inbox-triage v3 · returned
  run        s-4f2a…   by familiar:claude
  chain      #4182   after 77c1 0e9b …
  contents   inputs, result and 12 event(s); 1 child run(s) — fingerprints only
  signed by  773b a206 d1b6 755c   ✓ verifies

$ summonr chain mark
summonr-mark:v1:4182:9f3a61c2e0d4b7a1…  2026-11-16T10:42Z
  Paste this anywhere other people can see it: an email, a chat, a notebook.

$ summonr verify summonr-mark:v1:4182:9f3a61c2…
✓ 4395 epitaph(s) verify
✓ the chain still contains this mark; 213 epitaph(s) added since, none altered
```

A **chain mark** is a one-line snapshot of your chain at a moment. Anywhere you put it becomes an informal witness: once someone else has seen a mark, you can't change or remove anything at or before it without the mark failing.

`summonr verify` with no argument checks your whole chain:
- every signature;
- every link to the one before;
- every fingerprint against the elegy still on disk.

It names the first epitaph with a problem and exits **15**. If you've deleted an old elegy, its epitaph still verifies as signed and chained, but its contents can no longer be re-checked; that run is listed as *not checkable*, never as tampered.

## Showing someone a run without showing everything

```bash
summonr epitaph s-4f2a… reveal inputs > inputs.json     # or result, error, essence, cleanup, event:3
summonr epitaph export 4100 4200 chain.json             # a chain segment
```

A reveal file contains the epitaph and one piece of the elegy with its salt. Anyone with your core's public key can run `summonr verify inputs.json key ed25519:…` to confirm the piece is exactly what the epitaph fingerprinted, and learn nothing else about the run. An export can be checked the same way, and `summonr verify chain.json MARK` checks a mark against it.

`verify` never trusts a key a file merely claims. It uses `key ed25519:…`, or your own core's key, or a pinned [peer](identity.md)'s.

## Witnesses

Whoever holds a core's key can, in principle, rewrite its history and re-sign everything. Signatures alone can't prevent that; other people holding copies can expose it. A **witness** is a core that keeps a countersigned copy of your chain's head as it grows.

On the witness (one of your other devices, a partner, or a witnessing service):

```bash
summonr witness accept desk key ed25519:…   # your core's key, from `summonr identity`
summonr listen 0.0.0.0:7744
```

On your core, with the witness pinned as a peer:

```bash
summonr witness lab          # submit what's new; lab countersigns
summonr witness list         # on lab: the cores it witnesses, their heads, any forks
```

- **What a witness sees:** only the chain's links, never bodies, salts or contents. It learns how many runs happened and when it was shown them, not which rituals ran, who asked, or what came out.
- **What it checks:** that what you send extends exactly what it last countersigned, then it countersigns the new head. On its first visit it vouches from the head it's shown; it makes no claim about history before that.
- **Forks:** if a core offers a history that doesn't extend what was witnessed, the witness refuses and records a **fork**, keeping both heads. That's the evidence that a history was rewritten. If the witness holds a head your own chain no longer contains, `summonr witness` tells you so rather than sending anything.
- **Silence:** a dishonest owner wouldn't submit a rewritten chain; they'd simply stop witnessing. So `summonr verify` lists each witness's last countersignature and how long ago it was.

Witnessing runs when you say so; `summonr witness lab` from cron makes it regular.

## Public timestamps (RFC 3161)

```bash
summonr witness tsa https://timestamp.example     # any RFC 3161 timestamp authority
summonr verify tsa-ca authority-ca.pem
```

A timestamp authority signs a token saying your chain head existed at a time. Several are free; some carry formal legal weight (for example, qualified providers under the EU's eIDAS rules). All certificate and token handling is done by the system's `openssl`; Summonr contains none of its own. Set `openssl: /path/to/openssl` in `config.yaml` to use a particular one. Without a CA file, or without `openssl`, stored timestamps are reported as unchecked, which is never a failure of the chain.

## Limits, plainly

- **Unwitnessed history can be rewritten** by whoever holds the core key. Marks, witnesses and timestamps make a rewrite detectable after the point they cover; nothing makes it impossible.
- **Split view:** a core could keep two histories and show each to a different witness. Catching that needs witnesses, or the people checking, to compare what they hold. One trusted witness everyone relies on doesn't have this problem.
- **Deleted elegies** leave epitaphs that verify as signed but can't be re-checked against their contents.
- **Post-run petition decisions** (`permit`, `deny`, `stale`, `failed`, `unverified`) are sealed onto the chain in the same transaction that updates the petition (`run = petition:<id>`), and `chain_verify` re-checks the stored petition row against its salted fingerprint.

## The format

Epitaph links carry `v: 1` and `chain: 1`. `chain: 2` is reserved for a Merkle transparency log, which would let a witness check consistency, and anyone check that one epitaph is in a witnessed chain, with short proofs instead of every link. It will be added without invalidating existing epitaphs. Domains: links are signed over `summonr-epitaph-v1`, witness countersignatures over `summonr-witness-v1`.
