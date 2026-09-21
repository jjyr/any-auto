# Commands

## Install and update

```bash
any-auto install                 # Interactive agent selection (terminal required)
any-auto install --auto          # Install detected agents, no prompts
any-auto install --agents agy-cli,pi
any-auto install --dry-run       # Preview detected selection, no writes
any-auto install --cli-only
any-auto install --desktop-only
any-auto install --pi            # Pi extension only; requires Pi 0.84.2+
any-auto update
any-auto update --version v0.4.5
```

`install --pi` atomically writes `extensions/any-auto.ts` under
`PI_CODING_AGENT_DIR` or `~/.pi/agent`, with the absolute binary path. Other
extensions/settings are preserved. Run `/reload` in Pi afterwards. The three
legacy installation selectors are mutually exclusive; use `--agents` to select several agents.
Only bare `install` opens the TUI. Any arguments disable interaction.
The wizard defaults to detected agents, permits pre-installing undetected agents,
and asks before writing. Installation preserves existing approver settings and does not prompt for provider, model or effort. By default, both agy agents use agy CLI (`cli`) and Pi uses Pi RPC (`pi`). Use `any-auto config --edit` to customize approvers.
The multi-select lists both agent detection and integration installation status.
Up/Down moves, Space toggles, Enter continues, and Esc cancels. Detected agents
are preselected; installed integrations are updated only if selected. Selecting
none exits without changes, and unselected integrations are never removed.

Before confirmation, the installation summary shows the selected actions and
effective reviewer for each agent, distinguishing defaults, existing configuration,
and environment overrides. It explains automatic tool review, preserved reviewer
settings/credentials, and any development-command permission additions to existing
agy CLI settings. Invalid reviewer configuration fails before installation writes.
Completion messages list only the selected agents' reload/restart instructions.

Noninteractive installation preserves existing approver settings. Non-terminal bare calls fail with usage guidance. Update refreshes installed Pi integration as well as enabled agy
integrations and stops the shared daemon. All instances resume lazily on new requests. See [releasing](releasing.md) for distribution details.

## Routing and daemon lifecycle

```bash
any-auto daemon status
any-auto daemon status --agent pi --instance work
any-auto daemon start
any-auto daemon stop
any-auto daemon restart
any-auto daemon reset --agent pi --instance work
any-auto daemon run --idle-timeout 1800 --session-idle-timeout 300
```

