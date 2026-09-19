# agy-auto-approve

Automatic approval for Antigravity CLI/Desktop and Pi. A shared Rust approval
pipeline supports agy CLI, agentapi, persistent Pi RPC, and OpenAI Responses
reviewers. Defaults need no configuration: agy uses its existing backend and Pi
uses a separate, tool-disabled Pi RPC session. Each host has independent daemons
and sessions; logs and statistics can be grouped across them.

Reviewer subprocesses preserve the host's `PATH` and append
`$HOME/.gemini/antigravity-cli/bin` as a fallback for CLI launches. Host-injected
commands take priority. This lookup works on both macOS and Linux.

## How it works

Antigravity CLI, Desktop, and Pi share the same approval pipeline. Local rules handle allowlisted
read-only tools and blocked commands; other requests go to an AI reviewer that
assesses risk and user authorization.

```text
Antigravity CLI / Desktop / Pi
           |
     Approval hook
           |
     Read-only tool? -------- yes ------> Allow
           | no
     Blocklisted command? -- yes ------> Deny
           | no
     Circuit breaker open? - yes ------> Ask user
           | no
     Persistent daemon
           |
     Configured reviewer     -------------> Allow / Deny
           |
     Error or timeout -----------------> Deny
```

Each daemon reuses a separate reviewer session for each user conversation, allowing the model service
to reuse KV/prompt caches for shared context. Cache hits can reduce repeated
processing and input-token costs, depending on the provider's caching and pricing.
Repeated AI-review denials trip the circuit breaker, requiring user review on
subsequent requests. Decisions and reasons are logged locally. See the
[pipeline details](docs/sidecars.md).

## Install

Choose one way to install the binary.

Download a [GitHub Release](https://github.com/jjyr/agy-auto-approve/releases/latest)
(macOS or Linux, ARM64 or x86_64). Set the release tag and your platform target:

```bash
VERSION=v0.4.2
TARGET=aarch64-apple-darwin
curl -fLO "https://github.com/jjyr/agy-auto-approve/releases/download/$VERSION/agy-auto-approve-$VERSION-$TARGET.tar.gz"
tar -xzf "agy-auto-approve-$VERSION-$TARGET.tar.gz"
mkdir -p ~/.local/bin
mv agy-auto-approve ~/.local/bin/
```

Targets: `aarch64-apple-darwin`, `x86_64-apple-darwin`,
`aarch64-unknown-linux-musl`, `x86_64-unknown-linux-musl`.
Make sure `~/.local/bin` is on your `PATH`.

Or install from crates.io (requires Rust/Cargo and a C compiler):

```bash
cargo install agy-auto-approve --locked
```

Then select host integrations in the terminal wizard:

```bash
agy-auto-approve install
# Scripts: agy-auto-approve install --auto
# Explicit selection: agy-auto-approve install --hosts agy-cli,pi
```

For Pi (0.84.2 or newer):

```bash
agy-auto-approve install --pi
# Then /reload in Pi
```

See the [Pi extension/RPC research](docs/pi-research.md),
[configuration](docs/configuration.md), and [command reference](docs/commands.md).

## Configuration

```bash
agy-auto-approve config --host pi        # View effective Pi reviewer settings
agy-auto-approve config --edit           # Edit common and per-host reviewer settings
agy-auto-approve daemon restart --host pi       # Restart Pi reviewer sessions
agy-auto-approve daemon status --all            # Inspect all daemon instances
```

Settings support common defaults, per-host overrides, and environment overrides. See the
[configuration reference](docs/configuration.md) for provider, model, effort, and configuration precedence.

## Commands

For all commands and options, see the [command reference](docs/commands.md). For more details, see the [Codex Guardian background research](docs/auto_approver_architecture.md) and [sidecar documentation](docs/sidecars.md).

### Logs

```bash
agy-auto-approve logs                     # Recent approvals grouped by host
agy-auto-approve logs --no-group          # Merged timeline
agy-auto-approve logs -f                  # Follow new approvals
agy-auto-approve logs --decision deny     # Show denied approvals
agy-auto-approve logs show APPROVAL_ID    # Show the full approval record
```

Logs are stored in `~/.gemini/agy-auto-approve` and can be read without a running daemon.

Example output (`agy-auto-approve logs --limit 2`, illustrative data):

```text
host: agy-cli
2026-09-18T04:22:07.302579+00:00  18c4a1-12ab-0  allow      run_command  stage=reviewer host=agy-cli provider=cli
  [agy-auto-approve: ALLOWED] Requested local validation is low risk.
  command: cargo test
  cwd: /workspace/my-project
2026-09-15T04:22:07.302579+00:00  18c3b2-12ab-0  allow      run_command  stage=reviewer host=agy-cli provider=cli
  [agy-auto-approve: ALLOWED] Requested local validation is low risk.
  command: cargo build --release
  cwd: /workspace/my-project
```

### Approval statistics

Run `agy-auto-approve stats` for tables grouped by host plus a total of input/output tokens and approval time
(totals and averages) over the last 24 hours, 7 days, and 30 days. Use `--host pi`, `--provider pi`, or `--group-by model` to select a view;
`--no-group` shows only totals. Statistics read daily UTC audit logs directly;
there is no database. Unknown token usage displays `N/A`. Only completed model
reviews count; see [statistics details](docs/commands.md#stats-usage-and-latency-aggregates).

Example output (illustrative data):

```bash
agy-auto-approve stats --no-group
```

```text
┌───────────────┬───────────┬──────────────┬───────────────┬────────────┬───────────┬────────────┬──────────┐
│ Period        │ Approvals │ Input Tokens │ Output Tokens │ Total Time │ Avg Input │ Avg Output │ Avg Time │
├───────────────┼───────────┼──────────────┼───────────────┼────────────┼───────────┼────────────┼──────────┤
│ Last 24 hours │         1 │        2,700 │           320 │       1.0s │     2,700 │        320 │     1.0s │
│ Last 7 days   │         2 │        5,600 │           620 │       2.4s │     2,800 │        310 │     1.2s │
│ Last 30 days  │         3 │        8,700 │           920 │       4.2s │     2,900 │        307 │     1.4s │
└───────────────┴───────────┴──────────────┴───────────────┴────────────┴───────────┴────────────┴──────────┘
```

## Releasing

See the [release guide](docs/releasing.md) for crates.io trusted publishing setup
and the tag-based CI release process.

## License

MIT
