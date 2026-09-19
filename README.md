# any-auto

Automatic approval for Antigravity CLI/Desktop and Pi. A shared Rust approval
pipeline supports agy CLI, agentapi, persistent Pi RPC, and OpenAI Responses
reviewers. Defaults need no configuration: agy uses its existing backend and Pi
uses a separate, tool-disabled Pi RPC session. One daemon serves all agents and instances with isolated reviewer
sessions; logs and statistics can be grouped across them.

| Agent | Default approver backend |
| --- | --- |
| Antigravity CLI | `cli` (agy CLI) |
| Antigravity Desktop | `cli` (agy CLI) |
| Pi | `pi` (persistent RPC) |

The agent is where approval requests originate. The approver backend decides them;
it can differ from the agent. Pi's model provider (such as Anthropic) is a separate
choice encoded in the model ID.

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

The daemon maintains a separate reviewer session for each user conversation, allowing the model service
to reuse KV/prompt caches for shared context. Cache hits can reduce repeated
processing and input-token costs, depending on the provider's caching and pricing.
Idle sessions leave memory after five minutes; Pi RPC children are terminated and reaped.
The next request restores persisted state. Each Pi session owns its own RPC process.
Repeated AI-review denials trip the circuit breaker, requiring user review on
subsequent requests. Decisions and reasons are logged locally. See the
[pipeline details](docs/sidecars.md).

## Install

Choose one way to install the binary.

