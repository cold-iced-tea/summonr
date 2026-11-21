# Pacts: agent-to-agent contracts

A **pact** (plain spelling: `contract`) is a standing agreement between two Summonr cores. Under it, one organisation's agent hands work to the other organisation's core, through a ritual both sides inspected and pinned in advance. Each side ends up holding the same signed record of what was agreed, asked, done, approved and accepted, and either can check the other's copy offline.

Think of a manufacturer (the **caller**) ordering from a supplier (the **callee**), two insurers settling a claim, or a hospital screening patients for a trial.

## What both sides agree

The terms are a small YAML file (see `examples/07-pacts/order-parts.yaml`):

- **The ritual, pinned by its content hash.** It runs on the callee. If the callee's saved copy no longer has that hash, calls are refused.
- **Inputs:**
  - `fixed` holds values the pact sets, such as a threshold;
  - `inputs` lists what each call may give.
  A call can't change a fixed input or pass anything else.
- **Ceilings:**
  - per call (requests, tokens, time, tool calls);
  - for the whole pact (number of calls, total requests and tokens);
  - a `deadline` for the buyer's decisions.
- **Held by the caller:** steps of the ritual the caller must also approve, optionally only when a true/false value the ritual itself computes is true (for example `over_threshold`). Summonr adds no expression language; the condition is ordinary ritual code both sides can read.
- **Acceptance:**
  - `accept: owner` (the default) means the caller's owner decides each delivery;
  - `accept: checks` lets the checks decide.
  Checks always run first:
  - the ritual's return schema (`schema: true`);
  - optionally a caller-side **acceptance ritual**, carried in full in the terms so the callee can read it.
- **`expires`**, and for a revision, **`revises`**: the pact it replaces.
- **Optional `witnesses`:** additional core public keys allowed to receive and sync the signed thread for independent third-party verification.

## What stays local

- **Authority never crosses.** The callee runs under its own grants for the pinned ritual, narrowed to the pact; the caller's grants mean nothing there.
- **The stricter budget wins.** The run's limits are the smallest of the ritual's essence, the pact's per-call ceilings, and anything the callee's owner tightened with `pact limit`. Both sides refuse a call over the pact's totals.
- **Approvals stay with the party that owns them.**
  - A held write needs the callee owner's approval if the ritual holds it.
  - It also needs the caller owner's approval if the pact does.
  - Caller holds add to the callee's holds and never replace them. A deny from either side is final.
  - The write still runs under the callee's grants at the moment it is performed.
- **Agents decide nothing.**
  - A familiar may call only under pacts its owner allowed (`familiar allow NAME pact P`), and sees only its own calls.
  - Held actions, acceptance and agreement belong to owners.
  - The anonymous MCP agent can't use pacts at all.

## A call, end to end

1. The caller's core signs a **call** entry and sends it to the callee over `listen`.
2. The callee runs the pinned ritual. Holds do not pause the run: a held write becomes a petition, and the run carries on.
3. When the run ends, the callee signs a **delivery**: the outcome, the result, its usage, the held actions, and the run's [epitaph](epitaphs.md). The callee asynchronously pushes new entries (`pact_post`) to reachable caller and witness peers, with caller polling as a fallback. The caller checks that epitaph against the callee's key and the pinned hash; a delivery that fails that check freezes the pact.
4. Each held action is **decided**. The caller's owner sees caller holds as ordinary petitions (`summonr petition`). The callee performs or drops the action and signs a **settle** entry.
5. If the caller never decides, the action expires at the deadline on the callee's own clock. It is never performed, and the expiry is recorded on both sides.
6. Once every held action is settled, the caller's core runs the checks. The result is signed as an **accept** or **reject** entry by the caller's core, decided by the checks or by its owner (`summonr pact accept P CALL`).

A completed run and an accepted deliverable are separate records: a run can return and still be rejected.

## The thread

Every event is an entry signed by one core over `summonr-pact-v1`, naming the entry before it. Together they form one hash chain per pact: the **thread**. Both cores store all of it.

- **Ordering:** the callee orders the thread. A caller entry sent on an out-of-date head is refused, and the caller signs it again on top.
- **Anchoring:** every entry a core signs also goes into its own [epitaph chain](epitaphs.md) as a fingerprint only, so a core can't drop a pact without breaking its chain. Witnesses vouch for pact entries without seeing anything in them.
- **Forks:** two different entries signed by the same core on the same previous entry are a **fork**, which is signed proof that the core equivocated. The receiving core keeps both and freezes the pact, and `verify` reports it (exit 15).

```bash
summonr pact history order-parts          # every entry, re-verified
summonr pact export order-parts t.json    # the thread, for anyone to check
summonr verify t.json                     # with both cores' keys pinned, or `key K`
```

`verify` never trusts a key written in the file. It uses this core's own key, pinned peers, or a key you give.

## Revising and ending

Changes need an explicit revised pact: an offer whose terms say `revises: <old id>`, agreed like any other. When it is agreed, the old pact is ended (superseded) and takes no new calls, but calls already open on it finish. Either owner can `pact end P`.

## Commands

| Keyword form | Plain spelling | Effect |
|---|---|---|
| `pact offer PEER FILE [as NAME]` | `contract offer …` | Send terms to a pinned peer |
| `pact fetch PEER` | `contract fetch PEER` | Fetch offers a callee made to this core |
| `pact list` / `pact show P` | `contract list` / `show P` | Pacts; terms, both rituals, calls, warnings |
| `pact agree P` / `pact decline P [because T]` | same | Sign or refuse an offer |
| `pact call P given k=v… [wait]` | `contract call P --arg k=v` | Make a call |
| `pact status P CALL [wait]` | same | A call's state and entries |
| `pact accept P CALL` / `reject P CALL [because T]` | same | Decide a delivery (`accept: owner`) |
| `pact history P` / `pact export P FILE` | same | The thread |
| `pact admit NAME key K` | `contract admit NAME --key K` | Callee: admit a counterparty core |
| `pact limit P k=v…` | same | Callee: tighten per-call limits |
| `pact end P [because T]` / `pact poll` | same | End a pact / sync now |
| `familiar allow NAME pact P` | `agent allow NAME --pact P` | Let a familiar call under a pact |

MCP tools for a familiar (`summonr mcp as NAME`): `list_pacts`, `describe_pact`, `call_pact`, `check_pact_call`.

## Limits, plainly

- **Transport encryption is opt-in:** without `tls: {cert, key}` in `config.yaml`, inputs and results travel over plain HTTP (signed and checked, but readable on the network). Configure `tls` for encrypted transport or run over a private network.
- **The callee runs the work on its machine.** Its epitaph proves what its core recorded, not that its model or backend was honest.
- **Only the two cores hold contents.** A regulator or court checks an export one of them hands over; witnesses only prove entries existed when witnessed.
- **Which familiar asked** is the caller's own claim, recorded but not provable to the callee.
- **Two parties per pact.** A claim involving three companies is several pacts.
- **Push with polling fallback:** when the callee has a reachable URL for the caller or a witness, it pushes new entries asynchronously; the caller also polls (every 15 s while calls are open, backing off to 5 min) so callers behind NAT still sync.
