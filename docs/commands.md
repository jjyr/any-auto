# Command reference

Complete command and option reference for `agy-auto-approve` and its installation commands.
AI reviews require `agy` (CLI mode) or the host's `agentapi` (sidecar mode), and an active login.

## Installation

Install the binary using the Release download or Cargo registry commands in the
[README](../README.md#install), then run `agy-auto-approve install`.
Release archives support macOS and Linux on ARM64 and x86_64.
Release downloads also include `SHA256SUMS`; you can verify the archive against
its matching entry using `sha256sum` (Linux) or `shasum -a 256` (macOS).

## Updates

```bash
agy-auto-approve update                  # Upgrade to the latest stable release
agy-auto-approve update --version v0.4.2  # Install a specific stable version
```

`update` checks the current executable's installation root and Cargo's
`.crates2.json` records. Registry installations use `cargo install --locked --force`
with the original root and registry index. Other standalone binaries download a
GitHub Release using `curl`, verify SHA-256 and the executable version, then
atomically replace the current binary. Download or validation failures leave the
existing binary unchanged. The executable directory must be writable.
Cargo failures are reported without falling back to Release downloads.
Cargo Git and local-path installation sources are unsupported.

After upgrading, the new executable refreshes only the currently enabled CLI
hook and/or Desktop sidecar. Existing configurations from older versions are
recognized; disabled or missing integrations remain unchanged. If neither is
enabled, run `install` to enable the plugin. Configuration refresh failures are
reported separately from the completed binary upgrade.

Both mode daemons are stopped after the upgrade; the CLI starts the new version
on its next AI review request. Restart Antigravity Desktop when its sidecar is
enabled. A failed daemon stop is reported and may require a manual restart.

## Help and version

```bash
agy-auto-approve --help
agy-auto-approve --version
agy-auto-approve help logs
agy-auto-approve logs --help
agy-auto-approve logs show --help
agy-auto-approve daemon --help
agy-auto-approve daemon run --help
agy-auto-approve install --help
agy-auto-approve update --help
agy-auto-approve hook --help
```

Use `-h` as a short form of `--help`, or `-V` for `--version` on the top-level command.

## Plugin installation

```bash
agy-auto-approve install                 # Register CLI hooks and Desktop sidecar
agy-auto-approve install --cli-only      # Register CLI hooks only
agy-auto-approve install --desktop-only  # Register Desktop sidecar only
```

Installation preserves unrelated settings and updates existing hooks. CLI registration writes `~/.gemini/config/hooks.json` and adds development command permissions to existing CLI settings. Desktop registration deploys and enables the sidecar. The two options are mutually exclusive.

Installation uses the executable's absolute path. If you move it, run `install` again.

## Daemon

```bash
agy-auto-approve daemon start                 # Start in the background
agy-auto-approve daemon status                # Inspect the current daemon
agy-auto-approve daemon stop                  # Stop and wait for socket cleanup
agy-auto-approve daemon restart               # Restart with a fresh reviewer conversation
agy-auto-approve daemon run                   # Run in the foreground
agy-auto-approve daemon run --idle-timeout 0   # Disable idle shutdown
```

`start` returns the existing daemon's status if it is already running. CLI hooks automatically start the daemon when an AI review is needed.

`restart` stops the daemon, waits for its lifetime lock to be released, removes the
selected mode’s entire `sessions/` directory (reviewer IDs and workspaces), and starts the daemon. If already stopped,
it clears those sessions and starts it. The next approval creates a new conversation
using the latest configuration; remote conversations are not deleted. Ordinary
`stop` / `start` retains each user’s cached conversation. Restart interrupts active reviews.
For Desktop sidecars, restart the host if it needs to reattach to the daemon.

`status` does not start the daemon. It reports the PID, version, socket, uptime, cached session count, and approval counts when running; it exits with code 1 when stopped. `start`, `status`, and `stop` write JSON to stdout and diagnostics to stderr.

`run` is intended for foreground use or service managers. Its `--idle-timeout SECONDS` option defaults to `1800` (30 minutes); `0` disables idle shutdown.

All daemon commands accept `--mode cli|sidecar`, defaulting to `cli`. Each mode has its own daemon, lock, session, and circuit breakers. Default sockets are `~/.gemini/antigravity-cli/approver-cli.sock` and `approver-sidecar.sock`. Status includes `mode`. Desktop registration explicitly selects sidecar mode.

`hook --mode cli|sidecar` overrides routing. Without it, a nonempty `ANTIGRAVITY_LS_ADDRESS` selects sidecar; otherwise CLI. See [sidecars](sidecars.md) for migration and backend details.

## Approval logs

```bash
agy-auto-approve logs
agy-auto-approve logs --limit 100
agy-auto-approve logs --decision deny
agy-auto-approve logs --tool run_command --conversation CONVERSATION_ID
agy-auto-approve logs --json
agy-auto-approve logs -f
agy-auto-approve logs --follow --decision deny
agy-auto-approve logs -f --json
agy-auto-approve logs show APPROVAL_ID
```

| Option | Behavior |
| --- | --- |
| `--limit N` | Show up to N recent completed approvals; defaults to 20 and must be at least 1. |
| `--decision VALUE` | Filter by `allow`, `deny`, `ask`, or `force_ask`. |
| `--tool NAME` | Filter by tool name. |
| `--conversation ID` | Filter by conversation ID. |
| `--json` | Output a JSON array, or JSON Lines when following. |
| `-f`, `--follow` | Show recent matching approvals, then follow newly completed approvals until Ctrl-C. |

List options can be combined. Normal lists show newest records first. Follow mode shows the initial matching records in chronological order, waits for the log file if necessary, and handles detected replacement or truncation.

`logs show APPROVAL_ID` outputs all recorded events for an exact approval ID as JSON, including hook input, reviewer input/output, and the final decision. It cannot be combined with list options. Incomplete approvals may have events available through `show` even though they do not appear in summaries.

Logs are read directly from `~/.gemini/agy-auto-approve/approvals-YYYY-MM-DD.jsonl` (UTC dates); the daemon does not need to be running. These records include `mode` and contain approval-service and backend output, not the subsequent tool execution's stdout/stderr. Logs are not automatically rotated or cleaned up.

New approval summaries include the command and working directory when supplied in
`CommandLine` and `Cwd`, and display the pipeline stage. `reviewer_error` indicates
an approval infrastructure or response-format failure, whereas `reviewer` indicates
a parsed reviewer decision. Older records may lack the command and directory;
`logs show ID` still exposes their original hook input.

Detailed `agentapi_request` and `agy_request` events include the daemon PID, operation, effective
search PATH, and CLI fallback directory. Process errors include the operation and
failure stage; session persistence errors identify the affected path. Logs do not
dump the full process environment.

Only dated logs are read. Legacy `approvals.jsonl` files are not read or migrated.
`logs --follow` discovers new daily files and drains remaining events from earlier files.

To inspect records in a different directory:

```bash
AGY_AUTO_APPROVE_LOG_DIR="$HOME/.gemini/antigravity-cli" agy-auto-approve logs
```

## Approval statistics

```bash
agy-auto-approve stats                 # All backend modes
agy-auto-approve stats --mode cli      # CLI only
agy-auto-approve stats --mode sidecar  # Desktop sidecar only
```

Output is a single English table with rows for **Last 24 hours**, **Last 7 days**,
and **Last 30 days**. Columns show Approvals, Input Tokens, Output Tokens, Total
Time, Avg Input, Avg Output, and Avg Time. Windows are inclusive at the start
and end and relative to one UTC timestamp captured when the query begins.

Only completed model reviews (`stage=reviewer`) count, including allow, deny,
ask, and force_ask decisions. Whitelist, blacklist, circuit-breaker, and failed
reviews are excluded. An approval's completion timestamp determines its window.
Time is the hook's measured duration, including queueing, initialization and
retries; concurrent approvals contribute their own durations independently.

Tokens include all backend calls associated with a counted approval, including
session initialization and retries. CLI cumulative input/output counters are
converted to per-call `usage_delta` in response events. The previous counters
and round number are atomically persisted alongside the reviewer conversation
ID in `reviewer_session.json`, so restarts preserve the baseline. Missing
baselines, skipped/duplicate rounds, counter resets, unsuccessful backend calls,
and unsupported usage formats are unknown, never zero. Sidecar usage is currently
unknown. Cache and thinking counters are not separately added to input/output.

If any counted approval has incomplete token data, that window's token totals
and averages display `N/A`; time and counts remain available. Missing durations
likewise display `N/A` for time. Empty windows show zero totals and `—` averages.
Token averages are rounded to the nearest integer; average time is shown to
one decimal second. Failed requests' costs remain in logs but are excluded from
this table, even when they consumed tokens.

Statistics use a 64 KiB buffered, line-by-line scan of dated files covering the
last 30 days plus the preceding day for boundary-crossing requests. File lengths
are snapshotted before scanning; readers do not hold locks while scanning. No
database, statistics cache, daemon, or full-log load is required. Malformed and
incomplete lines are skipped. The command never reads the legacy undated log,
and does not rotate or delete historical files. Memory holds only per-request
counters and completion metadata for the scanned dates.

## Hook

The host normally invokes this command automatically. It reads a PreToolUse JSON payload from stdin and emits one JSON approval result to stdout:

```bash
printf '%s\n' '{"toolCall":{"name":"view_file","args":{}},"workspacePaths":[]}' | agy-auto-approve hook
```

Pass `conversationId` (or `conversation_id`) to reuse an isolated reviewer session for that user conversation. Missing or blank IDs use temporary, nonpersistent review sessions. Same-user requests serialize; different users can be reviewed concurrently. The hook deadline is 28 seconds including queue waiting.

The payload must contain `toolCall.name` as a string and `toolCall.args` as an object. Input is limited to 1 MiB. Invalid input produces an `ask` result. Approval decisions are `allow`, `deny`, `ask`, or `force_ask`.

## Environment variables

| Variable | Purpose / default |
| --- | --- |
| `AGY_APPROVER_SOCKET` | Socket base; `~/.gemini/antigravity-cli/approver.sock`. Inserts `-cli` / `-sidecar` before `.sock`. |
| `AGY_APPROVER_STATE_DIR` | State base; `~/.gemini/antigravity-cli/state`. Appends `cli/` / `sidecar/`. |
| `AGY_AUTO_APPROVE_LOG_DIR` | Log directory; `~/.gemini/agy-auto-approve`. |
| `AGY_AUTO_APPROVE_SILENT` | Set to `1` to suppress approval notices on stderr; structured logs remain enabled. |
| `AGY_AUTO_APPROVE_PROMPT` | Override the reviewer prompt. |
| `AGY_AUTO_APPROVE_MODEL` | Override the sidecar model tier: `flash_lite`, `flash`, or `pro`. |
| `AGY_AUTO_APPROVE_CLI_MODEL` | Override the agy model ID; empty uses the host default. |
| `AGY_AUTO_APPROVE_REVIEWER` | Internal child-process marker: hooks deny all nested reviewer tool calls. |

When a nonempty log directory is explicitly configured and no nonempty state directory is set, state is stored in the log directory's `state/cli/` or `state/sidecar/` subdirectory.

## Configuration

See the [full configuration reference](configuration.md) for every plugin-managed configuration file, field descriptions, precedence, and examples.

```bash
agy-auto-approve config         # Show global settings, sources, and runtime paths
agy-auto-approve config --json  # Machine-readable settings (includes the full prompt)
agy-auto-approve config --edit  # Open the global TOML file
agy-auto-approve daemon restart # Apply settings in a new reviewer conversation
```

The global file is `~/.gemini/config/agy-auto-approve.toml`:

```toml
# Empty means the host's default model; otherwise flash_lite, flash, or pro.
model = ""
# CLI model ID from `agy models`; empty uses its default.
cli_model = ""
prompt = """Your reviewer instructions here."""
```

`config --edit` creates a template when absent, using the host default model
and the built-in prompt. It opens `$VISUAL`, then `$EDITOR`, falling
back to `vi`. Editor arguments are supported (for example `EDITOR='code --wait'`);
use a blocking editor so validation runs after saving. Invalid TOML, unknown keys,
and unsupported model values produce an error. Editing does not restart the daemon.

Precedence per setting: nonempty `AGY_AUTO_APPROVE_PROMPT` / `AGY_AUTO_APPROVE_MODEL` / `AGY_AUTO_APPROVE_CLI_MODEL`,
then the TOML field, then built-in defaults. An empty TOML model explicitly
selects the host default. An empty prompt is used literally.
Old global `~/.gemini/config/agy-auto-approve-*.txt` files and project
`.agents/agy-auto-approve-*.txt` files are not read or migrated. `--local`
is unsupported. `effort` and `AGY_AUTO_APPROVE_EFFORT` are no longer supported:
`agentapi` does not expose an effort option. Old full model identifiers such as
`gemini-3.7-flash` must be replaced with a supported tier for `model`; use `cli_model` for CLI model IDs.

Model tiers are passed as `agentapi new-conversation --model=<tier>` (supported
by agy 1.2.2). No model flag is passed when using the host default.
Settings are read when a reviewer conversation is created. Existing conversations
keep their configuration, including across ordinary daemon stops and starts.
There is no change detection or hot reload; use `daemon restart --mode cli` or `daemon restart --mode sidecar` to forget the
selected mode’s cached conversations. Environment overrides use the daemon's startup environment.
`config` shows settings resolved in the invoking process, not the active conversation.
Runtime paths (socket, state, logs) remain controlled by the environment variables above.

## Development checks

Run from the repository root:

```bash
cargo fmt --check
cargo clippy --all-targets --locked -- -D warnings
cargo test --locked
```

Integration tests use temporary home directories and mock `agentapi` and `agy` executables; no login or Python is required.
