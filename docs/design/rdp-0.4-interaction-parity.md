# RDP 插件 0.4：交互体验对齐 Royal TSX

## 摘要

目标是在不修改 IronRDP 的前提下，把 DBX RDP 的**操作手感**尽量对齐 Royal TSX：macOS 快捷键、键盘布局、输入顺序、触控板滚动、缩放/全屏、焦点、剪贴板、重连反馈和工具栏视觉。

本轮按 4 个阶段推进：

1. **P0 键鼠与输入可靠性**：SDK 二进制顺序化、输入重试、Cmd→Ctrl 重映射、键盘布局、滚轮平滑、焦点保持。
2. **P0/P1 显示交互**：Zoom 快捷键与档位、HTML 全屏/沉浸模式、resize 降级提示。
3. **P1 剪贴板与文件**：聚焦时预广播文本、本地图片粘贴到远程、远程图片另存 PNG、共享目录选择器、文件发送取消。
4. **P1/P2 会话反馈**：重连次数与取消、状态栏、工具栏图标化、会话信息。

多显示器、Gateway/Proxy、H.264/AVC、智能卡、打印机、麦克风仍不在本轮范围。

---

## 一、现状与差距

| 操作 | 0.3.0 现状 | Royal TSX | 本轮 |
| --- | --- | --- | --- |
| macOS 快捷键 | 只处理 Cmd+V，其余 Cmd 组合被 `preventDefault` 吞掉（`RemoteCanvas.onKeyDown`） | Cmd+C/X/A/Z/S/F 等映射为 Windows Ctrl 快捷键 | 实现 |
| 键盘布局 | 未设置，服务端默认布局 | 可选择 Keyboard Layout | 实现 |
| 输入顺序 | SDK 把每个二进制帧提交到 8 线程 worker（`dispatch_binary` → `workers.submit`），键鼠事件可能乱序 | 原生有序输入 | 修复 |
| 丢键 | `handle_input` 对 `try_send` 失败只记日志并丢弃 | 输入排队 | 修复 |
| 触控板滚动 | 每个 wheel 事件固定 ±120 单位（`wheelDelta`），触控板过冲 | FreeRDP 按像素增量发送 | 修复 |
| 缩放 | 下拉 4 档（Fit/100/125/150），无快捷键 | Cmd+/-/0、Zoom In/Out、Actual Size | 增强 |
| 全屏 | 无 | 全屏 + 浮动工具栏 | 实现 HTML 全屏 |
| 焦点 | 点击工具栏后键盘焦点丢失；App 未使用 `focused` 事件 | 自动聚焦会话 | 修复 |
| 剪贴板文本 | 只在 Cmd+V/粘贴按钮时广播 | 实时同步 | 聚焦时预广播 |
| 剪贴板图片 | 无 | 双向图片 | 本地→远程；远程→另存 PNG |
| 共享目录 | 手填路径 | 目录选择器 / 预设 | 目录 picker |
| 重连 | 有浮层，但无次数/取消 | 显示尝试次数、可取消 | 增强 |
| 会话反馈 | 只有安全面板和统计浮层 | Dashboard/状态栏 | 状态栏 |
| 工具栏 | 文本按钮平铺 | 图标 + 分组 | 视觉重做 |

宿主能力边界（来自 DBX 0.5.x 源码）：

- 没有原生全屏/窗口 API；iframe 已声明 `allow="clipboard-write; fullscreen"`，只能用标准 `requestFullscreen()`。
- 剪贴板只有文本写、文本读、**图片读**（`clipboard.readImage()`，能力位 `clipboardImageRead`，返回 PNG base64）；没有图片写、没有剪贴板变更事件；读取有 1 秒限流和每会话 200 次审计。
- workbench 的 `fileTransfer` 没有目录选择；连接表单字段支持 `picker: { kind: "directory" }`（仅桌面宿主）。
- workbench 收到的是宿主转发的 sidecar binary（Tauri event base64）；UI→sidecar 单条上限 8 MiB。

IronRDP 固定 rev 的可用能力：

