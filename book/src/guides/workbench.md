# The command workbench

Run `summonr` or `summonr console` (`shell` is its alias). The main pane is a
persistent Summonr command prompt; the left pane shows the core and its runs.
There is no need to type the executable name before every command:

```text
rituals
inspect total-receipt
run total-receipt --arg 'text={"amounts_minor":[100,25]}' --wait
job status receipt-totals
record s-YOUR-RUN-ID
```

Both keyword and flag spellings work. Commands use the same parser, authority,
and execution paths as the standalone CLI. The workbench uses normal readable CLI output. Add `--json` explicitly when
you want the full structured envelope. Arrays and keys such as `items`, `outputs`, and
`partial` in user results are displayed as data, not interpreted as UI instructions.

## The left instrument

The status view polls the existing daemon roughly twice per second. It shows:

- process and version, active runs, ended root runs, and pending petitions;
- the selected run's ID, ritual hash, outcome, elapsed time, and item progress;
- its root model, request/token limits and reported usage;
- recent recorded events, including model calls and operations.

By default, the panel follows the newest observed active run, or the latest
completed run when none are active. Use `F3` to pin one of up to 100 active and 50
recent ended runs, or enter `scry ID` to pin a specific run. Type `follow` to
resume automatic selection. An unknown ID is reported in the transcript. The standalone `summonr scry` still provides
the full run-and-evidence observer. A root without a model may have model-backed
steps or children; the sidebar says so rather than assuming zero inference.
Token totals are reported usage, not proof that all backend usage was available.
The panel does not invent GPU, cache, model-residency, or backend-digest telemetry.

`inspect NAME` loads the saved ritual and draws its structure in the left pane.
The diagram distinguishes pure computation, reads, external writes, model calls,
child calls, loops, and conditional branches. Both branches are shown; the diagram
is a **static plan**, not a claim that those steps executed. Child calls are shown
with their pins and are not recursively expanded. `F2` switches between this plan
and live status. Opening or inspecting the interface performs no inference.

## Editing and navigation

| Key | Action |
| --- | --- |
| `Enter` | Submit the command |
| `Left` / `Right`, `Home` / `End` | Move within the command |
| `Up` / `Down` | Browse this session's command history |
| `F1` | Show the workbench key guide |
| `F2` | Switch status / ritual plan |
| `F3` | Select the next run |
| `F4` | Show or hide the sidebar |
| `PageUp` / `PageDown` | Scroll the transcript |
| `Alt-PageUp` / `Alt-PageDown` | Scroll the sidebar |
| `Ctrl-L` or `clear` | Clear the transcript |
| `Ctrl-C` | Clear the input; does not cancel daemon work |
| `Ctrl-D` at an empty prompt, or `exit` | Leave the workbench |

Quote values containing spaces or JSON, just as in the examples. Pasted text
stays in the editor until Enter is pressed. Quoting is supported; shell expansion,
pipes, redirection, command substitution, and operating-system commands are not.
The `shell` alias refers to this Summonr console, not to Bash or another shell.

Commands run independently of sidebar polling. Up to eight ordinary commands may be pending. A separate slot is reserved for
one `stop` request, so eight waits cannot prevent cancellation. Local navigation
and exit remain available when command slots are full. Status commands use the
ordinary slots; the sidebar polls independently. Each completed
command is labeled with a session number and exit code. This is finite command
execution, not an agent loop. `core` without an operation and `mcp` are foreground
servers and must be launched from a separate terminal.

The transcript retains the most recent 2,000 lines and history the most recent
100 commands, in memory only. Each output stream is displayed up to 1 MiB; a
larger stream is drained and an explicit truncation notice is shown. Use the
standalone CLI to capture complete output. The workbench does not replace durable
run records. It inherits the directory, environment, and `SUMMONR_HOME` from the
terminal where it was opened.

Closing the workbench leaves accepted work running. To cancel a run, use
`stop ID` explicitly. Polling never restarts a core you stopped: disconnected data
is labeled as stale. Submitting a command that needs the core starts it normally.

Below 100 columns, the sidebar stacks above the prompt when the terminal is at
least 22 rows high; otherwise it is hidden. Below 36 columns or 14 rows the resize
message replaces the workspace.
`F4` makes more room for commands. Colour, ASCII, and reduced-motion preferences
follow the rest of Summonr.

Cancellation is cooperative. An accepted stop request displays **STOPPING** while
an in-flight call returns; this does not claim the backend has stopped computing.
A client cannot guarantee remote inference cancellation. After completion, the
normal terminal outcome replaces that indication.

An unfinished quote is echoed with its error and the editor is cleared. Press Up
to restore and repair the rejected command. Static diagrams show the ritual's
model fallback and full conditions; runtime overrides can change the actual model.
Recent evidence is abbreviated in the sidebar; `record ID` retains full details.
