# Configuration

The optional global file is `~/.config/agy-auto-approve/config.toml`. No file is
needed for the defaults. Unknown fields, malformed TOML, invalid providers and
unsupported effort values are errors. Configuration is not read from projects.

## Defaults and overrides

| Agent (`--agent`) | Default approver provider | Authentication |
| --- | --- | --- |
| `agy-cli` | `cli` — agy executable | Existing agy login |
| `agy-desktop` | `agentapi` | Desktop connection environment and login |
| `pi` | `pi` — persistent Pi RPC | Existing Pi authentication or provider environment |

Agent and provider are independent: an agy hook can use Pi RPC, and a Pi extension
can use agy CLI or OpenAI. `agentapi` requires a Desktop connection even when
selected by another agent. Failures never silently switch providers.

```toml
# Optional common override. Omit all settings to keep agent-specific defaults.
[approver]
provider = "pi"
model = "anthropic/claude-sonnet-4-5"
effort = "low"

# Optional per-agent override. Also available: agents.agy-cli, agents.agy-desktop.
[agents.pi.approver]
provider = "openai"
model = "gpt-5.5"
effort = "low"
# base_url = "https://api.openai.com/v1"
# api_key_env = "OPENAI_API_KEY"
```

The example models illustrate syntax; they are not application defaults. Models
must be available in the chosen backend/account. Pi supports provider/model IDs;
its provider prefix is distinct from this application's `provider = "pi"`.

All approver fields are optional except `model` when using OpenAI. Omitted model
or effort uses the backend default. A blank model also selects the default.
The optional top-level `prompt` replaces the built-in policy, including when it
is explicitly empty. Do not put secrets in prompts or model names.

Resolution: built-in defaults, common `[approver]`, matching agent override,
then nonempty environment overrides. Switching provider in an override resets
inherited model, effort, base URL, and key environment variable. Same-provider
overrides merge fields. Only the selected agent's effective settings are validated,
apart from the retained legacy `model` tier validation.

Legacy `model` (agentapi tier), `cli_model` (agy model), and `prompt` remain accepted.
New `approver.model` takes precedence over the corresponding legacy model.

## Effort support

| Provider | Control | Accepted values and limitations |
| --- | --- | --- |
| `pi` | Startup --thinking plus RPC capability/state checks | off, minimal, low, medium, high, xhigh, max; explicitly requested levels must be in `get_available_thinking_levels` and match `get_state` afterwards |
| `cli` | `agy --effort` on each turn | low, medium, high; final model support is determined by agy |
| `agentapi` | No verified control | Explicit effort is rejected |
| `openai` | Responses `reasoning.effort` | none, minimal, low, medium, high, xhigh, max; supported subset depends on model; API errors fail closed |

Pi non-reasoning models only support off. xhigh/max require model capability
mappings. No effort is sent when omitted. `off` is Pi syntax, `none` is OpenAI
syntax; they are not universally supported. There is no silent fallback to a
lower effort. The 20-second backend and 28-second hook deadlines still apply.
See [official research and sources](pi-research.md).

## Pi runtime and credentials

Requires Pi 0.84.2 or newer with `agent_settled` and
`get_available_thinking_levels`. Reviewers use isolated workspaces and explicit session files, disable
tools/extensions/skills/context files/templates and startup catalog network refresh.
They wait for agent_settled, including any automatic retries or compaction, within
the overall deadline.

The selected source agent directory is `PI_CODING_AGENT_DIR` or `~/.pi/agent`.
Reviewers use this original directory for authentication, preserving Pi's native
OAuth refresh lock identity. There are no credential copies or symlinks. Model
and effort are supplied as startup flags and checked with read-only RPC commands;
we do not call model/thinking setters that can persist defaults in Pi 0.84.2.
An offline startup smoke test verified that the source settings file remained
byte-for-byte unchanged with explicit model/thinking overrides. Pi may perform
its normal authentication refresh and configuration bookkeeping.
Extension-defined providers are unavailable in the tool-disabled reviewer; use
built-in providers or models.json. Environment API credentials are inherited.

## OpenAI runtime