- `ConfigBuilder::with_keyboard_layout(u32)`（HKL 低字，如 `0x0409`）。
- `MouseButton::{X1,X2}`、水平滚轮 `WheelRotations { is_vertical }` 均支持。
- 输入队列容量 128，只有 `try_send`，没有阻塞发送。
- `ironrdp-cliprdr-format` 提供 `dib_to_png` / `dibv5_to_png` / `png_to_cf_dib` / `png_to_cf_dibv5`。
- cpal 音频音量/静音没有公开句柄，无法运行时静音（需要改 IronRDP，后续）。

---

## 二、实施方案

### Phase 1：键鼠与输入可靠性（P0）

#### 1.1 SDK 二进制帧顺序化

文件：`vendor/sdk-root/plugins/sdk/rust/dbx-plugin-sdk/src/lib.rs`

问题：`dispatch_binary` 把每个帧 `workers.submit(...)` 到 8 线程池，`handle_input` 的 `input_db` 锁竞争顺序不等于帧到达顺序，快速输入/拖拽时可能乱序。

改动：

- `dispatch_binary` 不再提交 worker，直接在读取线程调用 `handler.handle_binary(&channel, data, &emitter)`，错误写 stderr。
- JSON RPC 仍走 worker，保持并发。
- 注释说明：二进制通道是输入/帧数据，必须保序；处理器必须快速返回。

副作用：某个二进制处理器阻塞会阻塞读取线程。我们的处理器只做 mutex + channel send，最多在 1.2 的有限重试内阻塞几毫秒，可接受。

验证：快速输入 "abcdefghij"、按住方向键、拖拽窗口，远端顺序正确；`rdp.log` 无丢键日志。

#### 1.2 后端输入重试

文件：`backend/src/session.rs` `handle_input`

- `MouseMove` 队列满时允许丢弃（最新位置可覆盖）。
- `Key` / `Unicode` / `MouseButton` / `Wheel` 使用与 `inject_text` 相同的重试：最多 10 次、每次 2ms，重新持有 `RdpInputEvent`。
- 仍失败才 `tracing::warn!`，日志带事件类型；不再静默 debug 丢弃。

这解决 128 容量队列在粘贴/快速输入时的丢键。

#### 1.3 macOS Cmd→Ctrl 重映射

新文件：`src/shortcuts.ts`（纯函数 + 单测）

- 导出 `remapMacCommand(event, enabled): Shortcut | null`。
- 映射表（仅无 Ctrl/Alt 的 Cmd 组合）：
  - `Cmd+C/X/A/Z/S/F/P/Y` → 对应 Ctrl 组合。
  - `Cmd+Shift+Z` → Ctrl+Shift+Z。
  - `Cmd+Left/Right` → Home/End（扩展 scancode）。
- 不映射：`Cmd+V`（走现有剪贴板流程）、`Cmd+N/O`（宿主新建/打开文件）、`Cmd+W/Q/H/M/Tab/Space/Backquote` 等系统/宿主快捷键。
- `RemoteCanvas.onKeyDown` 在现有 `isMacPlatform && metaKey` 分支里：
  - 若 `remapShortcuts` 开启且命中映射：`preventDefault()`，发送 Ctrl down + 目标键 down/up + Ctrl up 的完整 tap。
  - 未命中且非 Cmd+V：保持现有 `preventDefault()`，避免触发宿主快捷键。
- 新 prop `remapShortcuts: boolean`，由连接配置传入（见 1.4）。
- 单测覆盖：映射命中、Cmd+V 不映射、Cmd+W 不映射、Shift 组合。

配置：

- `manifest.json` 新增 `remap_macos_shortcuts`（boolean，默认 true，仅 macOS 有意义）。
- `credentials.rs` 解析为 `remap_shortcuts`，`attach` 结果返回 `remapShortcuts`。
- `App.vue` 把它传给 `RemoteCanvas`；非 macOS 忽略。

#### 1.4 键盘布局

文件：`manifest.json`、`credentials.rs`、`session.rs`

