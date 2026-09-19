# Commands

## Install and update

```bash
agy-auto-approve install                 # Interactive agent selection (terminal required)
agy-auto-approve install --auto          # Install detected agents, no prompts
agy-auto-approve install --agents agy-cli,pi
agy-auto-approve install --dry-run       # Preview detected selection, no writes
agy-auto-approve install --cli-only
agy-auto-approve install --desktop-only
agy-auto-approve install --pi            # Pi extension only; requires Pi 0.84.2+
agy-auto-approve update
agy-auto-approve update --version v0.4.5
```

`install --pi` atomically writes `extensions/agy-auto-approve.ts` under
`PI_CODING_AGENT_DIR` or `~/.pi/agent`, with the absolute binary path. Other
extensions/settings are preserved. Run `/reload` in Pi afterwards. The three
legacy installation selectors are mutually exclusive; use `--agents` to select several agents.
Only bare `install` opens the TUI. Any arguments disable interaction.
The wizard defaults to detected agents, permits pre-installing undetected agents,
and asks before writing. The wizard optionally configures provider, model and effort before the final confirmation.
Noninteractive installation preserves existing approver settings. Non-terminal bare calls fail with usage guidance. Update refreshes installed Pi integration as well as enabled agy
integrations and stops the shared daemon. All instances resume lazily on new requests. See [releasing](releasing.md) for distribution details.

## Routing and daemon lifecycle

```bash
agy-auto-approve daemon status
agy-auto-approve daemon status --agent pi --instance work
agy-auto-approve daemon start
agy-auto-approve daemon stop
agy-auto-approve daemon restart
agy-auto-approve daemon reset --agent pi --instance work
agy-auto-approve daemon run --idle-timeout 1800 --session-idle-timeout 300
```

One daemon and socket serve every agent and instance in the application runtime directory.
`--agent` selects the requesting agent; the old `--host` and `--mode` options are not supported. Hooks auto-detect Desktop
unless explicitly routed; the Pi extension always selects Pi. Instance defaults to
`default` (or `AGY_AUTO_APPROVE_INSTANCE`) for requests. Status lists all cached instances
unless filtered; `status --all` is a compatibility alias for the full status object.
Start/stop/restart affect the entire daemon, even when routing options are supplied.

Sessions are keyed by agent, instance and conversation. Different conversations can run
concurrently with no fixed concurrency or session-count cap; a conversation serializes.
Each Pi approver conversation owns a separate RPC child. RPC children are never reused
across conversations. agy/agentapi commands run only for the duration of a request.

Every second, the daemon removes session objects whose last request finished at least
`--session-idle-timeout` seconds ago (default 300, minimum 1). Executing and queued
requests hold leases and cannot be evicted. Pi cleanup kills and reaps the child;
other backends release their memory records. Empty instance caches are also removed.
Session files and circuit breakers survive eviction; new requests restore state lazily.
Cancelled Pi turns retain the existing dirty-session recovery behavior.

Stop/start and restart preserve persisted reviewer sessions. `reset --agent AGENT
--instance NAME` clears one instance's reviewer cache and session files, preserving
circuit breakers. Reset rejects instances with active or queued reviews. It does not
interrupt other instances. Daemon idle exit defaults to 1800 seconds; zero disables it.

Status is read-only and exits with code 1 when stopped. Start is idempotent. Hooks start
an absent daemon when model review is needed. Stop/restart interrupt active reviews,
which fail closed. Request-local backend environment and configuration changes take
effect without restarting the shared daemon; incompatible reviewer generations reset.

## Logs: individual approval records

Logs answer **what happened to this action?** They default to groups by agent,
newest first within each group. `--no-group` provides one merged chronological
list. `--limit` applies per agent when grouping by agent. For other grouping dimensions
and `--no-group`, it applies globally before grouping. Grouping is not an aggregation of usage.

```bash
agy-auto-approve logs                       # Groups by agent
agy-auto-approve logs --no-group            # Merged time order
agy-auto-approve logs --agent pi --limit 50
agy-auto-approve logs --provider pi --decision deny
agy-auto-approve logs --tool bash --conversation SESSION_ID
agy-auto-approve logs --instance desktop-two
agy-auto-approve logs --group-by agent
agy-auto-approve logs --group-by provider --json
agy-auto-approve logs --group-by session
agy-auto-approve logs -f --agent pi
agy-auto-approve logs show APPROVAL_ID
```

