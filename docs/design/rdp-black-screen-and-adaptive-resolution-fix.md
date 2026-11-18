# RDP 插件：黑屏与分辨率自适应修复计划

## 结论

黑屏和分辨率过小仍然是**客户端问题**，不是服务器设置。0.1.1 已解决密码问题，但 DBX 宿主在 iframe 加载/切换/缩放期间可能**静默丢弃二进制帧**，而当前 UI 在尺寸不匹配时会直接丢弃后续帧，且 resize 时会把画布清空；同时画布 CSS 最大只按 1:1 显示，导致远程分辨率较小时无法铺满工作台。

## 证据与根因

### 1. 黑屏：尺寸不匹配后永久丢帧

- `src/RemoteCanvas.vue:191-193`：当帧头 `desktopWidth/Height` 与当前画布尺寸不一致时直接 `return`，既不更新画布，也不请求全帧。
- DBX 0.6.51 宿主二进制通道：
  - `crates/dbx-plugin-runtime/src/plugins/runtime.rs:739-757` 分发 binary；
  - `crates/dbx-plugin-runtime/src/plugins/host.rs:595-609` 与 `src-tauri/src/commands/plugins.rs:164-183` 使用 `broadcast`（容量 64/128），消费者落后时静默丢最旧消息；
  - `apps/desktop/src/lib/plugins/pluginHostBridge.ts:601-607` 在 iframe 的 `targetWindow()` 尚未就绪时直接丢弃消息，无缓冲。
- 触发过程：连接成功 → 首帧短暂显示（“闪一下”）→ UI 请求 resize（1084×652 等）→ 宿主播发 RESIZE 帧/新尺寸全帧时 iframe 仍在加载或切换 → 消息被丢 → 后续局部帧尺寸与旧画布不符 → 全部被 `return` 丢弃 → 永久黑屏。
- 当前只在收到“部分帧且 awaitingFull”时请求全帧（`RemoteCanvas.vue:168-181`），无法覆盖“RESIZE 帧和首帧都丢失”的情况。

### 2. 分辨率过小/不自适应

- `src/App.vue:166-173` 的 `targetViewport()` 在宿主隐藏或尺寸为 0 时用 `max(1, clientWidth)`，随后被夹到 **1024×600**；后台恢复的 workbench 会以这个最小值 attach。
- `src/RemoteCanvas.vue:133-148` 的 `scheduleRemoteResize()` 只在 ResizeObserver 回调且尺寸 ≥320×240 时执行；标签页 `v-show` 隐藏时 iframe 为 0×0，恢复后可能只保留最初的最小请求。
- `src/RemoteCanvas.vue:63-75` 的 `canvasStyle` 使用 `Math.min(..., 1)`，**禁止放大**；远程比工作台小时画布保持原始像素尺寸，四周黑边，看起来“太小”。
- resize 时 `src/RemoteCanvas.vue:103-124` 清空画布为黑色，`backend/src/session.rs:98-108` 把 RGBA 缓冲重新分配为零；若服务器 resize 后没有立即全量重绘，画面就停在黑色。

## 修改方案

### 1. UI：尺寸不匹配时自愈并强制补全帧

文件：`src/RemoteCanvas.vue`

- 新增 `adoptSize(width, height)`：
  - 用旧画面做最近邻缩放生成新尺寸的临时画面（见第 2 点），保留内容；
  - 更新 `surfaceWidth/Height`、canvas 尺寸与 `imageData`；
  - 设置 `awaitingFull = true`。
- 修改 `handleBinary`：
  - 收到 `FRAME_TYPE_RESIZE`：`adoptSize`，并启动全帧重试。
  - 收到 bitmap 且 `surfaceWidth !== header.desktopWidth || surfaceHeight !== header.desktopHeight`：
    - `adoptSize(header.desktopWidth, header.desktopHeight)`；
    - 如果当前帧是整屏（`full === true`）就继续应用；
    - 否则请求全帧并返回。
  - 任何 `seq` 缺口、chunk 顺序异常、`awaitingFull` 时收到部分帧：保持现有逻辑，但统一走 `requestFullWithRetry()`。
- 新增全帧重试：`awaitingFull` 时立即请求一次，之后每 500ms 重试，最多 10 次；收到完整全帧后停止；组件卸载/会话切换时清理定时器。用于覆盖 iframe 初始化期间宿主丢帧的情况。

### 2. UI：resize 不再清成黑屏

文件：`src/RemoteCanvas.vue`

- 新增纯函数 `scaleRgbaNearest(source, sourceWidth, sourceHeight, targetWidth, targetHeight): Uint8ClampedArray`（可放到 `src/frameProtocol.ts` 或独立 `src/pixels.ts`，便于 vitest）。
- `resetSurface(width, height, { preserve = false })`：
  - 在修改 `canvas.width/height` 前保留旧 `imageData`；
  - 新尺寸下用 `scaleRgbaNearest` 填充，alpha 置 255；
  - `preserve=true` 时立即 `putImageData` 显示缩放后的旧画面；
  - 首次 attach 仍允许黑色。
- `FRAME_TYPE_RESIZE`、props 尺寸变化、尺寸不匹配全部使用 `preserve: true`。

### 3. UI：自适应铺满与初始视口

文件：`src/RemoteCanvas.vue`、`src/App.vue`

- `canvasStyle` 去掉 `1` 上限：
  - `scale = Math.min(availableWidth / surfaceWidth, availableHeight / surfaceHeight)`；
  - 保留 `max-width/max-height: 100%`；允许放大后铺满工作台。
- `targetViewport()`：
  - 当 `canvasHost.clientWidth/Height <= 0`（隐藏/未布局）时，返回当前 `desktopWidth/Height`（初始 1280×800），不要再夹到 1024×600；
  - 只有在拿到正尺寸后才按工作台尺寸请求。