Download a [GitHub Release](https://github.com/jjyr/any-auto/releases/latest)
(macOS or Linux, ARM64 or x86_64). Set the release tag and your platform target:

```bash
VERSION=v0.4.5
TARGET=aarch64-apple-darwin
curl -fLO "https://github.com/jjyr/any-auto/releases/download/$VERSION/any-auto-$VERSION-$TARGET.tar.gz"
tar -xzf "any-auto-$VERSION-$TARGET.tar.gz"
mkdir -p ~/.local/bin
mv any-auto ~/.local/bin/
```

Targets: `aarch64-apple-darwin`, `x86_64-apple-darwin`,
`aarch64-unknown-linux-musl`, `x86_64-unknown-linux-musl`.
Make sure `~/.local/bin` is on your `PATH`.

Or install from crates.io (requires Rust/Cargo and a C compiler):

```bash
cargo install any-auto --locked
```

Then select agent integrations in the terminal wizard:

```bash
any-auto install
# Scripts: any-auto install --auto
# Explicit selection: any-auto install --agents agy-cli,pi
```

For Pi (0.84.2 or newer):

```bash
any-auto install --pi
# Then /reload in Pi
```

See the [Pi extension/RPC research](docs/pi-research.md),
[configuration](docs/configuration.md), and [command reference](docs/commands.md).

## Terminal menu

Run `any-auto` without arguments to open agent readiness, installation,
configuration, logs and statistics. Explicit subcommands stay noninteractive,
except bare `install`, which opens its installation wizard.
Run `any-auto doctor` for local readiness checks without model requests.

## Configuration

```bash
any-auto config --agent pi        # View effective Pi reviewer settings
any-auto config --edit           # Edit common and per-agent reviewer settings
any-auto daemon reset --agent pi         # Reset Pi reviewer sessions
any-auto daemon status                  # Shared daemon and cached instances
```

Settings support common defaults, per-agent overrides, and environment overrides. See the
[configuration reference](docs/configuration.md) for provider, model, effort, and configuration precedence.

Default configuration: `~/.config/any-auto/config.toml`. No file is needed.
Use a common override or select a agent:

```toml
[agents.pi.approver]
provider = "pi"
model = "anthropic/claude-sonnet-4-5" # Example; must be available in your account
effort = "low"
```

`config` shows all agents and setting sources. Legacy configuration is not read.
Logs/stats and sessions start fresh; see [configuration and directory details](docs/configuration.md#overview-tui-and-directories).

## Commands

For all commands and options, see the [command reference](docs/commands.md). For more details, see the [multi-agent architecture](docs/architecture.md) and [sidecar documentation](docs/sidecars.md).

### Logs

```bash
any-auto logs                     # Recent approvals grouped by agent
any-auto logs --no-group          # Merged timeline
any-auto logs -f                  # Follow new approvals
any-auto logs --decision deny     # Show denied approvals
any-auto logs show APPROVAL_ID    # Show the full approval record
```

Logs are stored in `~/.local/share/any-auto/logs` and can be read without a running daemon.

Example output (`any-auto logs --limit 2`, rendered by the CLI from illustrative records;
the limit applies to each agent):

```text
agent: agy-cli
2026-09-19T13:11:47+00:00  18c4a2-12ab-0  allow      view_file  stage=whitelist agent=agy-cli provider=cli
  [any-auto: ALLOWED] Read-only tool.
2026-09-19T12:11:47+00:00  18c4a1-12ab-0  allow      run_command  stage=reviewer agent=agy-cli provider=cli
  [any-auto: ALLOWED] Requested local validation is low risk.
  command: cargo test
  cwd: /workspace/my-project
agent: pi
2026-09-16T14:11:47+00:00  18c3b2-34cd-0  ask        bash  stage=reviewer agent=pi provider=pi
  [any-auto: ASK] Confirm publishing this package.
  command: npm publish
  cwd: /workspace/my-project
2026-09-07T14:11:47+00:00  18c2c3-34cd-0  allow      bash  stage=reviewer agent=pi provider=pi
  [any-auto: ALLOWED] Requested local build is low risk.
  command: cargo build --release
  cwd: /workspace/my-project
```

### Approval statistics

Run `any-auto stats` for tables grouped by agent showing input/output tokens and approval time
(totals and averages) over the last 24 hours, 7 days, and 30 days. Use `--agent pi`, `--provider pi`, or `--group-by model` to select a view;
`--no-group` shows only totals. Statistics read daily UTC audit logs directly;
there is no database. Unknown token usage displays `N/A`. The usage tables count only completed model
reviews; see [statistics details](docs/commands.md#stats-usage-and-latency-aggregates).

Example output (`any-auto stats`, rendered by the CLI from the same illustrative records):

```text
agent: agy-cli
┌───────────────┬───────────┬──────────────┬───────────────┬────────────┬───────────┬────────────┬──────────┐
│ Period        │ Approvals │ Input Tokens │ Output Tokens │ Total Time │ Avg Input │ Avg Output │ Avg Time │
├───────────────┼───────────┼──────────────┼───────────────┼────────────┼───────────┼────────────┼──────────┤
│ Last 24 hours │         1 │        2,700 │           320 │       1.0s │     2,700 │        320 │     1.0s │
│ Last 7 days   │         1 │        2,700 │           320 │       1.0s │     2,700 │        320 │     1.0s │
│ Last 30 days  │         1 │        2,700 │           320 │       1.0s │     2,700 │        320 │     1.0s │
└───────────────┴───────────┴──────────────┴───────────────┴────────────┴───────────┴────────────┴──────────┘
agent: pi
┌───────────────┬───────────┬──────────────┬───────────────┬────────────┬───────────┬────────────┬──────────┐
│ Period        │ Approvals │ Input Tokens │ Output Tokens │ Total Time │ Avg Input │ Avg Output │ Avg Time │
├───────────────┼───────────┼──────────────┼───────────────┼────────────┼───────────┼────────────┼──────────┤
│ Last 24 hours │         0 │            0 │             0 │       0.0s │         — │          — │        — │
│ Last 7 days   │         1 │        1,800 │           240 │       1.4s │     1,800 │        240 │     1.4s │
│ Last 30 days  │         2 │        3,900 │           500 │       2.6s │     1,950 │        250 │     1.3s │
└───────────────┴───────────┴──────────────┴───────────────┴────────────┴───────────┴────────────┴──────────┘
```

## Repository layout

Agent plugin files live in [agy/](agy/README.md) and [pi/](pi/README.md).
`agy/` is the Antigravity plugin root; `pi/extensions/any-auto.ts` is the Pi
extension source. Shared Rust code remains in `src/`, tests in `tests/`, and
configuration/protocol documentation in `docs/`. The installer embeds the agent
files, so installed binaries do not need a source checkout.

## Releasing

See the [release guide](docs/releasing.md) for crates.io trusted publishing setup
and the tag-based CI release process.

## License

MIT