`--group-by` accepts agent, provider, model, effort, session, instance. Model and
effort in log summaries are requested settings; omitted settings are labeled
default/unknown. Detailed backend events record resolved model/effort when known.
`--json` returns an object of agent groups by default, or an array with --no-group. Follow emits
chronological records (JSONL with --json) and cannot be combined with grouping.
`--decision` accepts allow/deny/ask/force_ask. `--limit` defaults to 20.
`show` outputs correlated events including backend results and Pi human decisions.
A human confirmation is distinct from the model's ask/force_ask result.
A positive Pi confirmation also clears that conversation's consecutive-denial
state so later actions can return to automatic review.

Daily UTC `approvals-YYYY-MM-DD.jsonl` files are shared across agents/instances and
use cross-process file locks. New events use schema 3 and generic
backend_request/response/error names. No legacy-schema conversion is provided.
Logs work without a running daemon and may contain tool arguments and assessment
text. They do not log authentication headers or complete process environments.
File permissions are 0600. There is no automatic retention cleanup.

## Stats: usage and latency aggregates

Stats answer **which agent/backend consumed the reviews and tokens?** Default output
is one table per agent plus Total. Each table contains rolling 24-hour, 7-day and
30-day windows, counts, input/output tokens, summed time, and averages.

```bash
agy-auto-approve stats
agy-auto-approve stats --no-group
agy-auto-approve stats --agent pi --group-by provider
agy-auto-approve stats --provider pi --group-by model
agy-auto-approve stats --group-by effort
agy-auto-approve stats --group-by instance
```

Grouping accepts the same dimensions as logs. Stats model grouping uses backend
resolved model when available, otherwise the configured model. Effort grouping
uses requested effort; effective effort is separately available in detailed logs.
Labels missing from the backend are default/unknown, never invented.

Only completed model reviews count (allow/deny/ask/force_ask). Read-only fast
paths, blacklist, breaker and failed reviews are excluded. Initialization/retry
calls belong to the same approval. Completion time determines the window;
duration includes queueing and review, not subsequent human dialog time.

Pi takes get_session_stats deltas around each settled prompt, including retries
and compaction without counting duplicated lifecycle events. Its input is
input + cacheRead + cacheWrite. OpenAI input_tokens already includes
cached input; cached/reasoning detail counters are not added again. agy cumulative
counters are converted to deltas; agentapi usage is unknown. Any unknown usage
makes that group's token totals/averages N/A, not zero. Unknown durations behave
likewise. Empty windows have zero totals and no average. Failed-review costs
remain in detailed logs but are excluded from these completed-review tables.

Readers snapshot daily file lengths and stream events without holding writer
locks throughout the scan. No database or background stats daemon is needed.

## Configuration and hook protocol

See [configuration](configuration.md) for model/provider/effort settings.

```bash
agy-auto-approve config --agent pi --json
agy-auto-approve config --edit
```

`hook` reads one JSON object (up to 1 MiB) from stdin and outputs one JSON decision.
The common payload has toolCall.name, toolCall.args, workspacePaths, and optional
conversationId. Pi additionally supplies builtin_tool and request_id. Requests
without conversation identity use temporary sessions. The deadline is 28 seconds.
Pi normalizes bash command arguments and trusts read-only shortcuts only for
reported built-in tools. Unknown tools are model-reviewed. `human-result` is an
internal extension endpoint for recording the final confirmation result.

## Development

```bash
cargo fmt --check
cargo clippy --all-targets --locked -- -D warnings
cargo test --locked
node --test tests/pi-extension.test.mjs  # Node >= 22.18
```

Tests use isolated homes, fake executables and local API fixtures; no model login
or paid request is needed. [Pi research](pi-research.md) documents the RPC contract.

## Terminal menu and diagnostics

```bash
agy-auto-approve                  # Terminal menu: readiness/install/config/logs/stats
agy-auto-approve doctor           # Read-only local detection; no model requests
agy-auto-approve config           # Effective settings and sources for every agent
```

Only the root invocation without arguments and bare `install` enter interactive mode.
Other subcommands produce terminal text/JSON or perform their explicit action.
Non-terminal interactive invocations fail with actionable guidance.
`doctor` separates agent detection, integration presence and local backend availability.
It does not verify login, model access or Pi version compatibility.
The terminal menu uses the same shared-daemon routing as CLI commands.

Stats also prints a separate outcomes table with the same rolling windows and filters.
Rows distinguish pipeline stage/decision and human confirmations. Human confirmations
are separate events, not extra model reviews. Model usage excludes failed reviews and
must not be interpreted as total provider billing. No automatic log retention cleanup is enabled.
