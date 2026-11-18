# RDP 插件 0.2.0：移除 Retina、剪贴板/文件传输、被顶号体验与卡顿优化

## 摘要

本轮同时处理五组问题，版本升级到 `0.2.0`：

1. **移除 Retina / 设备像素模式**：删除 `retina` 选项及 `hidpi` 全链路；`fit` 保持工作台 CSS 尺寸；删除“服务器不支持动态分辨率”的 toast，改为状态栏小标记。
2. **降低连接初期卡顿**：限制全屏帧发送频率（默认 10 fps），保留脏矩形 30 fps；增加每秒帧数/字节统计，用真实服务器量化。
3. **文本剪贴板**：删除「共享剪贴板」按钮；`Cmd+V` / `Ctrl+V` / 工具栏「粘贴」统一走“宿主剪贴板 API 优先、原生 paste 事件兜底”；明确区分“宿主不支持 / 被拒绝 / 空剪贴板”；远程文件复制不再被当成文本。
4. **文件双向传输**：本机→远程支持拖拽/选择文件上传并放入远程剪贴板（远程 Ctrl+V 粘贴）；远程→本机支持把远程复制的文件下载并保存（单文件直存，多文件打包 zip）。
5. **被顶号等断开体验**：解析 RDP 断开原因，明确提示“会话已在其他位置登录”；提供「重新连接」和「关闭标签页」，重连中禁用按钮，贴近 MSTSC 的操作路径。

## 当前状态分析

### Retina 与 fit