One daemon and socket serve every agent and instance in the application runtime directory.
`--agent` selects the requesting agent; the old `--host` and `--mode` options are not supported. Hooks auto-detect Desktop
unless explicitly routed; the Pi extension always selects Pi. Instance defaults to
`default` (or `ANY_AUTO_INSTANCE`) for requests. Status lists all cached instances
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
any-auto logs                       # Groups by agent
any-auto logs --no-group            # Merged time order
any-auto logs --agent pi --limit 50
any-auto logs --provider pi --decision deny
any-auto logs --provider jev --decision ask
any-auto logs --tool bash --conversation SESSION_ID
any-auto logs --instance desktop-two
any-auto logs --group-by agent
any-auto logs --group-by provider --json
any-auto logs --group-by session
any-auto logs -f --agent pi
any-auto logs show APPROVAL_ID
```

`--provider` accepts pi, cli, openai, agentapi, and jev.
`--group-by` accepts agent, provider, model, effort, session, instance. Model and
effort in log summaries are requested settings; omitted settings are labeled
default/unknown. Detailed backend events record resolved model/effort when known.
`--json` returns an object of agent groups by default, or an array with --no-group. Follow emits
chronological records (JSONL with --json) and cannot be combined with grouping.
`--decision` accepts allow/deny/ask/force_ask. `--limit` defaults to 20.
`show` outputs correlated events including backend results and Pi human decisions.
A human confirmation is distinct from the model's ask/force_ask result.
A positive Pi confirmation clears the entire denial window for its pending
approval ID. A new validated user message also starts a fresh review window;
retries and agent/tool messages do not. For agy, a `PostToolUse` callback matching
the escalated conversation and step ends the pause. This callback records tool
completion, not a claim of human approval, and does not grant permission to future
actions. All subsequent actions are evaluated normally. Old or unrelated callbacks
cannot clear the window. Reset reasons are recorded as `circuit_breaker_reset`.
Existing installations must refresh hooks with `any-auto install --agents agy-cli`
(or the corresponding installed agents) to register `post-tool`.

Daily UTC `approvals-YYYY-MM-DD.jsonl` files are shared across agents/instances and
use cross-process file locks. New events use schema 3 and generic
backend_request/response/error names. No legacy-schema conversion is provided.
Logs work without a running daemon and may contain tool arguments and assessment
text. They do not log authentication headers or complete process environments.
File permissions are 0600. There is no automatic retention cleanup.

All backend detail records include the typed judgment, approval_checks,
failed_checks, and decision_policy_version. Without probabilities, probability
and threshold diagnostics are null; classification and completeness gates still
apply. With probabilities, the routine route requires P(low)+P(medium), while
the authorized route requires P(low)+P(medium)+P(high) and authorization
P(high)+P(medium). Both require permitted policy and P(permitted). The API's
confidence statistic is diagnostic only. See [shared policy](review-policy.md).


`reviewer_input.authorization_diagnostics` records received and normalized message
IDs, sources, counts and UTF-8 byte lengths, without message text. It reports the
configurable soft context budget (`approver.context_budget_bytes`, default 24576),
serialized common-input bytes before/after trimming, removed prior-message count,
and whether the retained input still exceeds the budget. It also reports whether
evidence changed or was already marked truncated upstream. These are lengths at the reviewer boundary, not the size of
an original transcript. For agy transcript collection, `hook_input` also includes
`authorization_collection`: collection limits, bytes read, selected counts and
any reached limit. A transcript read stopped at the size cap reports a lower bound;
a five-message window limit does not mean the entire transcript was inspected.
`jev_rubric` stores the effective questions (including custom instructions), and
all events include the binary's package version in `build_version`.

To capture the actual Jev request for a reproduction, enable `diagnostic_snapshot`
in `~/.config/any-auto/config.toml` (or the XDG config location). It defaults to
false, follows common/per-agent approver inheritance, and is supported by Jev.
Use `any-auto config --edit` to edit it and `any-auto config` to inspect the effective
value and source. Changes apply on the next review without restarting the daemon.

```toml
[approver]
provider = "jev"
diagnostic_snapshot = true

# Optional per-agent override:
[agents.pi.approver]
diagnostic_snapshot = false
```

Inspect the captured request with `any-auto logs show APPROVAL_ID`. Set the option
back to false when finished. There is no environment-variable override for this
setting.

The opt-in `jev_diagnostic_request` event contains the actual `model`, `state` and
`questions` body, including user authorization text and inspected script content.
`jev_diagnostic_response` contains the validated model and structured answers.
Transport credentials and authentication headers are not included. Secrets present
in the request body itself are preserved, so treat these snapshots as sensitive.
They use the same 0600 audit files and retention policy as other events. Disable the setting when finished.

## Reviewer evaluation

`any-auto reviewer-eval` runs maintained JSONL suites directly against the configured
Jev reviewer, without executing fixture actions or using production approval state.

```bash
any-auto reviewer-eval --agent agy-cli \
  --suite evals/suites/scenarios.jsonl --repeat 3 --output /tmp/baseline.json
any-auto reviewer-eval --agent agy-cli \
  --suite evals/suites/scenarios.jsonl --questions /tmp/candidate.json \
  --repeat 3 --compare /tmp/baseline.json --output /tmp/candidate-report.json
```

The default terminal summary shows result counts, total and per-scenario tokens
(input/output/combined), and elapsed time. Use --json for machine-readable stdout.
Full reports include probabilities, false approvals/rejections, service errors,
decision instability and baseline improvements/regressions. Missing token usage
is marked partial; available usage from retry responses is included. Calls are bounded by
`--max-calls`; retries default to zero. Suites live separately under `evals/`.
See [evaluation fixtures and report semantics](../evals/README.md).

## Stats: usage and latency aggregates

Stats answer **which agent/backend consumed the reviews and tokens?** Default output
is one table per agent, without a combined total or outcomes section. Each table contains rolling 5-minute, 24-hour, 7-day and
30-day windows, counts, input/output tokens, summed time, and averages.

```bash
any-auto stats
any-auto stats --no-group
any-auto stats --agent pi --group-by provider
any-auto stats --provider pi --group-by model
any-auto stats --provider jev --group-by model
any-auto stats --group-by effort
any-auto stats --group-by instance
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
counters are converted to deltas; agentapi usage is unknown. Jev reports per-request
input_tokens/output_tokens through the same events; missing usage remains unknown. Any unknown usage
makes that group's token totals/averages N/A, not zero. Unknown durations behave
likewise. Empty windows have zero totals and no average. Failed-review costs
remain in detailed logs but are excluded from these completed-review tables.