`openai` uses the Responses API, not Chat Completions. An arbitrary endpoint
advertising OpenAI compatibility may not support it. `base_url` defaults to
`https://api.openai.com/v1`, and `api_key_env` to `OPENAI_API_KEY`. Credentials are
read from that environment variable, not TOML, command arguments, or logs.
HTTPS is required except for local test servers. Redirects are disabled.

Requests contain no tools. Conversations use `previous_response_id` and
`store=true`; the provider retains response state according to its policies.
The policy is supplied on every request. Failed cached conversations are retried
once with fresh state within the overall deadline. Unsupported model/effort,
refusals, incomplete responses and invalid assessments fail closed.

## Applying changes

```bash
agy-auto-approve config --agent pi --json
agy-auto-approve config --edit
agy-auto-approve daemon reset --agent pi
```

Configuration is resolved before each model review. A changed effective
provider/model/effort/prompt creates a new reviewer generation; persisted session
IDs from another configuration are not reused. Circuit breaker history survives.
Hooks send relevant backend environment overrides and connection credentials through
the private socket. The shared daemon scopes these to each request and backend child,
never mutating its global environment or recording credentials in audit events.
Changing the request context resets incompatible reviewer state. Pi external defaults
are resolved at RPC startup; reset its instance to pick up changes immediately.
`config` shows the invoking process's effective settings.

## Paths and environment

Application configuration and data are independent of agent installation directories.
`XDG_CONFIG_HOME` and `XDG_DATA_HOME` override the defaults when absolute.
Pi users do not need agy installed.

| Variable | Purpose |
| --- | --- |
| `AGY_AUTO_APPROVE_INSTANCE` | Operational instance name when --instance is omitted |
| `AGY_AUTO_APPROVE_PROVIDER` | pi, cli, openai, agentapi |
| `AGY_AUTO_APPROVE_APPROVER_MODEL` | Model for the selected provider |
| `AGY_AUTO_APPROVE_EFFORT` | Backend-specific effort |
| `AGY_AUTO_APPROVE_MODEL`, `AGY_AUTO_APPROVE_CLI_MODEL` | Legacy agentapi tier / agy model |
| `AGY_AUTO_APPROVE_PROMPT` | Policy replacement |
| `AGY_APPROVER_SOCKET` | Socket base, default `~/.local/share/agy-auto-approve/runtime/approver.sock` |
| `AGY_APPROVER_STATE_DIR` | State base, default `~/.local/share/agy-auto-approve/agents` |
| `AGY_AUTO_APPROVE_LOG_DIR` | Shared daily logs, default `~/.local/share/agy-auto-approve/logs` |
| `AGY_AUTO_APPROVE_SILENT` | Suppress hook stderr notices |
| `PI_CODING_AGENT_DIR` | Pi configuration/authentication source and extension installation root |

The socket is shared across all agents and instances. State remains isolated by agent
and instance. Multiple Desktop connections should use distinct instance names on their
hooks. Setting only the log-directory override places state under its `state/`
subdirectory. The binary preserves each caller's backend PATH and appends
`~/.gemini/antigravity-cli/bin` for agy/agentapi lookup.

## Overview, TUI and directories

Bare `agy-auto-approve` opens the terminal menu. The configuration form can edit
per-agent approver backend, model and effort; installation uses the same form.
Agentapi has no effort selector. Other model-specific capabilities are checked during
review, not by sending paid requests in the form. Secrets remain in environment variables.
Changes are previewed and saved only after confirmation. Unrelated TOML fields/comments
are preserved. Switching a backend in the form replaces that agent's approver settings.

`config` displays all three agents and setting sources; `config --agent pi` selects one.
`config --edit` opens the shared TOML file. A root invocation with options requires a
subcommand and never enters the TUI.

Legacy `.gemini` configuration is not read. Logs, statistics and reviewer sessions
start fresh. Existing explicit path environment overrides still work.
Before upgrading an active installation, stop its old daemons using the old binary:
the new default paths do not discover processes listening on legacy sockets.

Default state directories are `agents/<agent>/<instance>` under the application data root.
Runtime sockets use `$XDG_RUNTIME_DIR/agy-auto-approve/runtime` when set to an absolute
path, otherwise the application data directory's `runtime/` directory. Explicit socket
overrides identify one shared socket; state overrides retain agent/instance suffixes.