- `scheduleRemoteResize()`：
  - 仅在 `availableWidth/Height >= 320/240` 且尺寸与上次请求/当前远程尺寸相差 >24 时发送；
  - 记录 `lastRequestedSize`，收到 RESIZE/desktop-size 后清除，避免重复 resize；
  - 同时监听 `document.visibilitychange` 与 `ResizeObserver`，标签页从隐藏恢复时重新触发；
  - 服务器 `DisplayResizeFallback` 后进入“仅缩放”模式：停止自动 resize，保持画布铺满，toast 改为“服务器不支持动态分辨率，已切换为自适应缩放”。

### 4. Sidecar：resize 时缩放保留旧画面

文件：`backend/src/session.rs`

- `FrameState::resize(width, height)`：
  - 若尺寸未变直接返回；
  - 用最近邻把旧 `rgba` 缩放到新尺寸（保持 alpha=255），而不是 `vec![0; ...]`；
  - 保留 `dirty=None`、`full=true`、`resize_pending=Some(...)`。
- 增加单元测试：4×3 已知像素 → resize 8×6，断言四角/中心像素按最近邻映射且非全黑。
- 这样 resize 后立即发出的全帧是旧画面缩放版，不是黑色；服务器后续重绘会覆盖。

### 5. 诊断日志（脱敏）

文件：`backend/src/session.rs`（或新增 `backend/src/diagnostics.rs`）

- 在 `DBX_PLUGIN_DATA_DIR/rdp.log` 追加（超过 1 MiB 时截断）：
  - attach 请求的 viewport；
  - 每次 resize 请求/`DisplayResizeFallback`；
  - 发送的 resize 帧与帧尺寸摘要（每秒最多一条，避免刷屏）；
  - 全帧/部分帧计数、重连次数。
- 不记录密码、用户名、剪贴板内容、证书内容。
- 用于在本机 DBX 无法看控制台时定位宿主丢帧与分辨率请求。

### 6. 版本与安装

- `manifest.json`、`package.json`、`backend/Cargo.toml` 升级到 `0.1.2`。
- 重新打包 `dist/io.github.lwclick.rdp-0.1.2-darwin-arm64.dbxp`。
- 安装后重启 DBX（或断开重连），确保旧的 0.1.1 sidecar 不再复用。

## 验证步骤

### 单元测试

```bash
PATH="$HOME/.cargo/bin:$PATH" cargo fmt --check
PATH="$HOME/.cargo/bin:$PATH" cargo clippy --all-targets
PATH="$HOME/.cargo/bin:$PATH" cargo test

PATH="$HOME/.local/share/mise/installs/node/22.13.0/bin:$PATH" npm run typecheck
PATH="$HOME/.local/share/mise/installs/node/22.13.0/bin:$PATH" npm test -- --run
```

- 新增 Rust：resize 保留/缩放像素。
- 新增 UI：`scaleRgbaNearest` 的边界、1×1、放大/缩小、alpha。
- 关键回归：`handleBinary` 在尺寸不匹配时不会静默丢弃（抽出纯函数测试尺寸采纳逻辑）。

### 真实服务器（sidecar 级）

- 用 0.1.2 sidecar + `connection_secrets` 连接，attach 后连续请求：
  - 1084×652 → 等待全帧；
  - 1600×1000 → 等待全帧；
  - 1000×700 → 等待全帧。
- 断言每次 resize 后：
  - 收到对应尺寸的 RESIZE 帧；
  - 至少一个全帧，且采样非黑像素 > 0（缩放保留后不应为 0）；
  - 后续局部帧持续到达；
  - 证书 pin 匹配。

### 开发宿主 UI（模拟宿主行为）

- 启动 `dbx-plugin dev`（Node 22 + 本仓库 vendored SDK）。
- 连接真实服务器后：
  - 在 1000×700、1400×900、1800×1000 三种页面尺寸间切换；
  - 每次截图并采样 canvas 非黑像素；
  - 将 workbench 标签切到后台再切回（模拟 `display:none` 0×0），确认恢复后自动 resize 且不黑屏；
  - 状态栏远程尺寸应与工作台可视区域匹配。
- 清理开发数据目录（含明文测试凭据）。

### 本机 DBX

- 安装 0.1.2 → 重启 → 连接真实服务器：
  - 不再黑屏；
  - 画面铺满工作台；
  - 状态栏尺寸随窗口变化；
  - 若出现“已切换为自适应缩放”，说明服务器拒绝动态 resize，但画面仍应正常且铺满。
- 查看 `DBX_PLUGIN_DATA_DIR/rdp.log` 确认 resize/帧记录。

## 决策与约束

- 保留动态分辨率，但以“任何情况下都有画面”为第一优先级；服务器不支持时自动降级为 CSS 缩放。
- 允许画布放大（可能略糊），不再保留黑边。
- 本轮不引入 devicePixelRatio 超采样，避免帧体积翻倍加剧宿主丢帧；若用户仍觉得模糊，再单独评估。
- 测试凭据只通过环境变量传入，日志不包含敏感信息。
- 不自动 commit/push；验证通过后展示 diff，等待确认。

## 风险

- 宿主 `broadcast` 丢帧无法完全消除，只能通过全帧重试和尺寸自愈恢复；极端情况下会短暂卡顿。
- 若服务器 resize 后长期不重绘，画面会停留为缩放后的旧内容（不再黑屏），但不会自动获得新内容。
- 缩放旧缓冲区在 1084×652 级别约 70 万像素，耗时可忽略；4K 级别需关注，暂由 2560/1600 上限约束。