- 新字段 `keyboard_layout`（select，默认 `auto`）：
  - `auto` → 不设置（0）。
  - `en-US` `0x0409`、`zh-CN` `0x0804`、`ja-JP` `0x0411`、`de-DE` `0x0407`、`fr-FR` `0x040C`、`es-ES` `0x0C0A`、`ru-RU` `0x0419`、`pt-BR` `0x0416`。
- `RdpConnectOptions` 增加 `keyboard_layout: Option<u32>`；`session.rs` 在 `builder` 链上调用 `with_keyboard_layout(value)`。
- 说明：前端仍发送物理 scancode，布局提示让 Windows 使用匹配的键盘布局。

#### 1.5 焦点保持与提示

文件：`src/App.vue`、`src/RemoteCanvas.vue`

- `RemoteCanvas` 已 emit `focused`；App 增加 `@focused` 状态 `canvasFocused`。
- 工具栏每个按钮点击后调用 `canvasRef.focusInput()`（`setTimeout(0)` 或 `nextTick`），缩放 `<select>` change 后同样回焦。
- 连接后、tab 重新可见（`window.focus` / `visibilitychange`）时自动 `focusInput()`，但如果用户正在操作连接表单则不抢焦点（workbench 内没有其他输入框，可安全聚焦）。
- 未聚焦时在画布底部显示 "Click to type / 点击输入" 小胶囊，点击即聚焦；聚焦后隐藏。
- 保留现有 blur 时 `releaseAll` 和 modifier sync。

#### 1.6 触控板/滚轮平滑

文件：`src/input.ts`、`src/RemoteCanvas.vue`、`tests/input.spec.ts`

现状 `wheelDelta` 每个事件固定 ±120，触控板一次手势会产生几十个事件，滚动过快。

改动：

- 新纯函数 `wheelUnits(deltaY, deltaX, deltaMode)`：
  - `DOM_DELTA_PIXEL`：按像素增量发送，`round` 后 clamp 到 `[-255,255]`（与 FreeRDP winit 行为一致）。
  - `DOM_DELTA_LINE`：每行 ±120。
  - `DOM_DELTA_PAGE`：每页 ±120 × 3。
  - `|deltaX| > |deltaY|` 时走水平滚轮。
- `RemoteCanvas.onWheel` 用 `requestAnimationFrame` 合并同一帧内的 wheel 事件（累加 residual），每帧最多发一个 `wheelPacket`；横向/纵向分别累计。
- 单测覆盖像素/行/页/横向/零增量。

### Phase 2：显示交互（P0/P1）

#### 2.1 Zoom 快捷键与档位

文件：`src/App.vue`、`src/RemoteCanvas.vue`、`src/i18n.ts`

- `zoom` 类型扩展为 `"fit" | 50 | 75 | 100 | 125 | 150 | 200`。
- 工具栏：`−` / 当前百分比 / `+` 三个按钮，Fit 单独按钮；保留 select 或改为按钮组。
- `RemoteCanvas` 监听：
  - macOS `Cmd+=` / `Cmd+-` / `Cmd+0`（0 = fit）。
  - Windows/Linux `Ctrl+=` / `Ctrl+-` / `Ctrl+0`。
  - `preventDefault()`，emit `zoom-change`。
- App 更新并持久化到 `rdpZoom:<connectionId>`（已有逻辑）。
- `canvasStyle` 已支持数字缩放，仅需放开档位和 stage 滚动行为。

#### 2.2 HTML 全屏 / 沉浸模式

文件：`src/App.vue`、`src/styles.css`、`src/i18n.ts`

- App 根节点 `ref="appRoot"`；工具栏增加全屏按钮。
- `toggleFullscreen()`：
  - 若 `document.fullscreenElement` → `exitFullscreen()`。
  - 否则 `appRoot.requestFullscreen()`；`catch` 或 `document.fullscreenEnabled === false` → 进入 CSS 沉浸模式（`rdp-app--immersive`）。
