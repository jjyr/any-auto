# Multi-agent approval architecture

Agent adapters normalize approval requests into one shared policy pipeline. Local rules
handle recognized safe operations and blocked commands. Requests requiring model review
are routed through one shared daemon to the matching agent/instance session and configured backend.

```text
agy CLI hook       Desktop hook       Pi extension
      |                  |                  |
      +------------------+------------------+
                         |
                shared approval pipeline
                         |
                single daemon / shared Unix socket
                         |
                approver backend interface
               /         |        |        \
           agy CLI    agentapi   Pi RPC   OpenAI Responses
                         |
                 shared audit JSONL files
                    /          \
               logs by agent   stats by agent

CLI commands / root TUI
            |
shared config resolution, installation and query functions
```

Agent, approver backend and model provider are independent concepts. Reviewer sessions
are isolated by agent, instance and conversation, and reset when effective configuration
changes. A single daemon dynamically routes all agents and instances. Each approver conversation
owns its backend; Pi RPC children are never shared across conversations. A request lease
protects both executing and queued requests. On lease release (including cancellation),
the session's last_active timestamp is updated. Every second the daemon evicts sessions
idle for five minutes and removes empty instance caches. Persistent session files and
breaker history survive eviction; the next request restores them lazily. There is no fixed
session-count or model-concurrency limit. Same-conversation requests remain serialized.

The daemon process starts/stops/restarts globally. Instance reset clears reviewer sessions
for one agent/instance without stopping other instances. Requests carry a bounded backend
environment context over the private socket; no per-request mutation of process environment
is permitted. Context values are not included in audit events. Backend child processes
receive the caller's relevant environment explicitly instead of using the first starter's
credentials or Desktop connection. Changes to context invalidate incompatible reviewer sessions.

Configuration and audit data live under application-owned XDG directories. Plugin sources are grouped under `agy/` and `pi/`, and embedded in the binary.
Only installed integration files belong in `.gemini` or Pi's extension directory. Installation previews mutations and
validates selected agy JSON configurations before writing. Individual files are atomically
replaced; installation is not a cross-file transaction.

Logs describe individual approvals; their default limit is per agent. Stats separates
completed model-review usage from pipeline outcomes and human confirmations. A human
confirmation is linked to its approval ID and never counted as another model review.

See [configuration](configuration.md), [commands](commands.md), and
[Pi extension/RPC contract research](pi-research.md). The
[Codex Guardian document](auto_approver_architecture.md) is background research, not
an implementation description of this application.
