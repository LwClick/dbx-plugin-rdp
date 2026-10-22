# DBX RDP

Remote Windows desktops inside DBX, on macOS, Windows and Linux.

- Native Rust sidecar powered by [IronRDP](https://github.com/Devolutions/IronRDP).
- Sandboxed Vue workbench that renders the remote framebuffer on a canvas.
- NLA/CredSSP by default; legacy TLS is opt-in.
- Untrusted server certificates require an explicit fingerprint confirmation and are pinned per connection.
- Passwords are stored by DBX Secret Store (`binding: "secret"`), never in connection JSON or the workbench UI.
- Text clipboard synchronization in both directions.

## Build

Requirements: Node.js 22+, Rust 1.97+, and `@dbx-app/plugin-cli`.

```bash
npm ci
npm run typecheck
npm test
npm run package
```

The unsigned development package is written to `dist/`. Install it from **Plugin Center → Settings → Third-party and developer options → Allow unsigned development packages**.

Native sidecars are platform-specific: build on macOS (arm64/x64), Windows x64, or Linux (x64/arm64) separately. The release workflow does this in a matrix and uploads one `.dbxp` per target.

## Security notes

- A native sidecar runs with the current user's privileges. Install only packages whose source you trust.
- Official distribution goes through `t8y2/dbx-store` review and repository signing; do not publish the unsigned development package as a trusted store artifact.
- Certificate pins live in `DBX_PLUGIN_DATA_DIR/known_hosts.json` (mode `0600`). Use **Reset certificate trust** in the connection dialog when a server certificate changes legitimately.

## Scope

v1 supports display, keyboard/mouse, dynamic resolution, NLA, certificate pinning and text clipboard. Multi-monitor, audio, drive redirection and file clipboard are out of scope.