- 监听 `fullscreenchange` 同步 `fullscreen` 状态；组件卸载时退出全屏。
- 全屏/沉浸时隐藏 `.toolbar` 和状态栏，画布铺满；鼠标移到顶部 40px 内显示浮动精简工具栏（Ctrl+Alt+Del、Paste、Zoom、退出），2 秒后自动隐藏。
- 快捷键：macOS `Ctrl+Cmd+F`，Windows/Linux `F11`；`preventDefault()`。
- 说明：浏览器全屏下 `Esc` 由浏览器用于退出全屏，无法转发给远程；非全屏沉浸模式下 `Esc` 仍发给远程。

#### 2.3 Resize 降级提示

- `rdp/resizeFallback` 已存在并设置 `resizeUnsupported`。
- 新增 i18n toast：「服务器不支持动态分辨率，已改用缩放显示」；状态栏显示 `Scaled` 徽标。
- 不改变现有 fallback 行为。

### Phase 3：剪贴板与文件（P1）

#### 3.1 聚焦时预广播本地文本

文件：`backend/src/plugin.rs`、`backend/src/session.rs`、`src/App.vue`、`src/bridge.ts`

- 新 RPC `rdp/clipboard/local`：
  - 入参 `{ connectionId, text }`。
  - `session.advertise_local_text(text)`：`ClipboardFiles::set_local_text`（已存在）+ `SendInitiateCopy([CF_UNICODETEXT])`，**不发送 Ctrl+V、不等待 ACK**。
  - 通道未 ready / clipboard_sync 关闭时直接成功返回（静默）。
- 前端在以下时机调用（1 次读剪贴板，不轮询）：
  - `window.focus` 且 `document.visibilityState === "visible"`。
  - attach 成功后。
  - 用户打开工具栏粘贴菜单/按钮前（已有读取逻辑，可复用）。
- 目的：远程应用通过菜单 `Edit → Paste` 或鼠标点击粘贴时，也能拿到本地最新文本。
- 不做定时轮询：宿主读取有 1 秒限流和每会话 200 次审计。

#### 3.2 本地图片 → 远程剪贴板

文件：`src/RemoteCanvas.vue`、`src/App.vue`、`src/bridge.ts`、`src/env.d.ts`、`backend/src/protocol.rs`、`backend/src/plugin.rs`、`backend/src/session.rs`、`backend/src/clipboard.rs`、`backend/src/files.rs`、`backend/Cargo.toml`

前端：

- `env.d.ts` / `bridge.ts` 增加 `clipboard.readImage()` 与 `capabilities.clipboardImageRead`。
- `RemoteCanvas.onPaste` 同时收集 `text/plain` 与 `image/*`：
  - 有图片 → `getAsFile()` 读 ArrayBuffer；文本非空时一并保留。
  - 发出 `paste-content`（`{ text?, image?: { bytes, mime, name } }`），`preventDefault()`。
  - 只有文本时走现有 `paste-text` 流程。
- Paste 按钮的 API fallback：先读文本；文本为空且 `clipboardImageRead` 为真时，调用 `readImage()` 得到 PNG base64，转换为 ArrayBuffer 后走同一 `paste-content`。
- App 收到 `paste-content` 后：
  - 生成 token，通过新二进制通道 `rdp-clipboard-v1` 分块发送图片（1 MiB/块，最多 24 MiB）。
  - 发送完后调用 `rdp/clipboard/content/paste` `{ connectionId, token, mime, size, text? }`。
  - 失败/超时 toast；清理 token。

二进制协议（`rdp-clipboard-v1`）：

```
magic 0xC2 | version 1 | type 1 (IMAGE)
token u32 BE | chunkIndex u16 BE | chunkCount u16 BE | totalLen u32 BE | payload
```

后端：

