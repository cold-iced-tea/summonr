# Installation

Summonr is a single Rust binary. You need a Rust toolchain (edition 2021) and, to use real models, an [Ollama](https://ollama.com) server.

## Build

```bash
git clone <your summonr checkout>
cd summonr
cargo build            # debug build at target/debug/summonr
cargo test             # the test suite runs without a model
```

To put it on your `PATH`:

```bash
cargo install --path .     # installs to ~/.cargo/bin/summonr
```

If `~/.local/bin` is already on your PATH, you can instead install a release build there:

```bash
cargo build --release --offline
install -m 755 target/release/summonr ~/.local/bin/summonr
```

Then, from any directory:

```bash
summonr                # command workbench + live sidebar
summonr daemon         # small daemon status panel
summonr scry            # dedicated run-and-evidence observer
```

This is a normal local application, not confined to the demo environment.
`SUMMONR_HOME` selects an isolated workspace only when you set it. A fresh terminal
uses your normal user state unless you have configured that variable globally.
Opening the UI starts the core if needed; leaving the UI leaves the core running.
Rebuild and repeat the install command after source changes to update the installed copy.

SQLite is bundled, so there is no separate database to install.

## A model backend

Summonr talks to two backends:

| Model id | What it is |
| --- | --- |
| `ollama/<name>` | A model served by Ollama, for example `ollama/qwen3:4b` |
| `mock/keyword` | A deterministic stand-in with no model at all |

Point Summonr at Ollama with `ollama_url` in the [config file](../reference/config.md), or with the `SUMMONR_OLLAMA_URL` or `OLLAMA_HOST` environment variables. The default is `http://127.0.0.1:11434`.

`mock/keyword` needs nothing. It picks the first option in an `enum` whose name appears in the prompt, and otherwise answers `abstain`. It is good for exercising mechanics (grants, holds, limits, records) and useless as evidence of quality. Its token counts are estimates and are labeled that way.

## Keep experiments separate

`SUMMONR_HOME` puts the socket, state and config under one directory. Use it whenever you are trying things out, so your real grimoire is never touched:

```bash
export SUMMONR_HOME=/tmp/summonr-sandbox
```

For a zero-setup walkthrough, `summonr demo` creates and cleans up its own temporary `SUMMONR_HOME` and Git repository automatically. See [Configuration and files](../reference/config.md) for where things live without it.

## Check it works

```bash
summonr version
summonr help
summonr core status
summonr core restart            # gracefully stop (if idle) and start a fresh core
summonr core stop
```

`version` and `help` do not need the core to be running. Any command that does need it starts the core automatically; `core restart` is the fastest way to reload a freshly installed binary without losing state.

## Build this book

The HTML documentation is built separately from the Summonr binary:

```bash
cargo install mdbook --locked
export PATH="$HOME/.cargo/bin:$PATH"
mdbook build book
```

Run these commands from the repository root. Open `book/build/index.html` in a browser, or use `mdbook serve book` for a local preview that rebuilds when you edit a chapter. If `mdbook` is already installed but the command is unavailable, check that its installation directory is on your `PATH`.
