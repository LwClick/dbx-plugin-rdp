# RDP 插件：分辨率与鼠标光标修复计划

## 摘要

本次只修改独立插件仓库 `/Users/zero/GitHub/dbx-plugin-rdp`，不改 DBX 宿主：

1. **“保存并连接”不自动打开 tab**：确认是 DBX 0.6.51 宿主硬编码行为，插件 manifest 无法修改。按用户决定保留现状，只在连接类型描述和 README 中提示“保存后从左侧连接列表打开”，不修改 DBX。
2. **分辨率不对**：新增连接级“桌面分辨率”选项，默认固定 `1920×1080`；固定模式不再跟随工作台动态 resize，只按比例缩放显示；保留 `fit`（自适应工作台）模式避免旧能力回归。
3. **鼠标不可见**：移除 `cursor: none`；关闭 IronRDP 软件光标合成，后端转发 `PointerDefault/PointerHidden/PointerBitmap`，前端用 CSS cursor 还原远程光标形状（箭头、I 型、缩放箭头等）。
4. 版本升级到 `0.1.3`，跑完 Rust/前端检查、打包，并在真实 Windows Server 上验证。

## 现状分析

### 宿主行为（只读确认，不修改）

- [frontendPlugin.ts](file:///Users/zero/GitHub/dbx-main/apps/desktop/src/lib/plugins/frontendPlugin.ts#L226-L233)：新建插件连接时无条件追加 `save-and-connect`，manifest 没有覆盖入口。
- [ConnectionDialog.vue](file:///Users/zero/GitHub/dbx-main/apps/desktop/src/components/connection/ConnectionDialog.vue#L6846-L6905)：保存后只调用 `startSavedConnection`。
- [ConnectionDialog.vue](file:///Users/zero/GitHub/dbx-main/apps/desktop/src/components/connection/ConnectionDialog.vue#L6822-L6836)：`startSavedConnection` 只执行 `store.connect`，不打开 workbench。
- [SidebarTreeRuntimeHost.vue](file:///Users/zero/GitHub/dbx-main/apps/desktop/src/components/sidebar/SidebarTreeRuntimeHost.vue#L1964-L1969)：只有从左侧连接项点击时才调用 `queryStore.openPluginConnection`。
- 结论：自动开 tab、把按钮改成“保存”都需要改 DBX；用户已确认不动 DBX。

### 插件现状

- [session.rs](file:///Users/zero/GitHub/dbx-plugin-rdp/backend/src/session.rs#L33-L34) 初始分辨率硬编码 `1280×800`。
- [session.rs](file:///Users/zero/GitHub/dbx-plugin-rdp/backend/src/session.rs#L224-L246) 使用 `with_desktop_width/height` 和 `with_pointer_software_rendering(true)`。
- [session.rs](file:///Users/zero/GitHub/dbx-plugin-rdp/backend/src/session.rs#L387-L414) `attach` 返回当前画面尺寸，但不告知前端是否允许动态 resize。
- [App.vue](file:///Users/zero/GitHub/dbx-plugin-rdp/src/App.vue#L45) `resizeUnsupported` 只处理服务器不支持动态分辨率的情况；默认仍按工作台尺寸 resize。
- [RemoteCanvas.vue](file:///Users/zero/GitHub/dbx-plugin-rdp/src/RemoteCanvas.vue#L587-L589) `.rdp-canvas--focused { cursor: none; }` 隐藏了本地光标。
- [session.rs](file:///Users/zero/GitHub/dbx-plugin-rdp/backend/src/session.rs#L765) `consume_output` 对 pointer 事件走 `_ => {}`，直接丢弃。
- [manifest.json](file:///Users/zero/GitHub/dbx-plugin-rdp/manifest.json#L27-L138) 没有分辨率字段。

### IronRDP 光标行为（依赖源码）

- `with_pointer_software_rendering(true)` 时：系统光标（默认箭头）仍通过 `PointerPosition` 交给宿主绘制；自定义光标才合成进 framebuffer。我们既丢弃 pointer 事件又设置 `cursor: none`，导致完全看不到光标。
- 关闭软件渲染后：`PointerDefault/PointerHidden/PointerBitmap` 正常输出，`PointerBitmap` 携带 `DecodedPointer { width, height, hotspot_x, hotspot_y, bitmap_data(RGBA, 非预乘) }`，适合直接转成 CSS cursor。

## 变更内容

### 1. manifest：新增分辨率选项 + 左侧打开提示

文件：`/Users/zero/GitHub/dbx-plugin-rdp/manifest.json`

在 `fields` 中新增（建议放在 `port` 之后）：

```json
{
  "key": "resolution",
  "label": "Desktop resolution",
  "type": "select",
  "default": "1920x1080",
  "binding": "config",
  "description": "Fixed keeps a standard Windows desktop size and scales it to the workbench. Fit asks the server to follow the workbench size.",
  "options": [
    { "label": "1920 × 1080 (recommended)", "value": "1920x1080" },
    { "label": "1280 × 720", "value": "1280x720" },
    { "label": "2560 × 1440", "value": "2560x1440" },
    { "label": "Fit workbench", "value": "fit" }
  ]
}
```

`localizations.zh-CN` 增加对应 `label/description/options`。

同时修改 connection-provider 的 `description`，给保存后的操作提示：

- en: `Connect to a Windows Remote Desktop server. After saving, open the connection from the left sidebar to start the desktop.`
- zh-CN: `连接 Windows 远程桌面服务器。保存后请从左侧连接列表打开以启动桌面。`

### 2. backend：解析分辨率配置

文件：`/Users/zero/GitHub/dbx-plugin-rdp/backend/src/credentials.rs`

- `RdpConnectOptions` 增加：

```rust
pub desktop_width: u16,
pub desktop_height: u16,
pub auto_resize: bool,
```

- 在 `from_lifecycle` 中解析 `external_config.resolution`：

```rust
let (desktop_width, desktop_height, auto_resize) =
    parse_resolution(external.get("resolution").and_then(Value::as_str));

fn parse_resolution(value: Option<&str>) -> (u16, u16, bool) {
    match value {
        Some("fit") => (1280, 800, true),
        Some("1280x720") => (1280, 720, false),
        Some("2560x1440") => (2560, 1440, false),
        _ => (1920, 1080, false),
    }
}
```

- 新增单元测试：
  - 缺省/未知值 → `1920×1080` 且 `auto_resize == false`
  - `fit` → `1280×800` 且 `auto_resize == true`
  - `1280x720` / `2560x1440` → 固定尺寸且 `auto_resize == false`

### 3. backend：使用配置尺寸并返回 autoResize

文件：`/Users/zero/GitHub/dbx-plugin-rdp/backend/src/session.rs`

- 删除仅用于初始画面的 `INITIAL_WIDTH/INITIAL_HEIGHT`，改为在 `RdpSession` 上保存：

```rust
desktop_width: u16,
desktop_height: u16,
auto_resize: bool,
```

- `ConfigBuilder` 使用 `options.desktop_width/height`；删除 `.with_pointer_software_rendering(true)`（保持默认 `false`）。
- `FrameState::new(options.desktop_width, options.desktop_height)`。
- `attach()` 返回新增字段：

```json
{ "autoResize": false }
```

- `resize()` 在 `!self.auto_resize` 时直接返回 `{ "accepted": false }`，避免固定模式被 UI 意外改尺寸；服务器主动变更分辨率仍通过 `apply_desktop_update` 接受。
- `attach()` 的兜底尺寸改用 `self.desktop_width/height`。

### 4. backend：转发远程光标

文件：`/Users/zero/GitHub/dbx-plugin-rdp/backend/src/protocol.rs`

新增二进制通道和 16 字节头：

```rust
pub const CURSOR_CHANNEL: &str = "rdp-cursor-v1";
pub const CURSOR_HEADER_LEN: usize = 16;
pub const CURSOR_MAGIC: u8 = 0xD3;
pub const CURSOR_KIND_DEFAULT: u8 = 0;
pub const CURSOR_KIND_HIDDEN: u8 = 1;
pub const CURSOR_KIND_BITMAP: u8 = 2;
```

头布局：`magic/version/kind/flags | sessionId u32 | hotspotX u16 | hotspotY u16 | width u16 | height u16`。 bitmap 消息后紧跟 `width*height*4` 的 RGBA（来自 `DecodedPointer::bitmap_data`）。

文件：`/Users/zero/GitHub/dbx-plugin-rdp/backend/src/session.rs`

- `RdpSession` 增加 `cursor: Mutex<Option<Vec<u8>>>` 缓存最后一条光标消息。
- `consume_output` 处理：

```rust
RdpOutputEvent::PointerDefault => session.emit_cursor_default(),
RdpOutputEvent::PointerHidden => session.emit_cursor_hidden(),
RdpOutputEvent::PointerBitmap(pointer) => session.emit_cursor_bitmap(&pointer),
RdpOutputEvent::PointerPosition { .. } => {} // CSS cursor 跟随本地鼠标，无需服务端坐标
```

- `emit_cursor_*` 先构建消息、写入缓存，再在 `emit_events && viewers > 0` 时 `emitter.binary(CURSOR_CHANNEL, &message)`。
- `attach()` 在 `viewers.fetch_add` 之后重放缓存的光标消息，解决“工作台打开前服务器已发过光标”导致丢事件的问题。
- 诊断日志记录 `cursor session=... kind=... WxH`，便于真实环境确认服务器是否发送光标位图。
- `protocol.rs` 增加光标头编解码测试。

### 5. 前端：解析光标并用 CSS cursor 还原

新增 `/Users/zero/GitHub/dbx-plugin-rdp/src/cursorProtocol.ts`：

- 常量与后端一致。
- `parseCursorMessage(data): CursorMessage`，校验 magic/version/session/kind/bitmap 长度。
- `CursorMessage = { sessionId, kind: "default" | "hidden" | "bitmap", hotspotX, hotspotY, width, height, bitmap?: Uint8Array }`。

新增 `/Users/zero/GitHub/dbx-plugin-rdp/src/cursor.ts`：

- `cursorCss(message): string`
  - `default` → `"default"`
  - `hidden` → `"none"`
  - `bitmap` → 用离屏 canvas `putImageData` 还原 RGBA，`toDataURL("image/png")`，返回 `url("...") hotspotX hotspotY, default`
  - 超过 128px 的位图按比例最近邻缩小（浏览器对 CSS cursor 尺寸有限制），hotspot 同步缩放；无 bitmap/尺寸非法时回退 `"default"`

文件：`/Users/zero/GitHub/dbx-plugin-rdp/src/App.vue`

- `onBinary` 增加 `rdp-cursor-v1` 分支；`sessionId` 为空（attach 返回前）也接受，非空时必须匹配。
- 新增 `cursorStyle = ref("default")`，`switchConnection` / `reconnect` 时重置为 `default`，避免残留 `none`。
- 向 `RemoteCanvas` 传入 `:cursor-style="cursorStyle"`。

文件：`/Users/zero/GitHub/dbx-plugin-rdp/src/RemoteCanvas.vue`

- props 增加 `cursorStyle?: string`，默认 `"default"`。
- `canvasStyle` 返回对象中加入 `cursor: props.cursorStyle || "default"`。
- 删除 `.rdp-canvas--focused { cursor: none; }` 和模板中的 `rdp-canvas--focused` class 绑定。

### 6. 前端：接入 autoResize

文件：`/Users/zero/GitHub/dbx-plugin-rdp/src/bridge.ts`

- `AttachResult` 增加 `autoResize?: boolean`。

文件：`/Users/zero/GitHub/dbx-plugin-rdp/src/App.vue`

- 新增 `autoResize = ref(true)`。
- `ensureAttached` 成功后：`autoResize.value = result.autoResize !== false`。
- 模板改为 `:auto-resize="autoResize && !resizeUnsupported"`。
- `desktopWidth/Height` 初始值改为 `1920/1080`，与默认分辨率一致，减少首帧闪动；attach 返回后仍以实际值为准。
- `switchConnection` / `reconnect` 时 `autoResize.value = true`，等待新 attach 结果纠正。

`RemoteCanvas.vue` 现有 `scheduleRemoteResize` 已由 `props.autoResize` 控制，无需再改；固定模式下不会发 `rdp/resize`。

### 7. 测试、版本与文档

- 新增 `tests/cursorProtocol.spec.ts`：default/hidden/bitmap 解析、错误 magic/version、bitmap 长度不符。
- `backend/src/protocol.rs` 增加光标头 round-trip 测试。
- `backend/src/credentials.rs` 增加分辨率解析测试。
- 版本号 `0.1.2 → 0.1.3`：`manifest.json`、`package.json`、`backend/Cargo.toml`，并更新 `backend/Cargo.lock`。
- `README.md`：
  - Scope 改为支持固定/自适应分辨率、远程光标形状。
  - 增加一句使用说明：保存连接后从左侧连接列表打开 RDP 桌面。

## 决策与假设

- **不改 DBX 宿主**（用户确认）：保留“保存并连接”和不自动开 tab；通过 description/README 提示从左侧打开。
- **默认分辨率 `1920×1080`**（用户确认）：已有连接未保存该字段时也按 1920×1080 处理。
- **保留 `fit` 模式**：避免动态分辨率能力回归；固定模式下只缩放显示，不再请求 resize。
- **关闭软件光标渲染**：远程光标形状由前端 CSS cursor 还原；`PointerPosition` 不参与定位，因为浏览器光标天然跟随本地鼠标。
- **CSS cursor 最大 128px**：超出时等比缩小，避免浏览器忽略自定义光标。

## 验证步骤

1. 静态检查与单测：
   ```bash
   cd /Users/zero/GitHub/dbx-plugin-rdp
   cargo fmt --manifest-path backend/Cargo.toml --check
   cargo clippy --manifest-path backend/Cargo.toml --all-targets
   cargo test --manifest-path backend/Cargo.toml
   npm run typecheck
   npm test
   ```
2. 打包：
   ```bash
   npm run package
   ```
   产物：`dist/io.github.lwclick.rdp-0.1.3-darwin-arm64.dbxp`。
3. 开发宿主 + 真实 Windows Server：
   - 连接配置选择默认 `1920×1080`，保存并连接后从左侧打开 RDP 连接。
   - 状态栏和 `rdp.log` 应显示 `1920x1080`；Windows 远程"显示设置"应为 1920×1080。
   - 拖动工作台窗口大小：`rdp.log` 不应出现 `resize request`，画面等比缩放。
   - 鼠标移入桌面：箭头可见；移到文本输入框显示 I 型；移到窗口边框显示缩放箭头；`rdp.log` 出现 `cursor ... kind=bitmap`。
   - 把连接改回 `fit`：`rdp.log` 应重新出现 `resize request` / `server resize`，验证旧能力未回归。
   - 验证 NLA、证书 pin、剪贴板、键盘输入不受影响。
4. 回归检查：
   - 新连接保存后状态栏仍显示已连接（宿主行为），从左侧点击可打开桌面。
   - 应用内切换连接、重连后光标和分辨率状态正确重置。
   - 无 `rdp-cursor-v1` 消息时默认显示系统箭头，不出现空白光标。

## 风险与限制

- “保存并连接”不自动开 tab、按钮文案无法在插件内修改；这是 DBX 宿主限制，本次按用户决定不处理。
- 远程光标形状依赖服务器发送 `PointerBitmap`；若服务器只使用系统默认光标，则显示本地系统箭头（仍可见）。
- 固定 1920×1080 在工作台较小时会被缩小显示，这是预期行为；需要 1:1 时可将工作台拉大或选择 `fit`。
- 真实服务器验证需要可用的 Windows 测试凭据；若服务器不可达，只能完成开发宿主和单元测试验证，并在最终回复中明确说明。
