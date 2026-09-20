# Pi extension and RPC research

Investigated 2026-09-19 against the official Pi repository and locally installed
`@earendil-works/pi-coding-agent` 0.84.2. This document separates agent integration
from the reviewer transport: an extension intercepts the user's Pi tool calls;
the Rust daemon owns a different, tool-disabled Pi RPC process for reviewing them.

## Official sources

- [Extensions](https://github.com/earendil-works/pi/blob/main/packages/coding-agent/docs/extensions.md)
- [RPC protocol](https://github.com/earendil-works/pi/blob/main/packages/coding-agent/docs/rpc.md)
- [CLI and configuration](https://github.com/earendil-works/pi/blob/main/packages/coding-agent/README.md)
- [Packages](https://github.com/earendil-works/pi/blob/main/packages/coding-agent/docs/packages.md)
- [Session format](https://github.com/earendil-works/pi/blob/main/packages/coding-agent/docs/session-format.md)
- [RPC implementation](https://github.com/earendil-works/pi/blob/main/packages/coding-agent/src/modes/rpc/rpc-mode.ts)
- [Thinking capability resolution](https://github.com/earendil-works/pi/blob/main/packages/ai/src/models.ts)
- [OpenAI reasoning effort](https://developers.openai.com/api/docs/guides/reasoning#reasoning-effort)

## Agent extension

Pi discovers global TypeScript extensions under `~/.pi/agent/extensions/*.ts`
and `extensions/*/index.ts`; project extensions require project trust. The agent
directory can be overridden by `PI_CODING_AGENT_DIR`. Explicit `-e` paths and
packages are also supported. Import types from `@earendil-works/pi-coding-agent`.

`pi.on("tool_call", async (event, ctx) => ...)` runs before execution. Inputs are
`toolName`, `toolCallId`, and mutable `input`. Returning `{block: true, reason}`
blocks the call; returning no blocking result allows subsequent handlers and
execution. All tools, including unknown custom tools, must be considered.
The current API also supports `terminate` on blocked results. In parallel tool
batches, termination does not necessarily stop unrelated permitted siblings.

Use `ctx.sessionManager.getSessionId()` for conversation identity, `ctx.cwd` for
workspace, and branch/fork/switch events to avoid carrying reviewer history into
an unrelated branch. `getBranch()` exposes active branch entries, not merely the
current tool; external tool content must never become reviewer instructions.

Pi has no built-in permission dialog equivalent to Antigravity's hook protocol.
The extension maps allow to continuation, deny to block, and ask/force_ask to
`ctx.ui.confirm()`. TUI and RPC expose UI; print/JSON do not. RPC dialogs depend
on the embedding client's extension UI protocol. Cancellation, unavailable UI,
invalid replies, and timeouts must not allow execution. A human confirmation is
recorded separately from the model's assessment.

`user_bash` handles manually entered `!` commands separately; it is outside this
agent-tool approval scope. Extensions execute with user permissions, can perform
their own I/O, and can mutate arguments in load order. This mechanism is not an
OS sandbox and cannot enforce policy against malicious installed extensions.

## Reviewer RPC

Start `pi --mode rpc` with piped stdin/stdout. Commands and responses use LF-only
JSONL framing (Unicode U+2028/U+2029 are valid within JSON strings). Each command
gets a unique `id`; correlate `type=response`, `command`, `id`, and `success`.
Most agent events have no request ID: permit only one prompt in flight per RPC
process. Drain stderr independently so a full pipe cannot deadlock the process.

`prompt` success acknowledges acceptance, NOT completion. Inspect final assistant
messages and terminal errors. Current upstream distinguishes `agent_end` (a low
level run can be followed by retries/compaction) from `agent_settled` (fully idle).
Version-dependent completion behavior must be tested against the installed Pi.
The implementation requires agent_settled (verified in installed 0.84.2), not an
agent_end fallback. It samples get_session_stats before/after each prompt to
include retries and compaction without double-counting message event copies.

Useful commands:

| Command | Purpose |
| --- | --- |
| `get_state` | Actual model, thinkingLevel, sessionId, sessionFile, streaming state |
| `get_available_thinking_levels` | Capability check for the selected model |
| `set_thinking_level` | Configure reasoning; check actual state afterwards |
| `prompt` | Send structured action as data, never a slash command |
| `clear_queue`, `abort` | Cancel queued work and current operation; abort acknowledges idle |
| `new_session`, `switch_session` | Session lifecycle, with cancellation results to inspect |

Each reviewer conversation owns its RPC child, isolated workspace and explicit
session file. Idle eviction or daemon shutdown kills and reaps the child. The shared daemon
checks every second and evicts inactive sessions after five minutes by default;
active and queued requests protect their session from eviction. During an
operation the child must be owned by the cancellable future: deadline cancellation
must kill it, not leave stale events for the next approval. Recovery starts a fresh
process and must never repeat a tool action (reviewers have no tools).

Launch with `--no-tools --no-extensions --no-skills --no-prompt-templates
--no-context-files --no-themes --no-approve` and a dedicated policy system prompt.
Disable startup network catalog refresh independently from actual model requests.
Pi 0.84.2 model/thinking RPC setters can persist global defaults (newer upstream
behavior differs). Use CLI startup model/thinking overrides followed by read-only
RPC capability/state checks. An offline local test confirmed settings.json stayed
byte-for-byte unchanged. Keep the original agent directory for native auth and
models.json; do not symlink auth.json into per-session directories: Pi locks with
realpath:false, so those symlinks would give concurrent refreshes different locks.
Do not put credentials in prompts, argv, audit records, or copied settings.

## Effort matrix

| Reviewer | Setting | Limits |
| --- | --- | --- |
| Pi RPC | `--thinking` plus RPC validation | off, minimal, low, medium, high, xhigh, max; query capabilities at runtime |
| agy CLI | `--effort` | low, medium, high according to installed CLI help; model acceptance still depends on agy |
| agentapi | No verified effort control | Explicit effort must error rather than be ignored |
| OpenAI Responses | `reasoning.effort` | none, minimal, low, medium, high, xhigh, max; supported subset depends on model |

Omission means backend default, not low or off. Pi non-reasoning models expose
only off. Pi may clamp unsupported values; explicitly requested unsupported
values should be rejected before prompting. `xhigh/max` require model capability
mapping. API `none` and Pi `off` denote backend-specific controls; do not assume
every model can disable reasoning. Record requested and effective effort
separately. High reasoning can exceed the synchronous approval deadline.

## Architecture

```text
agy hook --------+                         +-- agy CLI
desktop hook ----+--> shared Rust policy -->|-- agentapi
pi TS extension -+     daemon/session pool +-- Pi RPC child per session
                                          +-- OpenAI Responses
                           |
                  shared structured audit
                     /             \
                  logs            stats
```

Agent routing and reviewer provider are independent. One daemon/socket serves all
agents; state and reviewer sessions remain isolated by agent and instance. Logs are
chronological event records with optional grouping/filtering; stats aggregate
usage and latency by selected dimensions. These are different views of the same
audit data, not separate storage systems. No legacy log-schema compatibility is
required for the new implementation.

## RPC ownership and session switching

Official RPC supports `switch_session(sessionPath)`, but `prompt` targets the single
active session and has no target session parameter. Streaming steer/follow-up queues
belong to that session; they are not concurrent independent sessions. An offline Pi
0.84.2 check switched empty sessions A → B → A in one PID without model calls.

This application deliberately does not share RPC children across conversations.
One `(agent, instance, conversation)` owns one child while cached. Generic session-cache
eviction releases backend resources; Pi owns child cleanup, while agy/agentapi simply
release memory and retain their persisted conversation IDs. No `switch_session` RPC
is required by the implementation. Disk state survives eviction and is loaded on demand.

## User authorization context (2026-09-20)

The extension now reads role=user `SessionMessageEntry` values from the active
`getBranch()` path before each tool call. It forwards the latest user message,
prior user messages, entry IDs, and completeness status through the hook and
private daemon request to every backend. Message content may be a string or text
and image blocks; images, missing history, and budget limits mark evidence
incomplete rather than inventing authorization. Assistant and tool-result entries
are excluded. Compaction and branch summaries are not treated as user messages.

The entry and context types were checked against the installed Pi declarations
and the official [session format](https://github.com/earendil-works/pi/blob/main/packages/coding-agent/docs/session-format.md).
See [Jev context limits](jev.md#context-sent-for-review) for bounds and behavior.