- `protocol.rs` 增加 `ClipboardChunk` 解析与单测（越界、块序号、总长上限 24 MiB）。
- `plugin.rs::handle_binary` 接受 `rdp-clipboard-v1`，交给 `session.handle_clipboard_chunk`。
- `session.rs` 维护 `image_uploads: Mutex<HashMap<u32, ImageUpload>>`；收齐后：
  - 用 `png_to_cf_dibv5` / `png_to_cf_dib` 生成两种 DIB（依赖 `ironrdp-cliprdr-format`，同一 git rev）。
  - 写入 `ClipboardFiles` 的新字段 `local_image_dibv5` / `local_image_dib`。
  - **同时**把请求里的 `text` 写入 `local_text`，然后广播 `CF_UNICODETEXT + CF_DIBV5 + CF_DIB`；接收方应用自行选择格式（记事本取文本、画图取图片），与 Royal 行为一致。
  - 等待 `CB_RESPONSE_OK` 后发送 Ctrl+V（复用 `paste_via_clipboard` 的 waiter 机制，新增 `pending_image_paste`）。
- `clipboard.rs::on_format_data_request` 按请求格式返回 `CF_UNICODETEXT` / `CF_DIBV5` / `CF_DIB`。
- `ClipboardFiles::Drop` 清空图片内存。

安全/边界：24 MiB 上限、PNG 解码失败返回错误、token 5 分钟过期清理、不写磁盘。

#### 3.3 远程图片 → 另存 PNG

文件：`backend/src/clipboard.rs`、`backend/src/session.rs`、`backend/src/plugin.rs`、`src/App.vue`、`src/bridge.ts`、`src/i18n.ts`

- `on_remote_copy` 选择请求格式的优先级：`FILE_LIST` > `CF_UNICODETEXT` > `CF_DIBV5` > `CF_DIB`。
  - 理由：Office 复制选区时同时提供图片预览，优先文本避免把文字复制变成图片；Explorer 文件复制优先文件列表。
- 新增 `pending_image_request: Option<ClipboardFormatId>`；在 `on_format_data_response` 中识别图片响应：
  - `dibv5_to_png` / `dib_to_png` 转 PNG。
  - 写入 `DBX_PLUGIN_DATA_DIR/clipboard/images/<token>.png`（0600）。
  - emit `rdp/clipboard/image` `{ connectionId, sessionId, token, name, size, width, height }`。
- 新 RPC `rdp/clipboard/image/read` `{ connectionId, token, offset, length }` → `{ dataBase64, eof }`（1 MiB/次）。
- 前端：收到事件显示横幅「远程剪贴板图片 — 保存 PNG」；点击后用 `fileTransfer.beginSave` + 分块读取保存。
- 会话结束/新图片到达时清理旧临时文件。

#### 3.4 共享目录选择器

文件：`manifest.json`

- 给 `shared_folder_path` 增加：
  ```json
  "picker": { "kind": "directory" },
  "visible_when": { "field": "share_local_folder", "one_of": [true] }
  ```
- 桌面宿主显示原生目录选择器；Web 宿主不支持目录选择，字段仍可手填。
- 打包前用插件 CLI 校验 schema；如果当前 CLI 不支持 `picker`，退回为纯文本字段并记录兼容性问题。

#### 3.5 文件发送取消与进度

文件：`src/files.ts`、`src/App.vue`、`src/i18n.ts`

- `uploadFiles(connectionId, files, onProgress, signal?: AbortSignal)`：
  - 每块前检查 `signal.aborted`；中止时调用 `rdp/files/upload/cancel`。
- App 文件进度横幅增加「取消」按钮和当前文件名。
- 远端下载已有 `rdp/files/release`，在保存进度横幅同样加取消。

### Phase 4：会话反馈与视觉（P1/P2）

#### 4.1 重连次数与取消

文件：`backend/src/plugin.rs`、`backend/src/session.rs`、`src/App.vue`、`src/i18n.ts`

- `rdp/reconnecting` 已带 `attempt` / `maximumAttempts`；App 保存并在浮层显示「正在重连（2/3）」。
- 新 RPC `rdp/disconnect` → `session.disconnect()`；浮层增加「取消重连」按钮，点击后状态变为 disconnected。
- 重连期间禁用「重新连接」按钮（已有）。

#### 4.2 状态栏

文件：`src/App.vue`、`src/styles.css`、`src/i18n.ts`

