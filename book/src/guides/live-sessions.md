# Tutorial: Live summons and live bindings

A **live summon** (also called a **session**) lets you experiment interactively with a model—setting its system prompt (`soul`), attaching live files or text (`bind`), adjusting budgets (`imbue`), asking bounded turns (`bid`), trimming context (`lethe`), and compiling the finished session directly into a saved ritual (`inscribe --session`).

## 1. Open a bounded session

Start a session with a name, a model, and an explicit essence budget:

```bash
SUMMONR_HOME=$(mktemp -d)
export SUMMONR_HOME

summonr summon as analyst --model mock/keyword \
  --arg requests=10 --arg tokens=50000 --timeout 5m
```

Keyword spelling works too:

```bash
summonr summon as analyst through mock/keyword given requests=10 tokens=50000 within 5m
```

This prints a session ID (for example `m-01...`). Use `summonr summon list` to inspect open sessions or `summonr summon show SESSION_ID` to see turns and usage.

## 2. Give the session a soul (system prompt)

Set or replace the session's system instructions from text or a file:

```bash
summonr soul SESSION_ID "You extract themes and verbatim quotes from interview notes. Stay concise."
# or from a file:
# summonr soul SESSION_ID --file soul.md
```

Calling `summonr soul SESSION_ID` without new text prints the current system prompt.

## 3. Attach live bindings

Bind documents or inline text to named slots. Summonr hashes the content so the exact snapshot is recorded in the session and carried into any ritual you inscribe from it:

```bash
cat > /tmp/interview-notes.txt <<'EOF'
Speaker A: "The hardest part wasn't the software; it was getting three departments to agree on what 'urgent' meant."
Speaker B: "Once we wrote down the checklist, response time dropped in half."
EOF

summonr bind SESSION_ID as notes --file /tmp/interview-notes.txt
# or bind inline text:
# summonr bind SESSION_ID as rubric --task "Quote verbatim; abstain if unsure."
```

## 4. Tune settings or budget mid-session

Use `imbue` to adjust model parameters (`temperature`, etc.), swap the model, or tighten/update essence limits without restarting:

```bash
summonr imbue SESSION_ID --set temperature=0 --arg requests=12
```

## 5. Ask bounded turns (`bid` / `ask`)

Send prompts against the session's soul, bindings, and remaining essence. You can also enforce a JSON Schema on the reply (`--schema`):

```bash
summonr bid SESSION_ID "Summarize the main operational theme from the bound notes."

# Enforce structured JSON output:
summonr bid SESSION_ID "Return theme and verbatim quote." \
  --schema '{"type":"object","properties":{"theme":{"type":"string"},"quote":{"type":"string"}},"required":["theme","quote"]}'
```

## 6. Trim history (`lethe` / `forget`)

If early exploratory turns are cluttering context, keep only the most recent `N` turns—or clear all turns (`all`) while keeping the `soul` and `bindings`:

```bash
summonr lethe SESSION_ID 2      # keep only the last 2 turns
summonr lethe SESSION_ID all    # clear turn history, keep soul and bindings
```

## 7. Compile the session into a ritual

Once the prompt, bindings, and settings work the way you want, inscribe the session into your grimoire as a reusable, versioned ritual:

```bash
summonr inscribe interview-summarizer --session SESSION_ID
summonr rituals
```

When you're finished with the interactive session, close it:

```bash
summonr summon close SESSION_ID
summonr core stop
```
