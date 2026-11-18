# DBX RDP

Remote Windows desktops inside DBX, on macOS, Windows and Linux.

- Native Rust sidecar powered by [IronRDP](https://github.com/Devolutions/IronRDP).
- Sandboxed Vue workbench that renders the remote framebuffer on a canvas.
- Fixed desktop presets (1920×1080 by default) or fit-to-workbench dynamic resolution.
- Lossless bitmap updates by default (RDP lossy compression disabled) for sharp text and images.
- Remote cursor shapes rendered as native CSS cursors.
- NLA/CredSSP by default; legacy TLS is opt-in.
- Untrusted server certificates require an explicit fingerprint confirmation and are pinned per connection.
- Passwords are stored by DBX Secret Store (`binding: "secret"`), never in connection JSON or the workbench UI.
- Text clipboard synchronization in both directions. Local to remote advertises the text through the standard CLIPRDR channel and then sends `Ctrl+V` (`Cmd+V` on macOS or the Paste button); synthetic Unicode typing is kept as a fallback when the channel is unavailable. Remote to local writes the copied text into the system clipboard. The local text is also re-advertised when the workbench regains focus, so remote apps can paste it from their own **Edit → Paste** menu. On macOS, `Ctrl+V` is forwarded to the remote session as a normal Windows paste shortcut.
- Image clipboard in both directions: paste a local screenshot or image into the remote desktop (PNG, DIB/DIBV5), and save an image copied on the remote side as PNG from the banner above the canvas.
- Drag-and-drop or pick local files to put them on the remote clipboard (press `Ctrl+V` in the remote window to paste them), and save files copied on the remote side to your machine (a single file is saved directly, multiple files are packed into a ZIP archive). Large uploads can be cancelled from the toolbar.
- Optional shared folder: redirect a local directory to the remote session as `\\tsclient\DBX RDP` (RDPDR). Off by default; the default root is a private plugin data directory. The path field offers the host's native directory picker.
- Audio redirection through RDPSND on macOS and Windows (built with `cpal`). Linux builds omit audio to avoid an ALSA build dependency.
- macOS Command shortcuts (`Cmd+C/X/A/Z/S/F/P`, `Cmd+←/→`) are mapped to their Windows Ctrl equivalents; `Cmd+W/Q/Tab` stay with the host. A Windows keyboard layout can be selected per connection.
- Dirty-rectangle frame pipeline with a per-tick byte budget, plus a toolbar stats overlay and Fit/50/75/100/125/150/200% zoom (`Cmd/Ctrl` `+`/`-`/`0`). Trackpad wheel deltas are coalesced per animation frame for smooth scrolling.
- Fullscreen (`Ctrl+Cmd+F` on macOS, `F11` elsewhere) with a floating toolbar that appears near the top edge, falling back to an immersive workbench mode when the host denies HTML fullscreen.
- Keyboard focus follows the desktop: toolbar clicks, tab activation and window focus return input to the remote session, with a “Click to type” hint when it is lost.

## Build

Requirements: Node.js 22+, Rust 1.97+, `cmake` (bundled Opus for audio redirection), and `@dbx-app/plugin-cli`.

```bash
npm ci
npm run typecheck
npm test
npm run package
```

The unsigned development package is written to `dist/`. Install it from **Plugin Center → Settings → Third-party and developer options → Allow unsigned development packages**.

Native sidecars are platform-specific: build on macOS (arm64/x64), Windows x64, or Linux (x64/arm64) separately. The release workflow does this in a matrix and uploads one `.dbxp` per target.

## Usage

Create an RDP connection and click **Save & Connect**. DBX starts the sidecar session in the background but does not open the plugin workbench automatically; click the saved connection in the left sidebar to open the remote desktop tab.

## Security notes

- A native sidecar runs with the current user's privileges. Install only packages whose source you trust.
- Official distribution goes through `t8y2/dbx-store` review and repository signing; do not publish the unsigned development package as a trusted store artifact.
- Certificate pins live in `DBX_PLUGIN_DATA_DIR/known_hosts.json` (mode `0600`). Use **Reset certificate trust** in the connection dialog when a server certificate changes legitimately.
- The shared folder is readable and writable by the remote server. Keep it dedicated to RDP transfers and do not point it at your home directory; the default root is `DBX_PLUGIN_DATA_DIR/shared` (mode `0700`).

## Scope

Supports display, keyboard/mouse, fixed or dynamic resolution, remote cursor shapes, NLA, certificate pinning, real CLIPRDR text and image clipboard, file transfer through the clipboard channel, an optional RDPDR shared folder, audio redirection (macOS/Windows), fullscreen, and per-connection keyboard layouts and zoom. Multi-monitor, smart cards, printers and RD Gateway are out of scope for this version.
