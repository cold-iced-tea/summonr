# The command workbench

Run `summonr` or `summonr console` (`shell` is its alias). The command prompt stays below the session transcript. The board above it shows the local core and its runs.
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

## The run board

At 80×24 the board has two run windows. A taller terminal can show up to four.
Each window shows the ritual, a short run ID, its observed status and elapsed time.
The most recent operation appears beside a small seal. Its points follow the last
four recorded operation or model steps. The right edge closes when the run ends
and its last pending petition clears. The core animates while work is active and
shows a question mark when an action needs review. Counts come from the daemon.
A returned run may still have a held write.

Run windows keep their positions while visible. Pending petitions take priority
over recent history. F3 pins another run, including ones outside the visible set.
F6 opens the next pending petition in the run area. Esc closes that view.
Use `petition list` for the full request and the normal petition commands to
decide it. Incoming updates preserve the text and cursor at the prompt.

## Execution path

Press F7 on a selected run to open its execution path. Summonr reads the saved
ritual version, verifies its hash against the run, and shows the compiled steps
beside their recorded activity. Up and Down select a row. The evidence pane shows
its eight most recent entries, newest first. Page Up and Page Down scroll the evidence.
Press `r` to refresh the snapshot or Esc to return to your draft.

A pending write opens on its petition, including the operation, arguments, and
petition ID. A returned run opens on the recorded result. For other outcomes the
view starts at the last observed event and keeps the error in the outcome row.
Branch rows show the latest recorded decision. Older runs may have no branch
events. Rows without evidence have a blank status. Events with ambiguous or
missing step IDs remain in the run ledger.

## Run details

F5 switches between the board and the detailed instrument.

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
| `F4` | Show or hide the panels |
| `F7` | Open the selected run’s execution path |
| `F5` | Switch board / run details |
| `F6` / `Esc` | Review the next petition / close review |
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

The board fits from 60 columns and 22 rows. At smaller sizes the detailed instrument stacks above the prompt when space permits. Wide detail views sit alongside the transcript. Below 36 columns or 14 rows the resize
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