- [`manifest.json`](file:///Users/zero/GitHub/dbx-plugin-rdp/manifest.json) 的 `resolution` 含 `retina`；[`viewport.ts`](file:///Users/zero/GitHub/dbx-plugin-rdp/src/viewport.ts) 用 `devicePixelRatio` 计算目标尺寸。
- [`credentials.rs`](file:///Users/zero/GitHub/dbx-plugin-rdp/backend/src/credentials.rs#L233-L245) 解析 `retina`；[`session.rs`](file:///Users/zero/GitHub/dbx-plugin-rdp/backend/src/session.rs#L437) 返回 `hidpi`；[`plugin.rs`](file:///Users/zero/GitHub/dbx-plugin-rdp/backend/src/plugin.rs#L169-L186) 接受 `scaleFactor`。
- [`App.vue`](file:///Users/zero/GitHub/dbx-plugin-rdp/src/App.vue#L50) 维护 `hidpi`；[`RemoteCanvas.vue`](file:///Users/zero/GitHub/dbx-plugin-rdp/src/RemoteCanvas.vue#L154-L163) 监听 `hidpi` 重算尺寸。
- 真实服务器上 DisplayControl 协商可能 `CapabilitiesTimedOut`，触发 [`session.rs`](file:///Users/zero/GitHub/dbx-plugin-rdp/backend/src/session.rs#L927-L941) 的 `rdp/resizeFallback`，前端 [`App.vue`](file:///Users/zero/GitHub/dbx-plugin-rdp/src/App.vue#L281-L284) 弹出“服务器不支持动态分辨率”。Retina 依赖该通道，因此失败时体验最差。

### 连接初期卡顿

- [`session.rs`](file:///Users/zero/GitHub/dbx-plugin-rdp/backend/src/session.rs#L974-L1066) 每 33ms 泵一帧；`apply_pixels` 在脏区超过 60% 时把整帧标记为 full（[`session.rs`](file:///Users/zero/GitHub/dbx-plugin-rdp/backend/src/session.rs#L195-L212)）。
- 登录/桌面动画期间连续发送 1920×1080 RGBA 全帧（约 8.3MB/帧，4MiB 分块 + base64 后约 11MB IPC），30fps 时会把宿主/iframe 压满，表现为连接后加载卡顿、掉帧后反复请求全帧。

### 文本剪贴板

- [`clipboard.rs`](file:///Users/zero/GitHub/dbx-plugin-rdp/backend/src/clipboard.rs#L125-L136) 的 `on_remote_copy` 只要看到 `CF_UNICODETEXT` 就请求文本。Windows Explorer 复制文件时也可能带 `CF_UNICODETEXT`（显示名，通常无扩展名），于是本地剪贴板被写成“文件名且没有后缀”。
- [`bridge.ts`](file:///Users/zero/GitHub/dbx-plugin-rdp/src/bridge.ts#L125-L133) 把“API 不存在”“读取被拒绝”“剪贴板为空”都返回 `null`，UI 无法给出可操作提示。
- [`RemoteCanvas.vue`](file:///Users/zero/GitHub/dbx-plugin-rdp/src/RemoteCanvas.vue#L428-L482) 中 macOS `Cmd+V` 依赖沙箱内原生 `paste` 事件；真实 DBX 工作台是 opaque origin，`navigator.clipboard` 不可用，原生 paste 在部分宿主可能不触发。Host API 1.3 提供 `window.dbxPlugin.clipboard.readText()`，但当前 [`manifest.json`](file:///Users/zero/GitHub/dbx-plugin-rdp/manifest.json#L12-L14) 声明的是 `host_api: ^1.0`，且 UI 未检查 `capabilities.clipboardRead`。
- 「共享剪贴板」只广播文本、不自动粘贴（[`App.vue`](file:///Users/zero/GitHub/dbx-plugin-rdp/src/App.vue#L395-L412)），用户点击后仍需在远程按 Ctrl+V，容易误判为失败；按用户决定删除。

### 被顶号/断开

- [`session.rs`](file:///Users/zero/GitHub/dbx-plugin-rdp/backend/src/session.rs#L943-L954) 的 `Terminated` 只把状态置为 `Disconnected`，`Ok(reason)` 被忽略；`reason` 实际是 `GracefulDisconnectReason`，`Other(String)` 里包含 `[Protocol independent error] Another user connected to the server, forcing the disconnection of the current connection`。
- 前端 [`App.vue`](file:///Users/zero/GitHub/dbx-plugin-rdp/src/App.vue#L519-L526) 断开时只显示通用“已断开”和「重连」；工具栏「重连」始终可点（[`App.vue`](file:///Users/zero/GitHub/dbx-plugin-rdp/src/App.vue#L483)），没有关闭标签页入口，重复点击会并发重连。
- Host API 1.3 的 `closeWorkbench()` 可关闭当前工作台标签，但需要 `host.workbench` 权限（当前 manifest 未声明）。

## 变更内容

### A. 移除 Retina / 简化 fit

文件：`manifest.json`
- 删除 `resolution` 的 `{ "label": "Retina / device pixels (sharpest)", "value": "retina" }`，同步删除 zh-CN localization 的 `retina` 条目。
- 分辨率默认仍为 `1920x1080`；保留 `2560x1440` / `3840x2160` / `fit`。

文件：`backend/src/credentials.rs`
- 删除 `retina: bool` 字段；`parse_resolution` 返回 `(width, height, auto_resize)`，删除 `"retina"` 分支和 `parses_image_quality` 之外的相关断言。
- 更新 `defaults_to_fixed_1080p`、`parses_fit_and_fixed_resolutions`、`unknown_resolution_falls_back_to_1080p`，删除 retina 用例。

文件：`backend/src/session.rs`
- 删除 `retina` 字段、`"hidpi"` 返回值、`resize` 的 `scale_factor` 参数；`RdpInputEvent::Resize` 固定 `scale_factor: 100`。
- 保留 `DisplayResizeFallback` 的服务端缩放兜底和日志，但不再单独弹 toast（由前端状态栏提示）。

文件：`backend/src/plugin.rs`
- `rdp/resize` 不再解析 `scaleFactor`，调用 `session.resize(width, height)`。

文件：`src/viewport.ts` / `tests/viewport.spec.ts`
- `targetDesktopSize(cssWidth, cssHeight)` 删除 `hidpi`、`devicePixelRatio` 参数，保留 2560×1600 上限、最小 1024×600、偶数取整。
- 测试改为：1998×1071 → 1998×1070；0×0 → 1024×600；超大尺寸封顶 2560×1600。

文件：`src/bridge.ts`
- `AttachResult` 删除 `hidpi`；`resize()` 删除 `scaleFactor` 参数。

文件：`src/App.vue`
- 删除 `hidpi` ref、attach 赋值、`switchConnection`/`reconnect` 重置、`targetViewport` 的 DPR 参数、`onResizeRequest` 的 scaleFactor。
- `resizeUnsupported` 保留：状态栏在 `resizeUnsupported === true` 时显示 `缩放显示` 小标记（新增 i18n `app.scaledDisplay`），不再弹 toast。

文件：`src/RemoteCanvas.vue`
- 删除 `hidpi` prop 和 `watch(props.hidpi)`；`scheduleRemoteResize` 调用新签名。

文件：`src/i18n.ts` / `README.md`
- 删除 `retina` 相关文案与说明；删除 `app.resizeFallback`，新增 `app.scaledDisplay`。

### B. 连接初期卡顿

文件：`backend/src/session.rs`
- `FrameState` 新增 `last_full_emit: Option<Instant>` 和统计字段 `frames_sent: u64`、`full_frames_sent: u64`、`bytes_sent: u64`。
- 新增常量 `FULL_FRAME_MIN_INTERVAL: Duration = Duration::from_millis(100)`。
- `pump_frame` 开头判断：若 `frame.full` 为真、没有待发送的 `resize_pending`、且 `last_full_emit` 距今不足 100ms，直接返回且**不清除** `frame.full`；下一次 tick 继续发送最新像素。脏矩形帧不受影响，仍按 33ms 发送。
- 发送全帧时更新 `last_full_emit`；每次发送累加统计。
- 把现有 1 秒一次的 `emit full` 日志扩展为 `frames=... full=... bytes=...`，并在 `frame_pump` 每秒输出一次后清零计数。
- 纯函数 `should_emit_full(last: Option<Instant>, now: Instant, has_resize: bool) -> bool` 便于单测。

说明：登录/桌面动画的全屏帧从最多 30fps 降到 10fps，带宽/IPC 峰值约降三分之二；正常桌面局部更新不受影响。

### C. 文本剪贴板

文件：`manifest.json`
- `permissions` 增加 `"host.workbench"`；`engines.host_api` 从 `^1.0` 改为 `^1.3`（`clipboardRead` / `closeWorkbench` 属于 Host API 1.3）。
- 若打包校验拒绝 `^1.3`，保留 `^1.0` 并依赖 `capabilities.clipboardRead` 探测，但要在 README 注明需要 DBX 0.6.52+。

文件：`src/env.d.ts`
- `DbxPluginApi` 增加 `closeWorkbench?(): Promise<void>`。
- `capabilities` 类型改为 `Record<string, unknown>`，新增 `DbxPluginFileTransfer` 接口（见 D）。

文件：`src/bridge.ts`
- 新增：
  ```ts
  export type ClipboardReadResult =
    | { ok: true; text: string }
    | { ok: false; reason: "unsupported" | "denied" | "empty" };

  export async function readClipboard(): Promise<ClipboardReadResult> {
    const capabilities = api().capabilities ?? {};
    if (capabilities.clipboardRead === false) return { ok: false, reason: "unsupported" };
    const clipboard = api().clipboard;
    if (!clipboard || typeof clipboard.readText !== "function") {
      return { ok: false, reason: "unsupported" };
    }
    try {
      const text = await clipboard.readText();
      return text ? { ok: true, text } : { ok: false, reason: "empty" };
    } catch {
      return { ok: false, reason: "denied" };
    }
  }

  export function closeWorkbench(): void {
    const close = api().closeWorkbench;
    if (typeof close === "function") void close().catch(() => undefined);
  }
  ```
- 删除 `sendClipboardText`。

文件：`src/RemoteCanvas.vue`
- `onPaste` 保留原生 `paste` 事件作为快速路径；新增 150ms 兜底定时器：若本轮快捷键没有收到原生文本，则调用 `readClipboard()`，成功则 `emit("paste-text", text)`。
- 用 `pasteToken` 序号 + `lastPasteText/lastPasteAt` 去重，避免原生 paste 与 API 路径重复注入。
- macOS：`Cmd+V` 不 `preventDefault`（保留原生 paste）；不再把 `Ctrl+V` 当作本地粘贴，按 RDP 惯例转发远程 Ctrl+V。非 macOS：`Ctrl+V` 同样保留原生 paste + API 兜底。
- 删除 `emit("paste-request")` 路径（不再需要宿主剪贴板兜底）。

文件：`src/App.vue`
- 删除 `sendClipboard()`、`app.shareClipboard` 按钮、`app.clipboardSent` 文案。
- `pasteClipboard()` 改用 `readClipboard()`：
  - `unsupported` → toast `app.clipboardUnsupported`（“当前 DBX 宿主不支持读取剪贴板，请升级到 0.6.52+”）。
  - `denied` → toast `app.clipboardDenied`（“DBX 拒绝了剪贴板读取权限”）。
  - `empty` → toast `app.clipboardEmpty`，并发送远程 Ctrl+V（兼容“远程剪贴板里已有内容”）。
- `onPasteText` 保持直接注入。
- 初始化时读取 `api().capabilities.clipboardRead`；不支持时把「粘贴」按钮置灰并设置 title 提示。

文件：`backend/src/clipboard.rs`
- `on_remote_copy` 先判断文件格式：`format.id() == ClipboardFormatId::CF_HDROP` 或 `format.name().is_some_and(|name| name.value() == "FileGroupDescriptorW")`。存在文件格式时：
  - 不请求 `CF_UNICODETEXT`；
  - 通过 `SendInitiatePaste(file_list_format_id)` 拉取文件描述符（见 D）；
  - 直接返回，彻底避免“文件名当文本写进本地剪贴板”。
- 只有在没有文件格式时才请求 `CF_UNICODETEXT`。
- 删除 `local_text` 字段、`advertise_local_text`、`on_format_data_request` 的文本分支（「共享剪贴板」已删除）；`on_request_format_list` 改为：有暂存文件则广播文件列表，否则不主动广播。

文件：`src/i18n.ts`
- 新增 `app.clipboardUnsupported`、`app.pasteUnsupported`、`app.fileCopyUnsupported`；删除 `app.shareClipboard`、`app.clipboardSent`、`app.resizeFallback`。

### D. 文件双向传输

#### D1. 后端文件状态与暂存

新增 `backend/src/files.rs`：
- 常量：
  ```rust
  const MAX_FILES: usize = 500;
  const MAX_FILE_BYTES: u64 = 2 * 1024 * 1024 * 1024;
  const MAX_TOTAL_BYTES: u64 = 4 * 1024 * 1024 * 1024;
  const UPLOAD_CHUNK_BYTES: usize = 512 * 1024;
  const DOWNLOAD_CHUNK_BYTES: u32 = 1024 * 1024;
  ```
- `StagedFile { index, name, relative_path: Option<String>, size, path: PathBuf, written: u64 }`。
- `UploadState { token, dir: PathBuf, files: Vec<StagedFile>, total: u64 }`。
- `DownloadState { token, dir: PathBuf, files: Vec<FileDescriptor>, archive: Option<PathBuf>, archive_size: u64 }`。
- 文件名/路径清洗：
  - `sanitize_relative_path(name, relative_path)`：按 `/`（并把 `\` 转 `/`）拆分，拒绝空段、`.`、`..`、绝对路径、控制字符；每段长度 ≤ 255；最终相对路径用 `/` 连接。
  - Windows 保留名（CON、PRN、AUX、NUL、COM1-9、LPT1-9）不做特殊改写，交给远程 Windows 处理；本地暂存用编号目录规避。
- 目录权限：`DBX_PLUGIN_DATA_DIR/clipboard/uploads/<token>`、`.../downloads/<token>`，创建时 `0700`，文件 `0600`；写入使用 `create_new(true)` 防止符号链接/覆盖。
- 定期清理：会话结束 `Drop` 清理；启动时删除超过 1 小时的旧目录。
- 最小 ZIP writer（store，不压缩）：
  ```rust
  pub fn write_zip(entries: &[(String, PathBuf)], dest: &Path) -> io::Result<u64>
  ```
  - 使用 `crc32fast`（已在依赖树）计算 CRC32；本地文件头/中央目录/EOCD，方法 0，不启用 ZIP64（单文件 ≤ 2GiB）。
  - 多文件下载统一打包为 `rdp-files-<timestamp>.zip`；单文件直接使用原文件名，不打包。

#### D2. 剪贴板文件协议

文件：`backend/src/clipboard.rs`
- `RdpClipboardFactory::new` 增加 `files: Arc<Mutex<ClipboardFiles>>`；`ClipboardFiles { upload: Option<UploadState>, download: Option<DownloadState>, remote_files: Option<(Vec<FileDescriptor>, Option<u32>)> }`。
- `client_capabilities()` 改为：
  ```rust
  ClipboardGeneralCapabilityFlags::STREAM_FILECLIP_ENABLED
      | ClipboardGeneralCapabilityFlags::CAN_LOCK_CLIPDATA
      | ClipboardGeneralCapabilityFlags::HUGE_FILE_SUPPORT_ENABLED
  ```
  这样服务端才会走文件剪贴板。
- `on_remote_copy`：识别到 `FileGroupDescriptorW` 时记录其 format id，并 `SendInitiatePaste(format.id())`；不再请求文本。
- 新增 `on_remote_file_list(&mut self, files: &[FileDescriptor], clip_data_id: Option<u32>)`：
  - 保存到 `ClipboardFiles.remote_files`；
  - 通过 `emitter.event("rdp/files", { connectionId, sessionId, files: [{ name, relativePath, size }], clipDataId })` 通知 UI；
  - 不自动下载。
- `on_file_contents_request`（本机→远程）：
  - `SIZE`：返回 8 字节小端 `file_size`。
  - `RANGE`：按 `index` 找到暂存文件，`seek(position)` 读取 `min(requested_size, 4MiB, 剩余)`，`SendFileContentsResponse(FileContentsResponse::new_data(bytes))`；越界或文件不存在返回 `new_error(stream_id)`。
- `on_lock`/`on_unlock`：记录日志；暂存文件保留到会话结束或下一次 advertise。
- 下载响应：`on_file_contents_response` 通过 `stream_id` 找到等待中的 `oneshot::Sender`，把 `is_error`/`data` 发回下载任务。

#### D3. 后端 RPC

文件：`backend/src/plugin.rs`
- 新增方法：
  - `rdp/files/upload/begin` `{ connectionId, token, name, relativePath?, size }` → `{ index }`
  - `rdp/files/upload/chunk` `{ connectionId, token, index, offset, dataBase64 }` → `{ written }`
  - `rdp/files/upload/finish` `{ connectionId, token, index }` → `{ ok }`
  - `rdp/files/upload/cancel` `{ connectionId, token }`
  - `rdp/files/advertise` `{ connectionId, token }` → 调用 `SendInitiateFileCopy(files)`，返回 `{ count }`
  - `rdp/files/download` `{ connectionId, clipDataId? }` → `{ token }`（异步开始下载）
  - `rdp/files/read` `{ connectionId, token, offset, length }` → `{ dataBase64, eof }`
  - `rdp/files/release` `{ connectionId, token }`
- 下载任务使用 `tokio::spawn`，逐文件、逐 1MiB 分块发送 `SendFileContentsRequest`，通过 `oneshot` 等待响应，写入暂存目录；全部完成后打包（多文件）并 `emitter.event("rdp/files/ready", { token, name, size })`。
- 超时 30 秒/分块，失败时 `rdp/files/error`。
- `Cargo.toml` 增加 `crc32fast = "1"` 和 `base64 = "0.22"`（都已在依赖树中）。

#### D4. 前端文件 UI

文件：`src/files.ts`（新增，纯函数）
- `uploadId()`：`crypto.randomUUID()`，不可用时用时间戳+随机数。
- `formatBytes(n)`；`sanitizeDisplayName(name)`。
- 分块循环：`readHandle` → `invoke("rdp/files/upload/chunk")`。
- `saveArchive()`：`beginSave({ name, size })` → 循环 `rdp/files/read` → `write` → `finish`。
- 单元测试：分块 offset/eof、格式化、上传 token 唯一性。

文件：`src/App.vue`
- 工具栏删除「共享剪贴板」，新增「发送文件」按钮（`fileTransfer.pick({ multiple: true })`）。
- `onDrop` / `onDragState`：拖拽进入画布时显示遮罩“松开以发送到远程”；松开后上传。
- 新增 `fileOffer` ref：收到 `rdp/files` 事件后显示横幅“远程复制了 N 个文件（XX MB）”，按钮「保存到本机」；点击调用 `rdp/files/download`，收到 ready 后 `beginSave` 写盘，显示进度 toast。
- 上传/下载进度用 `showToast` 节流（≥200ms）。
- 托管能力检测：`capabilities.fileTransfer?.pick` / `.drop` / `.beginSave` / `.read`；不支持时隐藏「发送文件」和保存按钮，`rdp/files` 事件只提示“文件复制暂不支持”。
- `rdp/files` 事件只接受当前 `connectionId` + `sessionId`。

文件：`src/bridge.ts` / `src/env.d.ts`
- 新增 `FileTransferApi` 类型和 `fileTransfer?` 字段；`capabilities` 类型放宽。

文件：`manifest.json`
- `permissions` 增加 `"host.workbench"`；文件传输不需要额外权限（由宿主对话框/拖放授权）。

约束与安全：
- 只处理文本与普通文件；不跟随本机符号链接（`pick`/`drop` 由宿主打开句柄，sidecar 只接收字节）。
- 暂存目录 `0700`、文件 `0600`；会话结束或 1 小时后清理。
- 单文件 2GiB、总量 4GiB、500 个文件上限；超限直接报错。
- 远程文件名来自服务器，下载后再次清洗，禁止 `..` 和绝对路径。

### E. 被顶号与断开体验

文件：`backend/src/session.rs`
- 新增：
  ```rust
  struct DisconnectInfo { reason: String, detail: String }
  ```
  `RdpSession` 增加 `disconnect_info: Mutex<Option<DisconnectInfo>>`。
- `Terminated(Ok(reason))`：
  - `let detail = reason.to_string();`
  - `classify_disconnect(&detail)` → `another-connection | admin-disconnect | forced-logoff | idle-timeout | session-timeout | server-memory | server-denied | server-shutdown | server-reboot | logoff | other`。
  - 保存 `DisconnectInfo` 后 `set_state(Disconnected)`，`emit_state()` 在 payload 中附带 `reason`、`detail`。
- `Terminated(Err(error))`：`reason: "terminated"`, `detail: error.to_string()`。
- 连接成功时清空 `disconnect_info`。
- `classify_disconnect` 纯函数单测：包含 `Another user connected`、`administrative tool`、`idle session limit`、`server is shutting down` 等样例。

文件：`src/bridge.ts`
- `RdpStateEvent` 增加 `reason?: string; detail?: string`。

文件：`src/App.vue`
- 新增 `disconnectReason` / `disconnectDetail`；`rdp/state` 的 `disconnected` 分支保存。
- 断开 overlay：
  - 标题：`app.disconnected` 或 `disconnect.anotherConnection.title`。
  - 描述：优先显示本地化 `disconnect.<reason>.detail`，没有则显示 `detail`。
  - 按钮：「重新连接」（`button--primary`，`state === "connecting"/"reconnecting"` 时禁用并显示 spinner）和「关闭标签页」（调用 `closeWorkbench()`，宿主不支持时隐藏）。
- `reconnect()` 增加 `reconnecting` 守卫；循环开始前清理 `attachRetryTimer`，组件卸载时置 `unmounted` 标志并停止重试。
- 工具栏「重连」在 `connecting/reconnecting` 时禁用。
- i18n 新增：
  - `disconnect.anotherConnection.title` / `.detail`（中文：“会话已在其他位置登录” / “同一账号在另一处登录了远程桌面，当前连接已被断开。点击“重新连接”可重新接管会话。”）
  - `disconnect.adminDisconnect.*`、`disconnect.idleTimeout.*`、`disconnect.sessionTimeout.*`、`disconnect.serverShutdown.*`、`disconnect.serverReboot.*`、`disconnect.logoff.*`、`disconnect.terminated.*`、`disconnect.other.*`
  - `app.closeTab`、`app.reconnectingHint`。

说明：RDP 被顶号时服务器会发送 `ERRINFO_DISCONNECTED_BY_OTHER_CONNECTION (0x00000005)`，IronRDP 将其转换成 `GracefulDisconnectReason::Other("[Protocol independent error] Another user connected ...")`，因此按文本分类即可；若服务器直接断开 TCP、没有 ErrorInfo，则显示通用“连接被服务器断开”。

### F. 版本、测试与打包

- 版本：`manifest.json`、`package.json`、`backend/Cargo.toml`、`backend/Cargo.lock` → `0.2.0`。
- Rust 单测：
  - `classify_disconnect` 各类原因。
  - `should_emit_full` 节流逻辑。
  - 文件名/相对路径清洗、ZIP writer 结构（本地头/中央目录/EOCD）。
  - 保留现有 30 个测试。
- 前端单测：
  - `viewport.spec.ts` 删除 hidpi 用例。
  - 新增 `files.spec.ts`：分块 offset/eof、`formatBytes`、上传 token 唯一性、错误分类。
  - `frameProtocol`、`input`、`cursor` 测试保持通过。
- 真实环境验证见下节。

## 决策与假设

- **移除 Retina**（用户明确要求）：fit 模式按工作台 CSS 尺寸请求分辨率，清晰度受源分辨率限制，不再尝试 DPR 放大；依靠 lossless + 字体平滑保持现状。
- **删除「共享剪贴板」**（用户明确要求）：文本粘贴统一走「粘贴」按钮 / 快捷键；后端不再主动广播本地文本。
- **文件双向都做**（用户明确要求）：但不承诺 Finder/Explorer 直接 Cmd+C/Cmd+V；本机→远程为“拖拽/选择文件 → 远程 Ctrl+V”，远程→本机为“横幅提示 → 保存到本机（单文件直存 / 多文件 zip）”。
- **Host API 1.3**：`clipboard.readText()` 与 `closeWorkbench()` 需要，manifest 升到 `^1.3`；若宿主版本较低，UI 通过 `capabilities` 禁用并提示。
- **卡顿**：先做全帧节流和统计；不引入视频编码/压缩。若真实环境仍卡顿，再评估降低全帧上限或减少分块。
- **被顶号**：按 MSTSC 行为——明确原因、手动重新连接、可关闭标签页；不做自动抢占重连。
- **不改 DBX 宿主**。

## 验证步骤

1. 静态检查与单测：
   ```bash
   cd /Users/zero/GitHub/dbx-plugin-rdp
   cargo fmt --manifest-path backend/Cargo.toml --check
   cargo clippy --manifest-path backend/Cargo.toml --all-targets -- -D warnings
   cargo test --manifest-path backend/Cargo.toml
   npm run typecheck
   npm test
   ```
2. 开发宿主 + 真实服务器（`10.166.135.164`，NLA）：
   - **fit 回归**：连接后不再出现“不支持动态分辨率” toast；分辨率等于工作台 CSS 尺寸；拖动窗口只发脏矩形，状态栏显示 `缩放显示` 仅当回退。
   - **卡顿**：登录/桌面动画 10 秒内 `rdp.log` 的 `full` 帧数 ≤ 100；对比 0.1.5 的首帧时间和主观流畅度。
   - **文本粘贴**：`Cmd+V`（macOS）与工具栏「粘贴」均能注入多行中英文；API 被拒绝时 toast 文案准确；原生 paste 与 API 不重复注入。
   - **右键/光标/fit/NLA/证书 pin 回归**。
3. 真实 DBX 0.6.52（必须手动安装 0.2.0 验证 Host API）：
   - **宿主剪贴板**：确认 `capabilities.clipboardRead === true`；复制本机文本 → `Cmd+V` → 远程记事本出现文本；`Ctrl+V` 仍作为远程 Ctrl+V。
   - **本机→远程文件**：拖拽 2 个文件（含中文名/子目录）到画布 → toast 提示已放入远程剪贴板 → 远程 Explorer Ctrl+V → 文件名、扩展名、目录结构正确。
   - **远程→本机文件**：远程 Explorer 复制 1 个文件（有扩展名）→ 横幅提示 → 保存 → 文件名/扩展名正确；复制多个文件 → 保存为 zip → 解压后目录结构正确。
   - **远程→本机文本**：远程复制文本 → 本地剪贴板收到文本，不再出现“文件名/无后缀”。
   - **被顶号**：用另一个 RDP 客户端以同一账号登录 → 本插件显示“会话已在其他位置登录”及原因；「重新连接」可重新接管；「关闭标签页」能关闭工作台；重连中按钮禁用。
   - **文件大小/异常**：空文件、0 字节、中文名、含空格名、超过上限的文件给出明确错误；取消保存/取消上传无残留临时文件。
4. 打包：
   - `npm run package` 生成 `dist/io.github.lwclick.rdp-0.2.0-darwin-arm64.dbxp`。
   - `unzip -l` 确认 manifest 0.2.0、UI、sidecar、checksums 齐全；记录 SHA-256。
   - 本机 DBX 手动安装后复测文本、文件、被顶号、关闭标签页。

## 风险与后续

- 文件剪贴板依赖服务端 `CB_STREAM_FILECLIP_ENABLED` 协商；部分 Windows 版本/策略可能拒绝，届时明确提示并保留文本粘贴。
- 远程→本机只能“保存对话框/zip”，无法直接写入 Finder 剪贴板；这是 DBX 沙箱 Host API 的限制，不是插件实现问题。
- 全帧节流会降低全屏动画的帧率；如影响体验，可把 `FULL_FRAME_MIN_INTERVAL` 调整为 80ms。
- 若 `host_api: ^1.3` 导致旧 DBX 无法安装，保留 `^1.0` 并通过 `capabilities.clipboardRead` 降级，文档注明 0.6.52+ 才支持本机→远程文本粘贴。
- 真实环境验证依赖测试服务器可用；若不可达，至少完成单测、typecheck、dev-host 文本粘贴与打包，并在最终回复中列明未覆盖项。
