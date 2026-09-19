# Antigravity sidecar

Desktop installation and its agentapi connection remain supported. The default
Desktop reviewer is agentapi; the approver provider can also be pi, cli, or openai.
See [configuration](configuration.md) for provider selection and
[commands](commands.md) for current daemon, log and stats behavior.

`install --desktop-only` enables `agy-auto-approve/approver` in
`~/.gemini/config/config.json` and writes manifests under
`~/.gemini/config/sidecars/approver/sidecar.json` and
`sidecars/agy-auto-approve/approver/sidecar.json`. The launch command is the absolute
binary path with `daemon run --mode sidecar`. Hook registration is shared by agy
CLI/Desktop; `ANTIGRAVITY_LS_ADDRESS` selects Desktop unless overridden.

agentapi requires the host's connection environment, including its address/token;
finding the executable alone does not supply a connection. Different Desktop
connections should use distinct `--instance` values in daemon and hook commands.
A daemon never mutates its global environment to service another connection.

Each conversation has isolated reviewer state; same-conversation requests
serialize. Read-only tools bypass review, blocked shell commands are denied,
remaining tools go through the configured reviewer. Three consecutive model
denials (or four of the last five) trip the circuit breaker and request user
review. Reviewer/infrastructure failures deny the action. Reviewer subprocesses
cannot recursively approve tool calls.

Restart the Desktop host after installation/update to reload its sidecar.
An explicit daemon restart resets that instance's reviewer cache but retains
circuit breakers. Shared daily audit logs record host, provider and instance;
logs and stats need no running daemon.
