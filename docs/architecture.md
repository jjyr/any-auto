# Multi-host approval architecture

Host adapters normalize approval requests into one shared policy pipeline. Local rules
handle recognized safe operations and blocked commands. Requests requiring model review
are routed to the host/instance daemon and configured approver backend.

```text
agy CLI hook       Desktop hook       Pi extension
      |                  |                  |
      +------------------+------------------+
                         |
                shared approval pipeline
                         |
                daemon (host, instance)
                         |
                approver backend interface
               /         |        |        \
           agy CLI    agentapi   Pi RPC   OpenAI Responses
                         |
                 shared audit JSONL files
                    /          \
               logs by host   stats by host + Total

CLI commands / root TUI
            |
shared config resolution, installation and query functions
```

Host, approver backend and model provider are independent concepts. Reviewer sessions
are isolated by host, instance and conversation, and reset when effective configuration
changes. Existing daemon lifecycle and commands remain unchanged.

Configuration and audit data live under application-owned XDG directories. Only integration
files belong in `.gemini` or Pi's extension directory. Installation previews mutations and
validates selected agy JSON configurations before writing. Individual files are atomically
replaced; installation is not a cross-file transaction.

Logs describe individual approvals; their default limit is per host. Stats separates
completed model-review usage from pipeline outcomes and human confirmations. A human
confirmation is linked to its approval ID and never counted as another model review.

See [configuration](configuration.md), [commands](commands.md), and
[Pi extension/RPC contract research](pi-research.md). The
[Codex Guardian document](auto_approver_architecture.md) is background research, not
an implementation description of this application.
