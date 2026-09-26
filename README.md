# any-auto

[![crates.io](https://img.shields.io/crates/v/any-auto.svg)](https://crates.io/crates/any-auto)
[![CI](https://github.com/jjyr/any-auto/actions/workflows/ci.yml/badge.svg)](https://github.com/jjyr/any-auto/actions/workflows/ci.yml)

Auto-approve for Antigravity (agy) and Pi, powered by the AI backend of your choice.

```text
 Coding agents           Auto-approve           Reviewer backends

+-------------------+    +--------------+    +---------------------------+
| Antigravity       |--->|              |--->| Jev: System One           |
|                   |    | any-auto     |--->| CLI: agy or Pi            |
| Pi                |--->|              |--->| API: OpenAI Responses     |
+-------------------+    +--------------+    +---------------------------+
```

Automatically review tool requests and approve them when appropriate, so your
agent can keep working with fewer interruptions.

Choose your reviewer backend independently of your coding agent: use Pi to review
agy requests, agy to review Pi requests, or an API backend for either.

## How it works

Antigravity CLI, Desktop, and Pi share the same approval pipeline. Local rules handle allowlisted
read-only tools and blocked commands; other requests go to an AI reviewer that
assesses risk and user authorization.

```text
Antigravity / Pi
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
     Configured reviewer: Pi / agy / Jev
           |
     Typed judgment (risk, authorization, policy)
           |
     Shared policy + optional probability gate --> Allow / Deny
           |
     Error or timeout -----------------> Deny
```

Pi/agy use the [conversational prompt](src/prompts/prompt.txt); Jev uses
[Choice questions](src/prompts/questions.json).
Models supply classifications; [the shared policy](src/policy/mod.rs) computes
the final outcome. Pi/agy JSON is parsed into typed enums, not accepted as a model
allow/deny decision. Jev supplies probability distributions; other reviewers do
not need them. See [the contract and migration notes](docs/review-policy.md).

Conversational backends maintain a separate reviewer session for each user conversation, allowing the model service
to reuse KV/prompt caches for shared context. Cache hits can reduce repeated
processing and input-token costs, depending on the provider's caching and pricing.

Idle sessions leave memory after five minutes; Pi RPC children are terminated and reaped.
The next request restores persisted state. Each Pi session owns its own RPC process.
Repeated AI-review denials trip the circuit breaker, denying further reviewed
actions until a new validated user message starts a fresh review window. Decisions and reasons are logged locally. See the
[pipeline details](docs/sidecars.md).

## Install

Choose one way to install the binary.

Download a [GitHub Release](https://github.com/jjyr/any-auto/releases/latest)
(macOS or Linux, ARM64 or x86_64). Set the release tag and your platform target:

```bash
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

### OpenAI-compatible local server

OpenAI settings use nested tables; sampling parameters are optional:

```toml
[agents.pi.approver]
provider = "openai"
request_timeout = 20

[agents.pi.approver.openai]
model = "local-model"
base_url = "http://localhost:8080/v1"
api_key = "local"

[agents.pi.approver.openai.common]
temperature = 0.6
max_output_tokens = 2048

[agents.pi.approver.openai.llama_cpp]
reasoning_budget_tokens = 128
```

Use your server's model ID and credential. The thinking budget requires llama.cpp
and template support; these example values have not been evaluated for approval
accuracy. For the hosted OpenAI API, use its URL/model/key and omit `llama_cpp`.
See [all fields and migration from flat configuration](docs/configuration.md#openai-configuration-structure).

### Jev

Switching to Jev can substantially reduce auto-approval latency and token usage
compared with conversational reviewers. Actual savings depend on your previous
backend and the requests being reviewed.

Add to `~/.config/any-auto/config.toml` (open with `any-auto config --edit`):

```toml
[approver]
provider = "jev"
base_url = "https://api.typesafe.ai/v1"
api_key = "your-api-key"
model = "jev-1.13.0"
probability_threshold = 0.85
```

To configure only one agent, replace `[approver]` above with its table name:

- Pi: `[agents.pi.approver]`
- Antigravity CLI (agy): `[agents.agy-cli.approver]`
- Antigravity Desktop: `[agents.agy-desktop.approver]`

Keep the same fields under the selected table. To configure both Antigravity CLI
and Desktop, add both tables with the desired settings. See [Jev setup](docs/jev.md)
for configuration overrides and custom instructions.

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
(totals and averages) over the last 5 minutes, 24 hours, 7 days, and 30 days. Use `--agent pi`, `--provider pi`, or `--group-by model` to select a view;
`--no-group` shows only totals. Statistics read daily UTC audit logs directly;
there is no database. Unknown token usage displays `N/A`. The usage tables count only completed model
reviews; see [statistics details](docs/commands.md#stats-usage-and-latency-aggregates).

Example output from local usage (`any-auto stats`, captured on 2026-09-20):

```text
agent: agy-cli
┌───────────────┬───────────┬──────────────┬───────────────┬────────────┬───────────┬────────────┬──────────┐
│ Period        │ Approvals │ Input Tokens │ Output Tokens │ Total Time │ Avg Input │ Avg Output │ Avg Time │
├───────────────┼───────────┼──────────────┼───────────────┼────────────┼───────────┼────────────┼──────────┤
│ Last 5 min    │         3 │        3,542 │           423 │       2.7s │     1,181 │        141 │     0.9s │
│ Last 24 hours │        15 │      160,789 │         5,075 │     2m 19s │    10,719 │        338 │     9.2s │
│ Last 7 days   │        15 │      160,789 │         5,075 │     2m 19s │    10,719 │        338 │     9.2s │
│ Last 30 days  │        15 │      160,789 │         5,075 │     2m 19s │    10,719 │        338 │     9.2s │
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
