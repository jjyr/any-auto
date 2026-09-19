# Commands

## Install and update

```bash
agy-auto-approve install                 # Interactive host selection (terminal required)
agy-auto-approve install --auto          # Install detected hosts, no prompts
agy-auto-approve install --hosts agy-cli,pi
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
legacy installation selectors are mutually exclusive; use `--hosts` to select several hosts.
Only bare `install` opens the TUI. Any arguments disable interaction.
The wizard defaults to detected hosts, permits pre-installing undetected hosts,
and asks before writing. Existing approver settings are preserved; use `config --edit`
to configure provider, model and effort. Non-terminal bare calls fail with usage guidance. Update refreshes installed Pi integration as well as enabled agy
integrations and stops default host daemons. Nondefault instances need an explicit
restart. See [releasing](releasing.md) for distribution details.

## Routing and daemon lifecycle

```bash
agy-auto-approve daemon status --all
agy-auto-approve daemon status --host pi
agy-auto-approve daemon start --host agy-cli
agy-auto-approve daemon stop --host pi
agy-auto-approve daemon restart --host pi
agy-auto-approve daemon run --host pi --idle-timeout 1800
agy-auto-approve daemon status --host agy-desktop --instance desktop-two
```

`--host` aliases `--mode`; cli/agy-cli, sidecar/agy-desktop, and pi are accepted.
Hooks auto-detect Desktop's connection environment unless a host is specified.
Other operational commands default to agy CLI. Pi extension always selects pi.
Logs/stats cover all hosts unless filtered. `--instance` (or `AGY_AUTO_APPROVE_INSTANCE` for operations) defaults to default for
operations, and all instances for queries when omitted.

Each host/instance owns its socket, lock, persisted state and session pool.
Different conversations can run concurrently (four model reviews per daemon);
one conversation serializes. Up to 32 sessions are cached, with idle eviction
and a five-minute idle TTL. Each Pi session owns one reusable RPC child. Eviction
kills the child and retains clean session files; daemon restart clears the selected
instance's reviewer sessions but keeps circuit breakers. Stop/start retains clean
sessions. A cancelled/failed RPC turn marks its session dirty and resets it on
recovery. The default daemon idle timeout is 30 minutes; zero disables it.

Status is read-only and exits with code 1 when stopped. Start is idempotent;
hooks start a missing daemon when model review is needed. Restart/stop can
interrupt active reviews, which fail closed. Configuration changes are detected
before review; environment changes require restarting the affected daemon.

## Logs: individual approval records

Logs answer **what happened to this action?** They default to groups by host,
newest first within each group. `--no-group` provides one merged chronological
list. Grouping rearranges the most recent matching records; limit
applies before grouping. Grouping is not an aggregation of usage.

```bash
agy-auto-approve logs                       # Groups by host
agy-auto-approve logs --no-group            # Merged time order
agy-auto-approve logs --host pi --limit 50
agy-auto-approve logs --provider pi --decision deny
agy-auto-approve logs --tool bash --conversation SESSION_ID
agy-auto-approve logs --instance desktop-two
agy-auto-approve logs --group-by host
agy-auto-approve logs --group-by provider --json
agy-auto-approve logs --group-by session
agy-auto-approve logs -f --host pi
agy-auto-approve logs show APPROVAL_ID
```

`--group-by` accepts host, provider, model, effort, session, instance. Model and
effort in log summaries are requested settings; omitted settings are labeled
default/unknown. Detailed backend events record resolved model/effort when known.
`--json` returns an object of host groups by default, or an array with --no-group. Follow emits
chronological records (JSONL with --json) and cannot be combined with grouping.
`--decision` accepts allow/deny/ask/force_ask. `--limit` defaults to 20.
`show` outputs correlated events including backend results and Pi human decisions.
A human confirmation is distinct from the model's ask/force_ask result.
A positive Pi confirmation also clears that conversation's consecutive-denial
state so later actions can return to automatic review.

Daily UTC `approvals-YYYY-MM-DD.jsonl` files are shared across hosts/instances and
use cross-process file locks. New events use schema 3 and generic
backend_request/response/error names. No legacy-schema conversion is provided.
Logs work without a running daemon and may contain tool arguments and assessment
text. They do not log authentication headers or complete process environments.
File permissions are 0600. There is no automatic retention cleanup.

## Stats: usage and latency aggregates

Stats answer **which host/backend consumed the reviews and tokens?** Default output
is one table per host plus Total. Each table contains rolling 24-hour, 7-day and
30-day windows, counts, input/output tokens, summed time, and averages.

```bash
agy-auto-approve stats
agy-auto-approve stats --no-group
agy-auto-approve stats --host pi --group-by provider
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
agy-auto-approve config --host pi --json
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
