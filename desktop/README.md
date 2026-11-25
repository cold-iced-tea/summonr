# Summonr Desktop

Launch `./desktop/run` from the checkout on a Linux desktop with Wayland or X11 and a Vulkan-capable graphics driver. The launcher builds the CLI and GPUI client. The desktop crate has its own lockfile so a terminal-only build stays small. Installed clients find `summonr` on PATH, or use the executable named by `SUMMONR_BIN`. Both clients honor `SUMMONR_HOME`.

The console draws its wordmark directly on a fixed grid. The directory shows recorded run times and model calls. On wide windows, hover a directory row or timing bar to preview the recorded outcome and latest observed step. With the command line empty, use the up and down arrows to select a run and Enter to open its window. Click a bar in the run-time strip to open its record. Bars run oldest to newest and share the peak shown at the right. Its character-cell ring turns while the core has active runs and displays a question mark for held petitions. Set `SUMMONR_REDUCED_MOTION=1` to hold the ring still.

On wide boards the idle inscription opens in fine-constellation glyphs and resolves to “it rests in an empty palm” (Leonard Cohen). Click it to switch readings. The glyphs spell the English sounds using the Marain project’s MSP2 sound codes. Reduced motion starts in English.

In the preview and dedicated run window, the seal reserves positions for named steps in the saved ritual version. Points and connecting strokes follow observed step events in ledger order. Hover or click a point to read its latest evidence. A hollow apricot point indicates a pending petition for that step. Open two records of the same ritual version to compare their paths.

Click a run or rite in the console to open its desktop window, or press Alt with its directory number (1–9). Click again to raise it. Closing the window releases its observer. Open the same ID later to read the daemon's current record. Commands in the input use Summonr argument syntax, including quoted JSON. Enter submits the command.

| Console input | Action |
| --- | --- |
| `run NAME --arg key=value` | Start a saved ritual and open its run |
| `rite start FILE` | Start a rite and open its stage list when the command returns |
| `open ID` | Open or raise an existing run or rite |

For a zero-model example, save `examples/10-bughunt/batch-ready.yaml` with `summonr save`, then enter `run bughunt-batch-ready --arg 'output={"pass":false}'` in the console. The process window displays the Bughunt masthead, its actual ledger and the returned gate value.

## Laboratory

Click **Laboratory** on the board, enter `laboratory`, or select a star in the Orrery and choose **Use as baseline**. The lab uses the same local core. Its shelf reads the latest 100 completed assay series and 35 recent top-level runs from that core’s SQLite store in read-only mode. The word *baseline* on the bench means the left side of this comparison. Choosing it leaves the core’s recorded assay baseline alone.

Choose an assay, then a case. **Pair as trial** aligns another series of the same ritual by case ID. To try a single case, edit its version, model, JSON inputs or expected fields and click **Run simulated trial**. Writes are simulated and model calls use the configured backend. A case with no expected fields is unscored. The recorded difference compares event kinds and step names for the first 256 events and compares returned values. For matching ritual hashes, focus the bench and hold Space or click **Overlay**. Shared strokes are pale, baseline-only strokes violet and trial-only strokes apricot.

To run a complete assay, enter the saved ritual name and an absolute assay-set path. A blank path uses the core’s saved baseline set for that ritual. For a zero-model example, save `examples/10-bughunt/batch-ready.yaml`, enter `bughunt-batch-ready` and use `examples/10-bughunt/batch-ready.assay.yaml` from the checkout. **Run simulated assay** stores a scored series that can be reopened from the shelf. Closing the window releases its observers. Submitted work continues on the core; the current bench selection and single-trial pairing are session-local.

## Designing a window

Add an optional top-level block to a ritual or rite definition.

```yaml
window:
  layout: sidebar
  title: Night shift
  by: nightjar
  accent: "#D8A478"
  masthead:
    - '    .  *'
    - '  *(   )*'
    - '    *  .'
```

Each field is optional. Choose `banner` for a masthead above the activity or `sidebar` to place it alongside. Narrow windows stack the masthead automatically. Labels and art lines accept up to 64 characters. The masthead accepts up to five lines. The accent is a six-digit RGB color. These fields are part of the versioned definition. Editing a saved ritual's design uses the normal replacement workflow. Older runs keep their recorded design. The window always includes the canonical ritual or rite name, instance ID and observed status.

Click **Orrery** on the board or enter `orrery` to open its independent window. The dial spans local noon to the following noon. Rings run from the innermost listed ritual outward. Select a ritual to dim the other tracks, click a star or run row for its latest seal, and drag the dial to inspect which top-level runs were underway and which of their direct petitions were pending at that time. Earlier and Later move by one night. Now returns to the current board time. The Orrery refreshes every five seconds from the same core and reads up to the latest 1,000 ended top-level runs.

Active run windows check for new ledger events after 160 ms and immediately fetch additional full pages. The active directory polls after 300 ms. Idle polling and run metadata refreshes use a one-second interval. Run windows keep the most recent 64 ledger entries they have read. Rite windows show each stage and its gate state. When disconnected the console and windows identify their last observation.

To build the optional client directly, run `cargo build --manifest-path desktop/Cargo.toml --locked`. Its GPUI dependencies are pinned to the gpui-pre 0.3.12 snapshot. To install locally, install both Cargo packages and copy `summonr-desktop.desktop` into your applications directory with the installed binaries on your desktop PATH.


### Run oversight

With the command prompt empty, press `/` to search the directory and `Esc` to clear it. Search terms match ritual names, run ID prefixes, and outcomes or states such as `held`. Multiple terms narrow the list together. Arrow keys and Alt+1–9 follow the displayed order. The directory reads up to 1,000 active summons and the latest 1,000 ended top-level summons, plus rites. Child counts in the preview come from the selected run’s direct children.

The Orrery shows the runs that finished during the displayed local noon-to-noon window and those still active at its cutoff. Select a star to see its ritual, duration, and recorded outcome.
