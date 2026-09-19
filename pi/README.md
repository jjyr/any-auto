# Pi integration

`extensions/any-auto.ts` is the Pi extension that intercepts tool calls and invokes
`any-auto hook --agent pi`. It is embedded into the Rust binary at build time.

```bash
any-auto install --agents pi
```

The installer writes the extension to `extensions/any-auto.ts` under
`PI_CODING_AGENT_DIR` or `~/.pi/agent`, substituting the absolute binary path.
Run `/reload` in Pi after installation. Pi 0.84.2 or newer is required.

For development, load this source extension explicitly with Pi's `-e` option;
`any-auto` must be on PATH when using the unmodified source. There is no separate
npm package or build step for this integration.

The extension handles incoming approval requests; the approval backend's separate
Pi RPC process is implemented in `src/backend/pi.rs`. See the
[RPC and extension research](../docs/pi-research.md) and
[configuration reference](../docs/configuration.md).