- 画布下方新增 `.status-bar`：
  - 状态点 + 文案。
  - 分辨率（`desktopWidth × desktopHeight`）。
  - `stats.fps` / `stats.kbps` / `stats.rects`（无数据时显示 `—`）。
  - 安全模式（NLA/TLS）。
  - `\\tsclient\DBX RDP` 徽标（共享开启时）。
  - 音频 On/Off（attach 的 `audioEnabled`）。
  - resize fallback 的 `Scaled` 徽标。
- 全屏/沉浸模式隐藏状态栏。
- 数据复用现有 `rdp/stats`、`rdp/security`、`rdp/state`、attach 响应，不新增后端协议。

#### 4.3 工具栏图标化

文件：`src/App.vue`、`src/styles.css`、新增 `src/Icon.vue`

- 用内联 SVG 图标（无新依赖）替换纯文本按钮，按组分隔：
  - 输入：Ctrl+Alt+Del、Win、Esc。
  - 剪贴板：Paste、Send files。
  - 显示：Zoom −/%、+、Fit、Fullscreen、Stats。
  - 会话：Security、Reconnect。
- 次要动作（Esc、Win、Stats）收进 `⋯` 溢出菜单，减少工具栏宽度。
- 所有按钮保留 `title` / `aria-label`；`button--active` 样式复用。
- 点击后统一回焦画布（1.5）。

#### 4.4 会话信息

- 扩展现有 `SecurityPanel.vue`：增加分辨率、共享目录、音频、连接时长、最近 stats。
- 不新增独立 Dashboard，避免与状态栏重复。

### Phase 5：版本、文档与发布

文件：`manifest.json`、`package.json`、`backend/Cargo.toml`、`README.md`、`CHANGELOG`（若存在）

- 版本统一 `0.4.0`。
- README 增加：macOS 快捷键重映射、键盘布局、缩放/全屏快捷键、图片剪贴板、状态栏、共享目录选择器。
- 构建与打包：

```bash
cd /Users/zero/GitHub/dbx-plugin-rdp
cargo fmt --manifest-path backend/Cargo.toml
cargo clippy --manifest-path backend/Cargo.toml --all-targets -- -D warnings
cargo test --manifest-path backend/Cargo.toml
npm run typecheck
npm test
npm run package
```

- 产出 `dist/io.github.lwclick.rdp-0.4.0-<target>.dbxp` 并记录 SHA-256。
- 不自动 `git commit` / `git push`。

---

## 三、决策与假设

1. **全屏用标准 Fullscreen API**：宿主没有原生窗口全屏接口；iframe 已允许 `fullscreen`。被拒绝时降级为 CSS 沉浸模式。浏览器全屏下 Esc 由浏览器退出全屏，不能转发给远程；非全屏下 Esc 正常转发。
2. **不做剪贴板轮询**：宿主读取有 1 秒限流和 200 次/会话审计，轮询会很快耗尽额度；改为「聚焦时广播 + 粘贴时广播」。
3. **图片方向**：本地→远程支持 PNG→CF_DIB/DIBV5；远程→本地只能另存 PNG（宿主没有图片写入 API）。
4. **音频音量/静音**：IronRDP 未公开 cpal 音量句柄，本轮不做运行时静音；仅在状态栏/信息面板展示开关状态。
5. **SDK 二进制顺序化**：只改 vendored SDK 的 `dispatch_binary`，JSON RPC 仍并发；处理器保持快速返回。
6. **不修改 IronRDP**：H.264、Gateway、多显示器、智能卡、打印机仍为后续。
7. **快捷键范围克制**：只映射常见编辑快捷键，不碰 Cmd+W/Q/H/M/Tab/Space 等宿主/系统组合。
8. **目录 picker 兼容性**：`picker` 字段在打包时由插件 CLI 校验；如果当前 `$schema`/CLI 不支持，退回纯文本路径字段，不升级 `host_api`。
9. **版本**：本轮完成后升到 `0.4.0`；Phase 3/4 可视验证结果拆成 0.4.x/0.5.0。

