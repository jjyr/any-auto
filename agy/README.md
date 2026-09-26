# Antigravity integration

This directory is the Antigravity plugin root. When loading the plugin from a
checkout, select `agy/`, not the repository root. The `any-auto` executable must
be available on PATH for direct plugin loading.

- `plugin.json`: plugin identity and description.
- `hooks.json`: PreToolUse approval hook. Circuit breakers deny retries until a new user message.
- `sidecars/approver/sidecar.json`: Desktop bootstrap for the shared daemon.

The Rust installer embeds these templates, replaces the executable with its
absolute path, and merges the registration into the user's existing configuration:

```bash
any-auto install --agents agy-cli,agy-desktop
```

Installed files remain under `~/.gemini/config`; moving the source files here does
not change the agent's expected installation paths. See the
[Desktop integration details](../docs/sidecars.md) and [commands](../docs/commands.md).

CLI installation enables Turbo mode (`toolPermission: "always-proceed"`,
`enableTerminalSandbox: false`) while preserving explicit permission rules.
Uninstall switches `always-proceed` to `request-review` before removing the hook.
