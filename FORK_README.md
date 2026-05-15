# Running DeepSeek-TUI from this fork

This fork contains tool-input repair patches on the `tool-input-repair` branch
that improve DeepSeek model tool-calling reliability. These changes are not
upstreamed yet.

## Prerequisites

### Rust toolchain (required)

DeepSeek-TUI is a Rust project. The minimum supported Rust version is **1.88**.

Install via rustup (recommended):

```bash
curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh -s -- -y
source "$HOME/.cargo/env"
```

On macOS with Homebrew you can alternatively do:

```bash
brew install rustup
rustup-init -y
source "$HOME/.cargo/env"
```

Verify the installation:

```bash
rustc --version   # should be >= 1.88
cargo --version
```

### Other dependencies

The build uses `aws-lc-sys` which needs a C compiler and CMake:

```bash
# macOS (Xcode command-line tools are usually already installed)
xcode-select --install   # if not already installed

# CMake (needed by aws-lc-sys for TLS)
brew install cmake
```

On Linux (Debian/Ubuntu):

```bash
sudo apt-get install build-essential cmake pkg-config
```

## Building from source

```bash
cd src/tui
git checkout tool-input-repair

# Debug build (faster compile, slower runtime — good for testing)
cargo build

# Release build (slower compile, optimised runtime — use this for daily use)
cargo build --release
```

The release binary lands at:

```
src/tui/target/release/deepseek-tui
```

The CLI binary (if you also use `deepseek` command) lands at:

```
src/tui/target/release/deepseek
```

## Running

### Option A: Run directly

```bash
./target/release/deepseek-tui
```

### Option B: Shadow the npm-installed version

If you previously installed via `npm install -g deepseek-tui`, the npm binary
is a wrapper around a pre-built binary. You can shadow it by putting the cargo
build output earlier in your PATH.

Add to your `~/.zshrc` (or `~/.bashrc`):

```bash
# Use fork build of deepseek-tui instead of npm version
export PATH="$HOME/Desktop/TheOne/www/claude/wasseem/projects/tooling-tui/src/tui/target/release:$PATH"
```

Then reload: `source ~/.zshrc`

Verify which binary is active:

```bash
which deepseek-tui
# Should point to target/release/deepseek-tui, not the npm path
```

### Option C: Symlink over the npm binary

```bash
# Find where npm installed it
npm list -g deepseek-tui
which deepseek-tui
# e.g. /Users/you/.nvm/versions/node/v20.20.0/bin/deepseek-tui

# Back up the original and symlink
mv "$(which deepseek-tui)" "$(which deepseek-tui).bak"
ln -s "$PWD/target/release/deepseek-tui" "$(which deepseek-tui)"
```

To revert: `mv "$(which deepseek-tui).bak" "$(which deepseek-tui)"`

## Running tests

```bash
source "$HOME/.cargo/env"
cargo test -p deepseek-tui --bins
```

To run only the tests related to the tool-input repair changes:

```bash
cargo test -p deepseek-tui --bins -- arg_repair
cargo test -p deepseek-tui --bins -- deferred
cargo test -p deepseek-tui --bins -- tool_catalog
```

## Keeping in sync with upstream

```bash
# One-time: add upstream remote
git remote add upstream https://github.com/Hmbown/DeepSeek-TUI.git

# Fetch and merge latest upstream into main
git checkout main
git fetch upstream
git merge upstream/main

# Rebase the patch branch onto updated main
git checkout tool-input-repair
git rebase main
```

The patches touch a small number of files with additive changes, so rebases
should be clean unless upstream rewrites the same functions:

- `crates/tui/src/tools/arg_repair.rs` — new functions added
- `crates/tui/src/core/engine/dispatch.rs` — small additions to existing functions
- `crates/tui/src/core/engine/tool_catalog.rs` — new function + modified hydration logic
- `crates/tui/src/core/engine/turn_loop.rs` — field correction wiring
- `crates/tui/src/core/engine.rs` — one import line changed

## What the patches do

### 1. Null stripping and markdown autolink unwrapping (`arg_repair.rs`)

DeepSeek models send `null` for optional fields instead of omitting them, and
sometimes wrap file paths in markdown autolinks (`[file.md](http://file.md)`).
The sanitiser strips both after JSON parsing.

### 2. Execute deferred tools on first call (`tool_catalog.rs`, `turn_loop.rs`)

Instead of bouncing deferred tool calls with "retry with loaded schema" (which
wastes a full API round-trip), the harness checks whether the model's input
already has all required fields. If so, the tool executes immediately.

Field-name corrections are applied automatically before the check:
`old_string` → `search`, `new_string` → `replace`, `file_path` → `path`, etc.

### 3. Stringified array unwrapping (`dispatch.rs`, `arg_repair.rs`)

When DeepSeek emits a JSON array as a string (`"[{...}]"` instead of `[{...}]`),
the parallel tool-call parser now unwraps it. A schema-aware helper
(`try_unwrap_stringified_json`) is also exposed for tools that need it on
specific fields.