Readers snapshot daily file lengths and stream events without holding writer
locks throughout the scan. No database or background stats daemon is needed.

## Configuration and hook protocol

See [configuration](configuration.md) for model/provider/effort settings.

```bash
any-auto config --agent pi --json
any-auto config --edit
```

`hook` reads one JSON object (up to 1 MiB) from stdin and outputs one JSON decision.
The common payload has toolCall.name, toolCall.args, workspacePaths, and optional
conversationId. Pi additionally supplies builtin_tool, request_id, and bounded
user-origin authorization context. Jev is stateless remotely but shares local
routing and circuit breaker behavior; see [Jev setup](jev.md). Requests
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
any-auto                  # Terminal menu: readiness/install/config/logs/stats
any-auto doctor           # Read-only local detection; no model requests
any-auto config           # Effective settings and sources for every agent
```

The root invocation without arguments, bare `install`, and bare `uninstall` enter interactive mode.
Other subcommands produce terminal text/JSON or perform their explicit action.
Non-terminal interactive invocations fail with actionable guidance.
`doctor` separates agent detection, integration presence and local backend availability.
It does not verify login, model access or Pi version compatibility.
The terminal menu uses the same shared-daemon routing as CLI commands.
Configure approvers includes Jev with model, API URL, API API key,
and probability threshold fields, without an effort selector. Edit per-question
instructions through `config --edit`; `config --json` includes effective values
and their sources. doctor checks the selected Jev API key locally and sends
no API requests. Installation preserves these settings, and the update workflow
refreshes integrations without rewriting reviewer configuration.

Stats includes only completed model reviews; local rules and human confirmations remain available in logs. Usage must not be interpreted as total provider billing. No automatic log retention cleanup is enabled.

## Uninstall

```bash
any-auto uninstall
any-auto uninstall --agents agy-cli,pi
any-auto uninstall --all
any-auto uninstall --all --dry-run
```

Bare `uninstall` requires a terminal and lists installed integrations, including
orphaned any-auto desktop manifests. Nothing is selected by default. Up/Down
moves, Space toggles, Enter continues, and Esc cancels. Empty selection exits
without changes. The removal summary lists exact paths and requires confirmation,
which defaults to No. The root terminal menu also includes **Uninstall integrations**.

`--agents` accepts comma-separated or repeated selections and removes them without
prompting. `--all` selects all integrations and conflicts with `--agents`.
`--dry-run` previews only; without a selection it previews all integrations.
Already removed integrations are successful no-ops.

CLI removal deletes only the `any-auto` key from `~/.gemini/config/hooks.json`.
Desktop removal deletes `sidecars["any-auto/approver"]` from the shared config and
its two manifests after checking their name, executable, and arguments. Shared
JSON files and unrelated entries remain. Pi removal deletes `extensions/any-auto.ts`
under `PI_CODING_AGENT_DIR`, or `~/.pi/agent` by default.

All integration plans are validated before removal. Invalid JSON or a desktop
manifest whose any-auto ownership cannot be verified causes an error before
writes. Files changed after the preview also cause an error before writes.

Reviewer configuration, credentials, logs, history, persisted sessions, and the
any-auto executable are retained. Existing CLI command permissions are retained
because installation does not record which permissions were originally user-owned.
There is no purge option. The shared daemon is kept while integrations remain and
stopped after the last integration is removed; dry-run never stops it. A daemon
stop failure is reported separately from the completed integration removal.
Reload Pi or restart the selected Antigravity agents to unload their integrations;
an agent that has not reloaded may continue using the integration or restart the daemon.
