# Antigravity sidecar

Desktop installation and its agentapi connection remain supported. The default
Desktop reviewer is agentapi; the approver provider can also be pi, cli, or openai.
See [configuration](configuration.md) for provider selection and
[commands](commands.md) for current daemon, log and stats behavior.

Plugin source files live under [agy/](../agy/README.md); use that directory as
the plugin root when loading from a checkout. The installer embeds those JSON templates.

`install --desktop-only` enables `any-auto/approver` in
`~/.gemini/config/config.json` and writes manifests under
`~/.gemini/config/sidecars/approver/sidecar.json` and
`sidecars/any-auto/approver/sidecar.json`. The launch command is the absolute
binary path with `daemon start`, an idempotent bootstrap that detaches the shared daemon. Hook registration is shared by agy
CLI/Desktop; `ANTIGRAVITY_LS_ADDRESS` selects Desktop unless overridden.

agentapi requires the agent's connection environment, including its address/token;
finding the executable alone does not supply a connection. Different Desktop
connections should use distinct `--instance` values on hook requests. All use one daemon.
A daemon never mutates its global environment to service another connection.

Each conversation has isolated reviewer state; same-conversation requests
serialize. Read-only tools bypass review, blocked shell commands are denied,
remaining tools go through the configured reviewer. Three consecutive model
denials (or four of the last five) trip the circuit breaker and request user
review. Reviewer/infrastructure failures deny the action. Reviewer subprocesses
cannot recursively approve tool calls.

Restart the Desktop agent after installation/update to reload its sidecar.
`daemon reset --agent agy-desktop --instance NAME` resets that instance's reviewer cache
but retains circuit breakers. Restart preserves persisted sessions. Shared daily audit logs record agent, provider and instance;
logs and stats need no running daemon.
