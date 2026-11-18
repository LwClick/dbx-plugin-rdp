# RDP 右键蓝色遮罩与粘贴失效修复计划

## 摘要

两个问题分开处理：

1. **右击后整屏蓝色遮罩、右键菜单点击别处不消失**：优先修复鼠标按键状态泄漏。macOS/WKWebView 的右键可能让原生菜单吞掉 `mouseup`，导致远程 Windows 一直认为右键仍按下，菜单无法关闭；同时补上浏览器选区/焦点遮罩的 CSS 防护。
2. **粘贴无效且操作不便**：当前只拦截 `Ctrl+V`，macOS 的 `Cmd+V` 完全没处理；工具栏“发送剪贴板”只是把文本广播给远程剪贴板，并不会执行粘贴；本地→远程剪贴板重定向还依赖服务器策略。改为 **原生 paste 事件 + 后端 Unicode 文本注入**，快捷键和工具栏都能直接粘贴，不再依赖远程剪贴板重定向。

版本升级到 `0.1.5`，真实服务器验证后打包。

## 现状与根因

### 右键蓝色遮罩

- [RemoteCanvas.vue](file:///Users/zero/GitHub/dbx-plugin-rdp/src/RemoteCanvas.vue#L469-L483)：`onMouseDown` 对所有按钮都 `focusInput()`，右键也会聚焦隐藏 textarea；`onMouseUp` 只绑定在 canvas 上。
- 原生右键菜单在 `contextmenu` 阶段可能吞掉后续 `mouseup`，远程 Windows 收到右键按下但收不到释放，右键菜单和蓝色遮罩会保持。
- 隐藏 textarea 没有 `outline: none`/`user-select: none`，canvas/stage 也没有禁用选区；右击聚焦后可能叠加浏览器焦点或选区高亮。
- 目前没有“窗口级 mouseup / blur 释放鼠标按键”的兜底。
- 如果修复后仍存在蓝色遮罩，则属于远程 Windows 的“显示半透明选择矩形”（桌面左键拖拽效果），需要在服务器端关闭该视觉效果；插件侧不应强行修改。

### 粘贴失效

- [RemoteCanvas.vue](file:///Users/zero/GitHub/dbx-plugin-rdp/src/RemoteCanvas.vue#L394-L436)：只拦截 `Ctrl+V` 和 `Ctrl+Shift+V`，**macOS 的 `Cmd+V` 没有被拦截**，也没有 `paste` 事件处理。
- [App.vue](file:///Users/zero/GitHub/dbx-plugin-rdp/src/App.vue#L328-L347) 的工具栏“发送剪贴板”调用 `sendClipboardText`，后端 [session.rs](file:///Users/zero/GitHub/dbx-plugin-rdp/backend/src/session.rs#L460-L506) 只是 `set_clipboard` 并广播格式列表，**不会发送 Ctrl+V**；用户必须在远程再按一次 Ctrl+V，提示“已发送”容易误解。
- `rdp/paste` 采用“广播本地剪贴板 + 120ms 后发送 Ctrl+V”的时序方案，依赖服务器剪贴板重定向和插件/剪贴板通道顺序；服务器策略关闭或时序不稳时就会静默失败。
- DBX 宿主的剪贴板读取有首次授权提示和 1 秒限频，频繁用工具栏粘贴也会被限制。

## 变更内容

### 1. 鼠标按键与右键遮罩

文件：`src/RemoteCanvas.vue`

- 增加按键状态跟踪：

```ts
const pressedButtons = new Set<number>();
let lastRemote = { x: 0, y: 0 };
```

- `onMouseMove` / `onMouseDown` 更新 `lastRemote`。
- `onMouseDown`：
  - 只有左键（`event.button === 0`）才 `focusInput()`，右键不再触发隐藏 textarea 聚焦。
  - 记录 `pressedButtons.add(button)` 后再发送按下事件。
- `onMouseUp`：只有 `pressedButtons.delete(button)` 返回 true 时才发送释放事件，避免 `contextmenu` 已经释放后重复发送。
- 新增 `onContextMenu(event)`：
  - `event.preventDefault()`；
  - 如果右键仍在 `pressedButtons`，立即发送右键释放并移除，保证远程能收到 `right-up` 打开自己的菜单。
- 新增窗口级兜底：
  - `window.addEventListener("mouseup", onGlobalMouseUp)`：鼠标在 canvas 外释放时补发释放。
  - `window.addEventListener("blur", releasePressedButtons)`。
  - `visibilitychange` 隐藏时调用 `releasePressedButtons()`。
  - 卸载时移除监听并释放所有按键。
- 模板：
  - canvas 和 stage 都绑定 `@contextmenu="onContextMenu"`。
  - stage 的 `@pointerdown` 改为只在左键时聚焦（或直接移除，保留 canvas 的 `onMouseDown` 聚焦）。

CSS：

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

验证分支：若真实服务器右击后蓝色遮罩仍存在，且确认来自远程 Windows 的桌面选择矩形，则在最终回复中给出服务器端关闭 “Show translucent selection rectangle” 的路径，不把服务器视觉设置强行改到插件里。

### 2. 粘贴：原生 paste 事件（支持 Cmd+V / Ctrl+V）

文件：`src/RemoteCanvas.vue`

- `defineEmits` 增加：

```ts
(event: "paste-text", text: string): void;
```

- 隐藏 textarea 增加 `@paste="onPaste"`：

```ts
function onPaste(event: ClipboardEvent) {
  const text = event.clipboardData?.getData("text/plain") ?? "";
  event.preventDefault();
  interceptedPaste = true;
  if (text) emit("paste-text", text);
}
```

- `onKeyDown` 改为：

```ts
const pasteShortcut =
  (event.ctrlKey || event.metaKey) && !event.altKey && event.key.toLowerCase() === "v";
if (pasteShortcut) {
  if (event.target === inputSink.value) {
    // 让原生 paste 事件直接带出 clipboardData，不经过宿主授权/限频。
    interceptedPaste = true;
    return;
  }
  event.preventDefault();
  event.stopPropagation();
  emit("paste-request");
  interceptedPaste = true;
  return;
}
```

- 删除只处理 `Cmd+Shift+V` 的旧分支；`Cmd+V` 现在走原生 paste。
- `onKeyUp` 继续用 `interceptedPaste` 抑制 `v` 的释放事件，避免远程多输入一个 `v`。

### 3. 粘贴：后端 Unicode 文本注入

文件：`backend/src/session.rs`

- 新增常量：

```rust
const MAX_TYPE_CHARS: usize = 32_768;
const TYPE_CHUNK_CHARS: usize = 128;
```

- `RdpSession` 增加 `typing: tokio::sync::Mutex<()>`，串行化并发粘贴。
- 新增 `type_text(self: &Arc<Self>, text: String) -> serde_json::Value`：
  - 统计字符数，超过 `MAX_TYPE_CHARS` 截断并返回 `truncated: true`。
  - 按 `TYPE_CHUNK_CHARS` 分块；第一块先 `database.release_all()`（同时释放键盘和鼠标按键）。
  - 普通字符使用 `Operation::UnicodeKeyPressed/Released`；
  - `\n`/`\r` 使用 Enter scancode `0x1C`；
  - `\t` 使用 Tab scancode `0x0F`。
  - 每块通过 `input.try_send(RdpInputEvent::FastPath(events))` 发送；队列满时等待 10ms 重试，最多 1 秒。
  - 每块之间 sleep 5ms，避免输入队列被大文本打满。
  - 返回 `{ "typed": chars, "truncated": bool }`。
- `paste()` 改为直接调用 `type_text()`，不再“广播剪贴板 + 120ms 后 Ctrl+V”。
- `rdp/clipboardText` / `set_clipboard` / 剪贴板广播保留，供“共享剪贴板”按钮和远程→本地复制使用。
- 诊断日志增加 `type session=... chars=... truncated=...`。

文件：`backend/src/plugin.rs`

- `rdp/paste` 返回 `Ok(session.paste(text))`（保持方法名不变，UI 无需新增 RPC）。

### 4. 前端粘贴 UI

文件：`src/bridge.ts`

- 新增：

```ts
export interface PasteResult {
  typed: number;
  truncated: boolean;
}
```

- `paste()` 返回 `Promise<PasteResult>`。

文件：`src/App.vue`

- 新增 `pasteText(text: string)`：调用 `paste()`，按结果 toast：
  - 正常：`app.pasteSent`，参数 `{ count }`；
  - 截断：`app.pasteTruncated`，参数 `{ count }`。
- `onPasteText(text)` 从 `RemoteCanvas` 的 `paste-text` 事件接收文本并调用 `pasteText`。
- `pasteClipboard()`（工具栏按钮）：
  - 通过 `readClipboardText()` 读宿主剪贴板；
  - 读取失败/为空时保留原来的提示和 `sendCtrlV()` 兜底；
  - 成功则调用 `pasteText()`。
- `onPasteRequest` 复用 `pasteClipboard()`。
- 工具栏：
  - 新增主按钮“粘贴”（`app.pasteClipboard`），`title` 提示 `Cmd+V / Ctrl+V`。
  - 原“发送剪贴板”按钮改名为“共享剪贴板”（`app.shareClipboard`），保留 `sendClipboard()`；toast 改为“剪贴板已共享，请在远程按 Ctrl+V”。
- 模板给 `RemoteCanvas` 增加 `@paste-text="onPasteText"`。

文件：`src/i18n.ts`

- 新增/调整：
  - `app.paste`: “粘贴” / “Paste”
  - `app.shareClipboard`: “共享剪贴板” / “Share clipboard”
  - `app.pasteSent`: “已粘贴 {count} 个字符” / “Pasted {count} characters”
  - `app.pasteTruncated`: “已粘贴前 {count} 个字符（内容过长已截断）” / “Pasted the first {count} characters (truncated)”
  - `app.clipboardSent`: 改为“剪贴板已共享，请在远程按 Ctrl+V” / “Clipboard shared; press Ctrl+V in the remote desktop”

### 5. 测试与版本

- Rust 单测：
  - 提取纯函数 `text_operations(ch)` 或 `text_input_plan(text)`，测试普通字符、换行、Tab、CJK 和截断计数。
  - 已有 27 个测试保持通过。
- 前端测试：
  - `input.spec.ts` 增加右键映射断言（`rdpButton(2) === 2`）。
  - 不新增 DOM 依赖测试；粘贴交互通过开发宿主 + 真实服务器验证。
- 版本：`manifest.json`、`package.json`、`backend/Cargo.toml` 从 `0.1.4` 升到 `0.1.5`，更新 `backend/Cargo.lock`。
- 重新打包：`dist/io.github.lwclick.rdp-0.1.5-darwin-arm64.dbxp`。

## 决策与假设

- **粘贴主路径改为文本注入**：不再依赖远程剪贴板重定向，解决“提示成功但没内容”的静默失败；代价是超长文本会截断到 32,768 字符，且个别只接受粘贴的旧应用可能不支持 Unicode 注入。
- **保留“共享剪贴板”**：需要原生远程 Ctrl+V 或大文本/特殊应用时仍可使用；它与“粘贴”按钮职责明确分开。
- **快捷键优先走原生 paste 事件**：避免 DBX 宿主的剪贴板授权弹窗和 1 秒限频，`Cmd+V`/`Ctrl+V` 都可用。
- **右键修复优先处理按键泄漏**：不强行改变远程 Windows 的菜单视觉；如果蓝色层是服务器端“半透明选择矩形”，在最终回复中给出服务器设置路径。
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
2. 开发宿主 + 真实 Windows Server（`10.166.135.164`，NLA）：
   - **右键**：在桌面右键，Windows 菜单正常出现；左键点其他位置菜单消失；无整屏蓝色遮罩。
   - **右键拖拽/窗口边缘**：右键按下后移动再释放，不残留按键状态；连续多次右键正常。
   - **Cmd+V / Ctrl+V**：先左键点远程记事本，按 `Cmd+V`，多行文本直接出现在远程；按 `Ctrl+V` 同样有效。
   - **工具栏“粘贴”**：首次可能弹出 DBX 剪贴板授权；授权后文本直接出现；toast 显示实际字符数。
   - **“共享剪贴板”**：点击后 toast 提示到远程按 Ctrl+V；远程手动 Ctrl+V 能粘贴（若服务器策略允许）。
   - **远程→本地复制**：在远程复制文本，本地剪贴板收到。
   - **回归**：1920×1080 首帧、`fit` 动态 resize、光标形状、NLA/证书 pin 均正常。
3. 打包检查：
   - `unzip -l` 确认 0.1.5、UI 资源和 sidecar 均在包内。
   - 记录 `.dbxp` 的 SHA-256。
   - 本机 DBX 需手动安装 0.1.5 后复测右键和粘贴。

## 风险与后续

- Unicode 注入在极少数只接受剪贴板粘贴的应用（部分控制台/安全输入框）可能无效；保留“共享剪贴板”作为兼容路径。
- 32,768 字符上限是性能与可靠性的折中；超过时 toast 明确提示截断。
- 若右键蓝色遮罩在按键释放修复后仍存在，优先判断为远程 Windows 的“半透明选择矩形”或显卡/桌面效果，而不是继续在插件侧打补丁。
- 真实验证依赖测试服务器可用；若不可达，至少完成单测、typecheck 和开发宿主验证，并在最终回复中说明未覆盖项。
