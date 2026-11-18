# Identity: keys, familiars and peers

Every Summonr core has a signing key, and so does every agent you give access. An agent's requests are signed. The core checks each signature before acting, records which agent asked, and keeps agents out of each other's work. A core on another device can be called the same way, over HTTP. This is the groundwork for ritual proofs and agent-to-agent contracts (see [Planned key features](../planned-features.md)).

## What keys protect, and what they don't

| They protect against | They do not protect against |
| --- | --- |
| Someone on the network pretending to be an agent or a core | Malware running as you on the same machine: it can read your key files |
| A request or reply being changed in transit | A compromised device |
| A captured request being replayed later | A model being wrong: a signature proves who asked, never that the answer is true |
| One agent seeing, stopping or reusing another's runs | Anyone reading traffic on your network: requests are signed, **not encrypted** (no TLS yet) |

**On one machine**, every agent runs as you, so keys give separation and a clear record rather than protection from a hostile local program. **Between devices**, the separation is real: a device can only act through a key you registered, and only on the rituals you allowed.

## Files

| File | What it is |
| --- | --- |
| `config/core.key` | This core's private key. Created on first start. PKCS#8 PEM, mode 0600. |
| `config/core.pub` | This core's public key, `ed25519:…`. Safe to share. |
| `config/familiars/NAME.key` | A local agent's private key, mode 0600. |

Private keys are never written anywhere else: not to the database, logs, records, or anything a model or a calling agent can read. Summonr refuses to load a key file other users can read, and tells you to run `chmod 600`. An existing key file is never overwritten.

All cryptography is Ed25519 from the `ed25519-dalek` library, with strict verification and the operating system's random source.

## Fingerprints and seals

A key's **fingerprint** is the SHA-256 of its 32 public-key bytes, shown in groups of four:

```console
$ summonr identity
❦ IDENTITY · this core
  key        ed25519:vTegXg2ruIzYyfJKwmT__b5T_09ISLEUoE04YtQiVOY
  fingerprint 0cae 66d0 0013 f4c6 e09e 1adc 515f 893c 8ee2 b9d2 b5c6 95de e1b2 cf2f afe1 f166
```

When you add another device's key, **compare the full fingerprint** with what that device shows, over a separate channel (in person, a phone call). At a terminal, a seal is drawn beside it. The seal helps you recognise a key at a glance, but it is only 27 bits; it is not a way to compare keys.

## Familiars

A familiar is an agent bound to call on this core's rituals within what you allow: one of your own on this device, or another house's agent on another device. Each familiar has its own key, its own allowed rituals and its own runs.

```bash
summonr familiar new claude                  # make a key on this device
summonr familiar allow claude total-receipt  # a new familiar may call nothing until you allow rituals
summonr familiar add partner key ed25519:…   # a key made on another device
summonr familiar list
summonr familiar disallow claude total-receipt
summonr familiar revoke partner              # its next request is refused
```

Plain spellings: `summonr agent new claude`, `summonr agent add partner --key ed25519:…`. Names are 1–32 lowercase letters, digits and hyphens, and are never reused, even after revocation. A revoked familiar's runs that are already going continue unless you stop them.

What a familiar can do:

- See and call only the rituals you allowed. Others answer "not found", exactly as if they did not exist.
- See, wait on and stop only runs it started. Another agent's run answers "not found".
- See only held actions from its own runs. Only you can approve them.
- Use its own idempotency keys; another familiar's identical key is a different request.
- Nothing administrative: it cannot grant, inscribe, approve, run assays or drills, or manage familiars and peers. Those stay with you at the terminal.

The same restrictions as for any agent apply: it cannot change `user_only` inputs, or record, replay or simulate. It also cannot choose the model, settings or ritual version: the ritual runs as you saved it. A single `wait` blocks for at most 300 seconds.

## Connecting an agent

An agent always talks through `summonr mcp` on **its own device**. That process holds the agent's key, signs every request and checks every reply. The agent itself never sees the key.

```bash
claude mcp add --scope user summonr -- summonr mcp --name claude
codex mcp add summonr -- summonr mcp --name codex
```

Keyword spelling: `summonr mcp as claude`. Without `--name`, `summonr mcp` connects as the anonymous `agent`, as before.

## Another device: peers and `listen`

On the device being called:

```bash
summonr listen 0.0.0.0:7744      # keep this running
```

It accepts only signed requests at `POST /summonr/v1/signed`, refuses browser requests and bodies over 1 MiB, cuts off any request that hasn't fully arrived within 10 seconds, and passes each request, unchanged, to the local core, which checks it. (`summonr mcp http` is different: it serves only the anonymous agent and refuses `--name`.)

On the calling device, **pin** that core by hand, with the key its `summonr identity` shows:

```bash
summonr peer add lab url http://lab.local:7744 key ed25519:…
summonr mcp --name partner --via lab        # what the agent's MCP config launches
```

The calling device accepts only replies signed by the key you pinned. Nothing is trusted on first contact.

## The signed envelope

A request carries base64 payload bytes, the signing key's id and an Ed25519 signature over `summonr-signed-request-v1`, a zero byte, and those exact bytes. The payload names its purpose (`summonr/request/v1`), the familiar, the **audience** (the core it is meant for), a timestamp, a one-time **nonce**, the method and its parameters.

The core verifies the signature **before** reading the payload, then checks the purpose, that the familiar matches the key, that it is the intended audience, that the timestamp is within **±120 seconds** of its own clock, and that the nonce is new. Nonces are kept on disk, so a replay is refused even after a restart. Devices therefore need roughly correct clocks; enable NTP on each.

Replies are signed the same way by the core's key (`summonr-signed-response-v1`), and name the nonce of the request they answer.

## When a request is refused

The caller only ever sees `not_authenticated`: "request not authenticated", exit 4. The real reason (unknown or revoked key, bad signature, wrong core, clock skew, replay) goes to the core's log, `state/core.log`, so an attacker learns nothing from it. A caller whose reply fails verification sees "reply not authenticated".

## Three devices: a walkthrough

This is what `scripts/check_familiars_remote.py` does on one machine. Here, `lab` is a device that runs rituals and `desk` is a device whose agent calls it.

1. On **lab**: `summonr inscribe total-receipt.yaml`, then `summonr identity`. Note the key and fingerprint.
2. On **lab**: `summonr listen 0.0.0.0:7744`.
3. On **desk**: `summonr familiar new partner`. Note the key.
4. On **lab**: `summonr familiar add partner key ed25519:…` with desk's key. Compare fingerprints with desk. Then `summonr familiar allow partner total-receipt`.
5. On **desk**: `summonr peer add lab url http://lab.local:7744 key ed25519:…` with lab's key. Compare fingerprints.
6. On **desk**: point the agent at `summonr mcp --name partner --via lab`. It sees `total-receipt` and nothing else, and lab's history shows each run as `familiar:partner`.
7. Try the refusals: revoke `partner` on lab, or pin a wrong key on desk, and watch the calls fail.

The third device can play a second organisation: give it its own familiar on lab, and confirm the two cannot see each other's runs.

## Not yet

- **TLS.** Traffic between devices is authenticated, not encrypted.
- **An offline root key** for users who need one: a key kept off the machine that certifies the core key, so a stolen core key can be replaced without changing identity. `summonr identity` already reports `certified_by` (always empty for now) so it can be added without changing formats.
- **Encrypted key files** (a passphrase).
- An independent security review before real outside parties rely on this.
