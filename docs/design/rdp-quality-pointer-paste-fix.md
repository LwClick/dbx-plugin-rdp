# RDP 清晰度、右键遮罩与粘贴修复计划

## 摘要

本次同时处理三个问题，版本升级到 `0.1.5`：

1. **清晰度不如 Royal TSX**：关闭 RDP 有损压缩（默认改为 lossless）、修正性能标志让字体平滑真正开启、新增 Retina/设备像素模式（按工作台 CSS 尺寸 × `devicePixelRatio` 请求远程分辨率，封顶 3840×2160，RDP DPI 跟随本地 DPR）。
2. **右键后整屏蓝色遮罩、菜单点别处不消失**：修复鼠标按键状态泄漏（原生右键菜单可能吞掉 `mouseup`），增加窗口级释放兜底和浏览器焦点/选区遮罩防护。
3. **粘贴无效且不方便**：支持 `Cmd+V`/`Ctrl+V` 原生 paste 事件；后端改为 Unicode 文本注入，不再依赖远程剪贴板重定向；工具栏提供“粘贴”和“共享剪贴板”两个明确入口。

## 当前状态与根因

### 清晰度

- [session.rs](file:///Users/zero/GitHub/dbx-plugin-rdp/backend/src/session.rs#L233-L258) 没有显式设置 `color_depth` 和 `lossy_compression`；IronRDP 的 `lossy_compression` **默认是 true**（[config.rs](file:///Users/zero/.cargo/git/checkouts/ironrdp-98b53f60bfdbb711/850839f/crates/ironrdp-client/src/config.rs#L1887-L1897)），会启用动态色彩/色度抽样，文字和图片会变软。
- [session.rs](file:///Users/zero/GitHub/dbx-plugin-rdp/backend/src/session.rs#L1113-L1125) 的 Quality 模式使用 `PerformanceFlags::empty()`，反而关闭了 `ENABLE_FONT_SMOOTHING`；Balanced 的 `PerformanceFlags::default()` 才包含字体平滑。
- 当前固定 1920×1080，但真实 DBX 工作台约 1998×1071 CSS 像素；在 Retina（DPR 2）上实际显示为约 3996×2142 物理像素，1920 源被放大约 2 倍，必然发虚。
- Canvas 的 CSS 缩放 [RemoteCanvas.vue](file:///Users/zero/GitHub/dbx-plugin-rdp/src/RemoteCanvas.vue#L71-L87) 只能决定显示尺寸，无法弥补源分辨率不足。

### 右键蓝色遮罩

- [RemoteCanvas.vue](file:///Users/zero/GitHub/dbx-plugin-rdp/src/RemoteCanvas.vue#L469-L483) 对所有按钮都 `focusInput()`，右键会聚焦隐藏 textarea；`onMouseUp` 只绑定在 canvas 上。
- macOS/WKWebView 的原生右键菜单可能吞掉后续 `mouseup`，远程 Windows 收不到右键释放，菜单无法关闭；同时隐藏 textarea 没有 `outline:none`，canvas/stage 没有禁用选区，可能叠加浏览器焦点/选区高亮。

### 粘贴

- [RemoteCanvas.vue](file:///Users/zero/GitHub/dbx-plugin-rdp/src/RemoteCanvas.vue#L394-L436) 只拦截 `Ctrl+V`/`Ctrl+Shift+V`，**macOS 的 `Cmd+V` 完全没有处理**，也没有 `paste` 事件。
- [App.vue](file:///Users/zero/GitHub/dbx-plugin-rdp/src/App.vue#L349-L366) 的“发送剪贴板”只调用 `sendClipboardText`，后端 [session.rs](file:///Users/zero/GitHub/dbx-plugin-rdp/backend/src/session.rs#L460-L506) 只是广播剪贴板格式，不会执行粘贴。
- `rdp/paste` 采用“广播剪贴板 + 120ms 后 Ctrl+V”，依赖服务器剪贴板重定向和通道顺序，服务器策略关闭或时序不稳时会静默失败。

## 变更内容

### A. 清晰度

#### A1. manifest

文件：`manifest.json`

- 新增 `image_quality` 下拉：

```json
{
  "key": "image_quality",
  "label": "Image quality",
  "type": "select",
  "default": "lossless",
  "binding": "config",
  "options": [
    { "label": "Lossless (sharpest, more bandwidth)", "value": "lossless" },
    { "label": "Balanced (lossy, smaller bandwidth)", "value": "balanced" }
  ]
}
```

- `resolution` 新增：

```json
{ "label": "3840 × 2160", "value": "3840x2160" },
{ "label": "Retina / device pixels (sharpest)", "value": "retina" }
```

- 中文 localization 同步新增 `图像质量`、`无损（最清晰，带宽更高）`、`3840 × 2160`、`Retina / 设备像素（最清晰）`。

#### A2. backend 选项与画质

文件：`backend/src/credentials.rs`

- `RdpConnectOptions` 增加：

```rust
pub lossless: bool,
pub retina: bool,
```

- `parse_resolution` 改为返回 `(mode, width, height, auto_resize)`，其中 `"retina"` 返回初始 `1920×1080`、`auto_resize = true`、`retina = true`；`"3840x2160"` 返回固定尺寸。
- `image_quality` 解析：

```rust
let lossless = external.get("image_quality").and_then(Value::as_str) != Some("balanced");
```

- 新增单元测试：默认 lossless、`balanced` 为 lossy、`retina`/`3840x2160` 解析。

文件：`backend/src/session.rs`

- `RdpSession` 保存 `retina: bool`。
- `ConfigBuilder` 增加：

```rust
.with_color_depth(32)
.with_lossy_compression(!options.lossless)
.with_desktop_scale_factor(100)
```

- 修正 `performance_flags`：

```rust
PerformanceProfile::Quality => {
    PerformanceFlags::ENABLE_FONT_SMOOTHING | PerformanceFlags::ENABLE_DESKTOP_COMPOSITION
}
PerformanceProfile::Balanced => PerformanceFlags::default(),
PerformanceProfile::Speed => { /* 保持现有禁用视觉效果 */ }
```

- `attach()` 返回 `"hidpi": self.retina`。
- `resize()` 增加 `scale_factor: u32` 参数（clamp 100–500），传给 `RdpInputEvent::Resize { scale_factor, .. }`；日志记录 `scale=`。

文件：`backend/src/plugin.rs`

- `rdp/attach` 无需额外参数。
- `rdp/resize` 解析可选 `scaleFactor`，默认 100，传给 `session.resize(width, height, scale_factor)`。

#### A3. 前端 Retina 尺寸

新增 `src/viewport.ts`：

```ts
export interface DesktopSize { width: number; height: number }

export function targetDesktopSize(
  cssWidth: number,
  cssHeight: number,
  hidpi: boolean,
  devicePixelRatio: number,
): DesktopSize {
  const scale = hidpi ? Math.max(1, devicePixelRatio) : 1;
  let width = Math.round(cssWidth * scale);
  let height = Math.round(cssHeight * scale);
  const maxWidth = hidpi ? 3840 : 2560;
  const maxHeight = hidpi ? 2160 : 1600;
  const fit = Math.min(1, maxWidth / Math.max(1, width), maxHeight / Math.max(1, height));
  width = Math.max(1024, Math.floor((width * fit) / 2) * 2);
  height = Math.max(600, Math.floor((height * fit) / 2) * 2);
  return { width, height };
}
```

- 新增 `tests/viewport.spec.ts`：
  - 1998×1071、DPR 2、hidpi → 3840×2058（封顶且保持比例、偶数）。
  - 1440×900、DPR 2、hidpi → 2880×1800。
  - DPR 1、hidpi=false → 1998×1070 左右（上限 2560）。
  - 尺寸为 0 时返回最小值。

文件：`src/bridge.ts`

- `AttachResult` 增加 `hidpi?: boolean`。
- `resize()` 增加 `scaleFactor` 参数并传给 `rdp/resize`。

文件：`src/App.vue`

- 新增 `hidpi = ref(false)`，`ensureAttached` 成功后 `hidpi.value = result.hidpi === true`。
- `targetViewport()` 改用 `targetDesktopSize(availableWidth, availableHeight, hidpi.value, window.devicePixelRatio || 1)`。
- `onResizeRequest(width, height)` 调用 `resize(connectionId, width, height, hidpi.value ? Math.round(devicePixelRatio * 100) : 100)`。
- `switchConnection`/`reconnect` 重置 `hidpi=false`，等待新 attach 结果。
- 状态栏继续显示实际远程分辨率（Retina 下会是 3840×2058 等物理像素）。

文件：`src/RemoteCanvas.vue`

- props 增加 `hidpi?: boolean`，默认 false。
- `scheduleRemoteResize` 使用 `targetDesktopSize(availableWidth, availableHeight, props.hidpi, window.devicePixelRatio || 1)`，上限由 helper 统一处理。
- `watch(() => props.sessionId)` 在非空时额外调用 `scheduleRemoteResize()`，确保 attach 后 Retina 尺寸能立即应用。
- `canvasStyle` 仍按 stage 大小等比缩放；Retina 下源尺寸约为 stage 物理像素，显示比例为 0.5，达到 1:1 物理像素。

### B. 右键蓝色遮罩

文件：`src/RemoteCanvas.vue`

- 增加 `pressedButtons = new Set<number>()` 和 `lastRemote = { x, y }`。
- `onMouseMove`/`onMouseDown` 更新 `lastRemote`；只有左键 `focusInput()`。
- `onMouseDown` 记录按键；`onMouseUp` 只有按键确实在集合中才发送释放，避免重复。
- 新增 `onContextMenu(event)`：`preventDefault()`，若右键仍在集合中立即补发右键释放。
- 新增窗口级兜底：
  - `window.addEventListener("mouseup", onGlobalMouseUp)`；
  - `window.addEventListener("blur", releasePressedButtons)`；
  - `visibilitychange` 隐藏时释放；
  - 卸载时移除监听并释放。
- canvas 与 stage 都绑定 `@contextmenu="onContextMenu"`；stage 的 `@pointerdown` 改为只在左键时聚焦。
- CSS：

```css
.rdp-stage,
.rdp-canvas {
  user-select: none;
  -webkit-user-select: none;
  -webkit-user-drag: none;
  -webkit-tap-highlight-color: transparent;
  touch-action: none;
}

.rdp-input-sink {
  outline: none;
  user-select: none;
}
```

若验证后蓝色层仍存在且确认是远程 Windows 的“半透明选择矩形”，在最终回复中给出服务器端关闭路径，不把服务器视觉设置强行改到插件里。

### C. 粘贴

#### C1. 原生 paste 事件

文件：`src/RemoteCanvas.vue`

- `defineEmits` 增加 `(event: "paste-text", text: string): void`。
- textarea 增加 `@paste="onPaste"`；`onPaste` 读取 `event.clipboardData.getData("text/plain")`，`preventDefault()` 后 emit。
- `onKeyDown` 支持 `(ctrlKey || metaKey) + V`：
  - 如果事件目标是隐藏 textarea，不拦截，让原生 paste 事件带出数据；
  - 否则保留 `paste-request` 作为宿主剪贴板 API 兜底；
  - 设置 `interceptedPaste` 抑制后续 `v` 的 keyup。
- 删除只处理 `Cmd+Shift+V` 的分支。

#### C2. 后端文本注入

文件：`backend/src/session.rs`

- 常量：

```rust
const MAX_TYPE_CHARS: usize = 32_768;
const TYPE_CHUNK_CHARS: usize = 128;
```

- `RdpSession` 增加 `typing: tokio::sync::Mutex<()>`。
- `paste(text)` 改为调用 `type_text(text)`：
  - 超长截断并返回 `truncated`；
  - 分块 128 字符；
  - 普通字符 `Operation::UnicodeKeyPressed/Released`，`\n`/`\r` 用 Enter scancode `0x1C`，`\t` 用 Tab `0x0F`；
  - 第一块先 `release_all()`；
  - `try_send` 队列满时等待 10ms 重试，最多 1 秒；块间 sleep 5ms；
  - 返回 `{ "typed": n, "truncated": bool }`。
- 保留 `rdp/clipboardText`/`set_clipboard` 供“共享剪贴板”使用。

文件：`backend/src/plugin.rs`

- `rdp/paste` 返回 `session.paste(text)` 的结果。

#### C3. UI

文件：`src/bridge.ts`

- 新增 `PasteResult { typed: number; truncated: boolean }`，`paste()` 返回该类型。

文件：`src/App.vue`

- `pasteText(text)`：调用 `paste()`，按结果 toast `app.pasteSent` / `app.pasteTruncated`。
- `onPasteText(text)` 接收 `paste-text` 事件。
- `pasteClipboard()`：工具栏“粘贴”按钮，通过宿主剪贴板 API 读取文本后调用 `pasteText`；读取失败/为空保留现有提示和 `sendCtrlV()` 兜底。
- `onPasteRequest` 复用 `pasteClipboard()`。
- 工具栏：新增“粘贴”按钮（title 提示 `Cmd+V / Ctrl+V`）；原“发送剪贴板”改名“共享剪贴板”，toast 改为“剪贴板已共享，请在远程按 Ctrl+V”。
- 模板给 `RemoteCanvas` 增加 `@paste-text="onPasteText"`。

文件：`src/i18n.ts`

- 新增 `app.paste`、`app.shareClipboard`、`app.pasteSent`、`app.pasteTruncated`；修改 `app.clipboardSent` 文案。

### D. 测试与版本

- Rust：分辨率/图像质量解析测试；`performance_flags` 包含 `ENABLE_FONT_SMOOTHING` 的断言；文本注入的字符/换行/Tab/截断纯函数测试。
- 前端：`tests/viewport.spec.ts`；现有 33 个测试保持通过。
- 版本：`manifest.json`、`package.json`、`backend/Cargo.toml` 升到 `0.1.5`，更新 `backend/Cargo.lock`。
- 打包：`dist/io.github.lwclick.rdp-0.1.5-darwin-arm64.dbxp`。

## 决策与假设

- **画质优先**（用户确认）：默认 `image_quality = lossless`，关闭 RDP 有损压缩；带宽不足时可切换 `balanced`。
- **Retina 模式跟随本地 DPR**（用户确认）：远程分辨率按工作台物理像素请求，DPI 使用 `devicePixelRatio × 100`，封顶 3840×2160；如果服务器不支持动态 resize，回退到现有缩放逻辑并提示。
- **保留固定 1920×1080 预设**：不想提高带宽的用户可继续使用。
- **不改 DBX 宿主**；右键和粘贴问题都在插件内修复。
- **文本注入上限 32,768 字符**：超过时明确提示截断；只接受粘贴的旧应用可使用“共享剪贴板”。

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
2. 开发宿主 + 真实 Windows Server（`10.166.135.164`，NLA）：
   - **清晰度**：同一窗口下对比 `1920×1080 + lossless`、`Retina`、`2560×1440 + lossless`；Retina 状态栏应显示约 `3840×2058`，本地截图 1:1 裁剪文字明显更锐利；远程 Windows 显示设置/DPI 应为 200%。
   - **性能**：拖动窗口、打开记事本、滚动页面，观察是否有明显卡顿；若 4K 帧率不可接受，记录并考虑把 Retina 上限降到 2560×1440。
   - **右键**：桌面右键出现 Windows 菜单；左键点别处菜单消失；无整屏蓝色遮罩；连续右键、右键拖拽不残留按键。
   - **粘贴**：`Cmd+V` 和 `Ctrl+V` 都能把多行文本注入远程记事本；工具栏“粘贴”同样有效并显示字符数；“共享剪贴板”后远程手动 Ctrl+V 仍可用。
   - **回归**：1920×1080 首帧、`fit` 动态 resize、光标形状、NLA/证书 pin、远程→本地剪贴板均正常。
3. 打包检查：
   - `unzip -l` 确认 0.1.5、UI 资源和 sidecar 均在包内。
   - 记录 `.dbxp` 的 SHA-256。
   - 本机 DBX 手动安装 0.1.5 后复测。

## 风险与后续

- Retina 4K 模式会显著增加带宽和 CPU；如果真实环境卡顿，优先降到 2560×1440 或切换 `image_quality=balanced`。
- RDP `desktopScaleFactor` 是否被服务器完全采纳取决于 Windows 版本/策略；若远程内容仍偏小，可改用 `2560×1440 + 100%` 或手动调整远程 DPI。
- Unicode 注入在少数只接受剪贴板粘贴的应用中可能无效，保留“共享剪贴板”兼容路径。
- 若右键蓝色遮罩确认来自服务器端“半透明选择矩形”，插件侧不再继续打补丁，给出服务器设置说明。
- 真实验证依赖测试服务器可用；若不可达，至少完成单测、typecheck 和开发宿主验证，并在最终回复中列明未覆盖项。