---

## 四、验证步骤

### 4.1 自动化

```bash
cd /Users/zero/GitHub/dbx-plugin-rdp
cargo fmt --manifest-path backend/Cargo.toml -- --check
cargo clippy --manifest-path backend/Cargo.toml --all-targets -- -D warnings
cargo test --manifest-path backend/Cargo.toml
npm run typecheck
npm test
npm run package
```

新增测试：

- `tests/shortcuts.spec.ts`：Cmd→Ctrl 映射、排除项、Shift 组合。
- `tests/input.spec.ts`：像素/行/页/横向滚轮单位。
- Rust：`protocol.rs` 剪贴板分块解析；`clipboard.rs` 图片格式选择与 DIB 转换；`session.rs` 输入重试/丢弃策略。
- 现有 46 个后端测试 + 40 个前端测试保持通过。

### 4.2 真实 DBX 人工验收

1. 记事本/浏览器/Explorer 中 `Cmd+C/X/V/A/Z/S/F` 行为与 Windows Ctrl 一致；`Cmd+W/Q/Tab` 不被吞掉。
2. 选择德语/法语键盘布局后，远程输入法与符号位置正确。
3. 快速输入 200 字符、按住方向键、快速拖拽窗口，无丢键/乱序。
4. 触控板单指/双指滚动平滑，水平滚动可用；鼠标滚轮仍按格滚动。
5. `Cmd+=/-/0`、工具栏 Zoom、Fit、100%/200% 缩放正确，鼠标坐标不偏移。
6. 进入全屏、顶部悬停显示浮动工具栏、退出全屏；沉浸模式回退可用。
7. 点击工具栏后直接打字无需再点画布；未聚焦提示可见。
8. 本地复制文本后，在远程应用菜单 `Edit → Paste` 能粘贴。
9. 本地截图/图片 `Cmd+V` 粘贴到远程画图/Word；远程复制图片显示横幅并可保存 PNG。
10. 共享目录字段出现原生目录选择器；共享开关关闭时路径字段隐藏。
11. 发送大文件时取消按钮生效，远端不留半文件。
12. 网络中断后重连浮层显示 `2/3`，取消后进入 disconnected。
13. 状态栏 fps/带宽/分辨率/NLA/共享目录/音频状态与实际一致。
14. 回归 0.3.0：文本剪贴板、文件剪贴板、共享目录、音频开关、resize 黑屏保护。

### 4.3 回归风险

- SDK 顺序化后，确认 JSON RPC（连接、attach、文件）仍并发且无卡顿。
- 输入重试不得阻塞超过 ~20ms，避免影响画面帧。
- Cmd 重映射不得影响 Cmd+V 剪贴板流程和宿主快捷键。
- 全屏退出后工具栏/状态栏恢复，画布尺寸与 pointer 映射正确。
- 图片剪贴板不得影响现有文件剪贴板优先级。

---

## 五、风险与未完成

- **SDK 改动影响面**：`dispatch_binary` 是共享 SDK 代码；改动要保持最小并加注释，发布前做输入延迟和并发 RPC 回归。
- **Fullscreen 兼容性**：宿主 iframe/WebView 可能拒绝 `requestFullscreen()`；必须有 CSS 沉浸降级。
- **剪贴板图片**：PNG→DIB 转换内存峰值、24 MiB 上限、临时 PNG 清理；转换失败要回退到文本/文件。
- **宿主剪贴板图片能力**：`clipboardImageRead` 在旧 DBX 可能不存在；缺失时只保留文本粘贴。
- **键盘布局**：只影响服务端布局提示，不改变物理 scancode 映射；特殊布局仍需用户自行验证。
- **音频静音**：IronRDP 不提供运行时音量，本轮无法做到 Royal 的静音按钮。
- **未纳入本轮**：多显示器、Smart Reconnect（用户可选重连换分辨率）、Gateway/Proxy、H.264/AVC、智能卡、打印机、麦克风、触控手势。
- **未执行 git commit/push**：实现和验证完成后由用户决定提交。
