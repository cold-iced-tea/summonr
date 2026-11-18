# Watching runs

With no arguments, `summonr` opens the [command workbench](workbench.md), which pairs a command prompt with a live sidebar.

## The observer

Run `summonr scry` (or `summonr watch`) to open the observer, a terminal UI showing:

- every active summon and recently ended summon, with its sigil and state;
- **essence gauges**: requests, tokens and time used against each limit;
- batch progress (`done/total`);
- a live ledger of the selected summon.

Closing the observer never stops work.

| Key | Action |
| --- | --- |
| `j` / `↓` | Next summon |
| `k` / `↑` | Previous summon |
| `g` / `Home` | First |
| `G` / `End` | Last |
| `1` / `2` / `3` / `4` | Ledger / result / authority / full record |
| `Tab` / `r` | Next evidence view |
| `PageUp` / `PageDown` | Scroll evidence, including wrapped lines |
| `f` | Follow the latest ledger entries; reset other views to the top |
| `z` | Expand the selected summon to the full workspace |
| `d` | Open the daemon panel |
| `?` | Open the key guide |
| `b` | Banish the selected summon (press again to confirm) |
| `Esc` | Close the overlay, or leave the observer |
| `q` | Leave the observer; work continues |

Below 96 columns the selected summon fills the workspace. Selection keys still
work; the ritual and ID identify the current selection. The minimum useful size
is 36 columns by 12 rows. Smaller terminals show a resize message.

The authority view shows recorded effects, current petition states, and cleanup.
It does not grant permission or approve held actions. The full record retains
inputs and result data; use `summonr record ID --json` for machine-readable export.
A request count of zero is an observation about usage, not a proof of determinism.
The compiled contract describes the workflow's execution class.

## The daemon panel

```bash
summonr daemon
./scripts/vigil.sh              # a popup inside tmux; otherwise the current terminal
```

`summonr panel` is the plain-language alias for `summonr daemon`.

The **Vigil** panel shows the connected core's process, version, socket, active
count, ended root-run count, and pending petitions. Like other clients it starts
the core if needed. It never runs a ritual itself. Press `d` or `Esc` to return to
scry, or `q` to leave. In tmux, closing the popup also leaves the daemon running.
The launcher uses the local debug binary; set `SUMMONR_BIN` to an absolute path
for another build. It inherits `SUMMONR_HOME`.

The workspace uses Ratatui with dark ink, quiet grey rules, brass headings, bone
text, and coloured states. An empty workspace shows a centered instrument seal
and starting commands. Populated workspaces show a fixed-width run index and
visible evidence tabs. The daemon panel dims the workspace behind it; small
terminals use a compact status layout. A brief border highlight marks a changed view or selection; a phase mark
indicates active work. Animation does not represent model reasoning or progress.

The observer subscribes to live events over `core.sock` via `subscribe` (with cursor polling as fallback when reconnecting).

It needs a real terminal. To capture it from a script, run it in a detached `tmux` session.

## From the command line

```bash
summonr circle                 # active summons   (list)
summonr graveyard              # ended summons    (history)
summonr await s-05ce3b2c       # block until it ends (wait); add `within 5m` to bound the wait
summonr ledger s-05ce3b2c      # event log        (logs)
summonr elegy s-05ce3b2c       # full record      (record)
summonr banish s-05ce3b2c      # ask it to stop   (stop)
```

`within` bounds how long you wait, not the work. If the wait times out, the command exits with code 7 and the summon keeps running.

## Ledger symbols

Each ledger line starts with an offset from the summon's start and a mark for the kind of event. The ASCII mark is used with `SUMMONR_ASCII`, `NO_COLOR` or `SUMMONR_PLAIN`:

| Mark | ASCII | Event |
| --- | --- | --- |
| ☉ | `>` | Summon started |
| · | `-` | Operation (tool call), held write, or other event |
| ✶ | `~` | Model request, with tokens in and out and duration |
| ↳ | `\` | Child ritual called |
| ❧ | `+` | Batch item finished |
| ◌ | `?` | Abstention |
| ✕ | `!` | Refused operation, invalid output, or backend error |
| ≉ | `!` | Drift warning |
| ⊘ | `x` | Banish requested |
| ◁ | `<` | Model released (unload requested, left loaded, or held for a series) |
| ∎ | `.` | Summon ended |

## Colour and glyphs

| Variable | Effect |
| --- | --- |
| `NO_COLOR` or `SUMMONR_PLAIN` | No colour and no ornament |
| `SUMMONR_REDUCED_MOTION` | Stop phase animation and view highlights |
| `SUMMONR_ASCII` | Keep colour, use ASCII instead of other glyphs |
| `style: flag` in config | Help and suggestions use the flag spelling |
