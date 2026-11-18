# RDP 插件 0.3.0：真实剪贴板、显示管线、驱动器重定向与音频

## 摘要

本计划先记录对 Royal TSX 6.4.5（macOS）与 Royal TS 7.04（Windows）RDP 插件的逆向结论，再给出 DBX RDP 插件 0.3.0 的实施改动。

结论：Royal 的 RDP 能力不是“把 libfreerdp.dylib 注入主程序”，而是**独立 helper 进程 + 静态链接 FreeRDP + 共享 GPU 图层**；Windows 侧则直接拉起内嵌的 `wfreerdp.exe` 并把窗口挂到 WinForms 控件下。DBX 插件模型（WebView + stdio sidecar）无法照搬这两条渲染路径，但 Royal 的功能分层、剪贴板/驱动器/音频通道、错误分类和统计口径可以完整借鉴。

本轮范围（4 项）：

1. 真实 CLIPRDR 文本剪贴板，替换逐字符注入。
2. 显示管线：多脏矩形、按字节预算发送、避免并集触发全帧，附带统计浮层与缩放。
3. RDPDR 驱动器重定向（共享本地目录）。
4. RDPSND 音频播放。

不修改 IronRDP 依赖；H.264/AVC、Gateway、多显示器、智能卡、打印机放入后续 backlog。

---

## 一、逆向结论：Royal TSX 的「Remote Desktop (based on FreeRDP)」

### 1.1 安装形态

macOS 的插件不是随 DMG 一起安装，而是由 Royal TSX 的插件中心下载到用户目录。本机已安装：

```
~/Library/Application Support/Royal TSX/Plugins/Installed/1c919170-3ee3-437f-9326-a2316a9293a0.plugin/
├── PluginInfo/PluginInfo.xml          # 名称、版本 6.4.0.1000、SupportedProtocols=RDP
├── Contents/Resources/Plugin.rtsxbin  # 加密/签名的插件元数据（不可直接反编译）
└── Contents/Frameworks/
    ├── FreeRDPPlugin.framework/       # Swift 插件 UI 层（属性页、Dashboard、键盘布局）
    └── RoyalTSX_FreeRDPPlugin.app/    # LSUIElement helper，27 MB，真正的 RDP 引擎
        ├── MacOS/RoyalTSX_FreeRDPPlugin
        └── Frameworks/FreeRDPMetalRendering.framework
```

Windows MSI（`royaltsinstaller_7.04.51113.0_arm64.msi`）解包后与 RDP 相关的程序集：

- `RoyalApps.Community.FreeRdp.WinForms.dll`（9 MB，内嵌 `wfreerdp.exe` 资源）
- `RoyalApps.Community.Rdp.WinForms.dll`（微软 ActiveX + MsRdpEx 路线，另一条产品线）
- `MsRdpEx.dll` / `Devolutions.MsRdpEx.dll`

### 1.2 macOS：三层架构

1. **插件框架 `FreeRDPPlugin.framework`**
   - Swift + AppKit，依赖主程序的 `RoyalSDKKit.framework`。
   - 只负责属性页、连接配置、键盘布局列表、Dashboard 和重连浮层。
   - 属性页类别：Main / Advanced(Main, Display, Input, Performance, Program, Redirection, Gateway, Proxy, HyperV)。

2. **helper 进程 `RoyalTSX_FreeRDPPlugin.app`**
   - `LSUIElement = true`，`NSAppSleepDisabled = true`，独立进程隔离崩溃。
   - `otool -L` 显示没有 `libfreerdp*.dylib`：FreeRDP **静态链接**进 27 MB 主二进制。
   - `strings` 可见完整协议栈：`rdpgfx_*`、`gdi_*`、`cliprdr`、`rdpdr`、`rdpsnd`、OpenH264（`WelsH264SVCEncoder`、`avc444_decompress`）、OpenSSL 3.6.1、Gateway WebSocket。
   - Release Notes 记录当前版本对应 **FreeRDP 3.24.0**。

3. **Metal 渲染框架 `FreeRDPMetalRendering.framework`**
   - `CAMetalLayer` + `IOSurface` + `remoteStorageTexture` + `didReceiveCAContextID:`。
   - helper 把渲染结果放进跨进程共享的 `CAContext`，主程序按 contextId 嵌入图层，实现 GPU 零拷贝显示。

### 1.3 macOS：FreeRDPKit 的 API 设计

从 Swift 符号表（`nm` + `swift-demangle`）可还原出清晰的封装层：

- 配置：`FRDPOptions_C`、`FRDPOptionsApplier`、`frdp_options_create_with_json` —— **整份连接配置以 JSON 序列化**给 C 层。
- 会话：`FRDPSession`、`FRDPSessionDelegate_C`、`frdp_session_connect/disconnect/status_get`。
- 显示：`FRDPDisplayUpdate`、`FRDPImage`、`FRDPPixelDataAllocator_C`、`frdp_session_set_remote_display`、`canZoomIn/OutRemoteDisplay`、`zoomIn/OutRemoteDisplay`。
- 输入：`FRDPInput`、`frdp_session_keydown_send`、`keymodifiers_send`、`unicode_characters_send`、鼠标/滚轮。
- 剪贴板：`FRDPClipboardManager`、`FRDPLocalClipboardMacOS`、`FRDPLocalClipboardMonitor`、`FRDPLocalClipboardExternal`、`FRDPClipboardRemoteFileManager`，支持文本、BMP 图片和远程文件。
- 证书/认证：`FRDPCertificate`、`FRDPSecurityPolicy`、`FRDPTrustedCertificateStore`、`FRDPAzureVirtualDesktopAccessTokenRequest`。
- 错误分类：`frdp_error_is_cancelled / authentication / certificate_distrusted / potential_security_negotiation / user_triggered_disconnect`。

### 1.4 Windows：wfreerdp.exe 子进程

`RoyalApps.Community.FreeRdp.WinForms`（MIT）反编译结果：

- `FreeRdpControl` 从自身程序集资源写出 `wfreerdp.exe` 到 `%TEMP%`，用 `ProcessStartInfo` 启动。
- 参数由 `FreeRdpConfiguration` 的属性生成：`/parent-window:{hwnd}`、`/v:`、`/u:`、`/p:`、`/w:`、`/h:`、`/scale:`、`/scale-desktop:`、`/kbd:layout:`、`/network:`、`/audio-mode:`、`+clipboard`、`+compression`、`+fonts`、`+themes`、`+aero`、`+window-drag`、`/auto-reconnect`、`/auto-reconnect-max-retries:`、`/gdi:`、`/admin`、`/restricted-admin`、`/shell:`、`/shell-dir:`、`/load-balance-info:`、`/vmconnect:` 等。
- `WindowHelper` 用 Win32 `SetParent` 把 `wfreerdp` 窗口嵌进控件；`ProcessJobTracker` 保证宿主退出时杀死子进程。
- 退出码 `131080` 触发证书确认，`131092` 触发凭据重试；窗口尺寸变化用定时器防抖，按 `AutoScaling` / `SmartReconnect` 决策“缩放”还是“重连换分辨率”。

**对 DBX 的启示**：Royal 把 RDP 引擎放在独立进程、自己控制渲染表面；DBX 插件没有原生窗口 API，不能照搬 `SetParent`/`CAContext`，但可以照搬“通道能力 + 配置项 + 错误分类 + 尺寸策略”。

### 1.5 Royal 的功能清单（来自 Localizable.strings）

- 显示：True Color 32/24/16/15/8 bit、Scale Factor、Desktop Scale Factor、Smart Sizing、Smart Reconnect、动态分辨率、多显示器、Zoom In/Out。
- 输入：Keyboard Layout、Remap macOS shortcuts、Queue Input Events、Windows 键透传。
- 重定向：Clipboard、Disk drives（All / Home / Documents / Custom）、Audio、Microphone、Smart Cards、Printers。
- 网络：RD Gateway（含凭据、策略、smart card、AVD token）、HTTP/SOCKS Proxy、Compression、Network type、Load Balance Info。
- 体验：Maximum Reconnect Attempts、断线重连浮层、连接统计、Debug Logging。

---

## 二、DBX 插件现状与差距

### 2.1 代码位置

| 模块 | 文件 |
| --- | --- |
| 会话/帧泵/输入 | `backend/src/session.rs` |
| 剪贴板通道 | `backend/src/clipboard.rs` |
| 文件暂存/下载/ZIP | `backend/src/files.rs` |
| RPC | `backend/src/plugin.rs` |
| 连接配置 | `backend/src/credentials.rs` |
| 画布渲染/键鼠 | `src/RemoteCanvas.vue` |
| 工作台编排/文件 UI | `src/App.vue` |
| 协议类型 | `src/bridge.ts`、`src/frameProtocol.ts` |
| 清单 | `manifest.json` |

### 2.2 差距表

| 能力 | DBX 0.2.0 | Royal | IronRDP 固定 rev 可用性 |
| --- | --- | --- | --- |
| 本地→远程文本 | 逐字符 Unicode 注入，上限 32768 字符 | 标准 CLIPRDR | `CliprdrBackend` + `SendInitiateCopy/SendFormatData`，可用 |
| 远程→本地文本 | CLIPRDR，`api().copy()` | CLIPRDR | 已有 |
| 剪贴板图片 | 无 | CF_DIB/BMP | `ironrdp-cliprdr-format` 可转 PNG，但宿主只能写文本剪贴板，先做“另存” |
| 文件剪贴板 | CLIPRDR 文件流 + 暂存 | CLIPRDR | 已有 |
| 驱动器重定向 | 无 | RDPDR 磁盘 | `ironrdp-rdpdr` + `ironrdp-rdpdr-native`，可用 |
| 音频 | 无 | RDPSND | `ironrdp-client/sound`（cpal），可用 |
| 显示 | 单并集脏矩形；并集>60% 升全帧；全帧 10fps | 原生 Metal，按区域合成 | `DesktopUpdate` 已是按区域到达，可改多矩形 |
| 缩放 | 仅 CSS 等比缩放 | Smart Sizing / Zoom | 前端可做 |
| 统计 | 日志 frames/full/bytes | 原生统计 | 可复用计数发事件 |
| Gateway | 无 | 有 | `TransportKind::Gateway`，后续 |
| H.264/AVC | 无（EGFX 非 AVC） | rdpgfx + OpenH264 | 需 patch `ironrdp-client`，后续 |
| 多显示器 | 无 | 有 | `with_monitor_layout`，WebView 单画布，后续 |
| 智能卡/打印机/麦克风 | 无 | 有 | `client-smartcard` / RDPDR / `client-sound`，后续 |

### 2.3 关键现状细节

- `FrameState.dirty: Option<Rect>` 把 33ms 内所有更新合并成一个外接矩形（`session.rs:108-128`）。角落里两个小更新会把外接矩形放大，轻易触发 `dirty_area >= 60%` 的全帧逻辑（`session.rs:225-229`）。
- `pump_frame` 只发送一个矩形，全帧被 `FULL_FRAME_MIN_INTERVAL = 100ms` 限制（`session.rs:44-45`、`1388-1490`）。
- `on_request_format_list` 只声明空 `CF_UNICODETEXT`；`on_format_data_request` 返回空串（`clipboard.rs:147-160`、`232-257`）。
- `rdp/paste` 直接调用 `type_text`，前端在 `Cmd+V` / 粘贴按钮时读取宿主剪贴板文本再注入。
- 文件剪贴板依赖远程窗口处于可粘贴状态，服务器不支持文件剪贴板时只能报错；RDPDR 可作为更稳的替代路径。

---

## 三、实施方案

### Phase A：真实 CLIPRDR 文本剪贴板（P0）

**目标**：本地文本通过剪贴板通道提供给远程，不再逐字符注入；注入仅作为降级路径。

#### A1. 共享状态（`backend/src/files.rs` 或 `clipboard.rs`）

在 `ClipboardFiles` 增加：

```rust
pub local_text: Option<String>,
pub local_text_hash: u64,        // 去重，避免重复 SendInitiateCopy
pub pending_paste: bool,
pub paste_waiters: Vec<tokio::sync::oneshot::Sender<bool>>,
```

- 文本上限从 32768 提高到 `MAX_CLIPBOARD_CHARS`（1 MiB 字符）；写入前截断并返回 `truncated`。
- 会话结束时 `Drop` 清空，不落盘、不写日志。

#### A2. 通道实现（`backend/src/clipboard.rs`）

- `on_request_format_list`：始终声明 `CF_UNICODETEXT`；若 `local_text` 存在则直接 `SendInitiateCopy`，否则保持空实现以完成握手。
- `on_format_data_request`：`CF_UNICODETEXT` 返回 `local_text`（`FormatDataResponse::new_unicode_string`），否则 `new_error()`。
- 新增 `on_format_list_response(ok: bool)`：把 `ok` 发给所有 `paste_waiters`。
- `on_ready`：如果 `local_text` 存在，补发一次 `SendInitiateCopy`。
- 文件列表优先级保持现状（`FILE_LIST` > `CF_UNICODETEXT`）。

#### A3. 粘贴流程（`backend/src/session.rs` + `plugin.rs`）

新增 `paste_text`，替换 `type_text` 的 RPC 入口：

1. `clipboard_sync == false` 或通道未 ready → 走现有 `inject_text`，返回 `{ mode: "typed", typed, truncated }`。
2. 否则：
   - 保存 `local_text`，设置 `pending_paste = true`；
   - 发送 `ClipboardMessage::SendInitiateCopy([CF_UNICODETEXT])`；
   - 在 `runtime` 上等待 `paste_waiters`（1.2s 超时）；
   - 收到 `ok=true` → 用 `input_db` 发 `Ctrl+V`（`Operation::KeyPressed/Released`，scancode `0x1D` 与 `0x2F`），返回 `{ mode: "clipboard" }`；
   - 超时或 `ok=false` → `inject_text` 降级，返回 `{ mode: "typed", ... }`。
3. 整个流程复用现有 `typing` 互斥锁，避免并发粘贴交错。

`plugin.rs` 的 `rdp/paste` 改为调用 `session.paste_text(text)`；`PasteResult` 增加 `mode: "clipboard" | "typed"`。

#### A4. 前端（`src/bridge.ts`、`src/App.vue`、`src/i18n.ts`）

- `PasteResult` 增加 `mode`。
- `pasteText` 根据 `mode` 显示：
  - `clipboard`：`app.pasteClipboardDone`（新增）；
  - `typed`：保留 `app.pasteSent` / `app.pasteTruncated`。
- 粘贴按钮与 `Cmd+V` 流程不变；`Ctrl+V` 在 macOS 仍按远程 Ctrl+V 转发。
- 新增 i18n 中英文案。

#### A5. 测试

- Rust：`on_format_data_request` 返回本地文本；超长截断；未 ready 时降级；`on_format_list_response(false)` 触发超时降级（可用 mock sender）。
- 前端：`pasteText` 的 mode 分支（可抽纯函数）。

---

### Phase B：显示管线（P0）

**目标**：按真实脏区域发送，避免外接矩形放大；给全帧降级加明确阈值；暴露统计与缩放。

#### B1. 多脏矩形（`backend/src/session.rs`）

```rust
struct FrameState {
    // 替换 dirty: Option<Rect>
    dirty: Vec<Rect>,        // 上限 16
    dirty_area: u64,
    // ...
}
```

- `apply_pixels` 后 `add_dirty(rect)`：
  - 丢弃空矩形；
  - 与已有矩形相交/相邻（间距 ≤ 2px）时合并；
  - 超过 16 个时，合并“合并后面积增量最小”的一对；
  - `dirty_area` 按合并后的面积重算。
- 全帧条件改为 `dirty_area >= desktop_area * 85%`（替换 60% 并集逻辑）；`resize` 和首帧仍强制全帧。
- 保持 `last_good`、`skip_black_full` 行为不变，避免回归 0.2.0 的 resize 黑屏修复。

#### B2. 发送节奏（`pump_frame`）

- 每次 tick 遍历 `dirty`，每个矩形单独发一条 `FRAME_TYPE_BITMAP`，`seq` 每条递增；`resize` 只发一次。
- 单 tick 总负载上限 8 MiB；超出的矩形留在 `dirty` 里下个 tick 继续。这样小更新稳定 30fps，大更新不会堵住输出锁。
- 全帧仍受 `FULL_FRAME_MIN_INTERVAL` 限制，但仅在“确实接近全屏变化”时触发；普通滚动由多矩形覆盖。
- 日志增加 `rects` 计数：`frames/full/rects/bytes`。

#### B3. 统计与缩放（前端）

- 后端每秒发 `rdp/stats`：

```json
{ "connectionId": "...", "fps": 28, "fullFps": 0, "kbps": 512, "rects": 34, "width": 1920, "height": 1080 }
```

- `src/bridge.ts` 增加类型与监听；`App.vue` 工具栏增加可切换的统计浮层（fps、带宽、分辨率、矩形数）。
- 缩放：工具栏增加 `适应窗口 / 100% / 125% / 150%`；`RemoteCanvas.vue` 的 stage 使用 CSS `width/height` + 居中滚动，不改桌面分辨率请求；输入坐标仍按 stage 实际尺寸换算（现有逻辑已基于 `getBoundingClientRect`，无需改协议）。
- 缩放状态写入 `storage`，每个连接记住。

#### B4. 测试

- Rust：`add_dirty` 的合并、上限、85% 全帧阈值；`chunk_rect` 预算；`seq` 递增。
- 前端：`formatStats`、缩放比例映射的纯函数测试。

---

### Phase C：RDPDR 驱动器重定向（P1）

**目标**：把本地目录映射为远程 `\\tsclient\DBX RDP`，文件在 Explorer 里直接复制，替代不稳定的“注入远程剪贴板文件”。

#### C1. 依赖与特性（`backend/Cargo.toml`）

在现有 `ironrdp` git 依赖上增加：

- meta 特性：`client-rdpdr`
- 直接依赖（同一 rev）：`ironrdp-rdpdr`、`ironrdp-rdpdr-native`
- 保留 `client-clipboard`，两条文件路径并存

#### C2. 后端（新增 `backend/src/drives.rs`）

```rust
pub fn build_factory(root: &Path) -> Result<Box<dyn RdpdrBackendFactory + Send>, String> {
    #[cfg(any(target_os = "macos", target_os = "linux"))]
    { Ok(Box::new(NixRdpdrBackendFactory::new(root_string).with_drive(1, "DBX RDP".into()))) }

    #[cfg(windows)]
    { WindowsRdpdrBackendFactory::new(RedirectedDrive::new(1, "DBX RDP", root, false)?) ... }
}
```

- 根目录：
  - 默认 `DBX_PLUGIN_DATA_DIR/shared`，启动时 `create_dir_all`，Unix `0700`；
  - 允许连接配置指定绝对路径，必须存在且是目录；相对路径/空串忽略并回退默认值。
- `session.rs`：
  - `builder.with_rdpdr(true)`；
  - `RdpClient::new(...).with_rdpdr_backend_factory(factory)`。
- 会话启动失败（例如目录无权限）→ 记录日志并禁用 RDPDR，不阻断画面连接。

#### C3. 配置与 UI（`manifest.json`、`credentials.rs`、`App.vue`、`i18n.ts`）

新增连接字段：

- `share_local_folder`（boolean，默认 false）
- `shared_folder_path`（text，可选，占位 `/Users/you/Shared`）
- 说明文案：开启后远程可在 `\\tsclient\DBX RDP` 读写该目录，默认目录位于插件数据目录。

工具栏在共享开启且已连接时显示 `\\tsclient\DBX RDP` 徽标 + 提示；README 更新 Scope。

#### C4. 安全与测试

- 默认关闭；默认共享目录只包含插件数据，不暴露 home。
- 路径校验：拒绝相对路径、`..`、非目录；日志不输出文件内容。
- Rust 测试：路径校验、默认目录创建、工厂构建（macOS 可跑）；Windows 分支仅编译验证。
- 保留现有拖拽/发送文件/保存文件流程作为 fallback，不删除。

---

### Phase D：RDPSND 音频（P1）

**目标**：远程声音在 sidecar 用系统音频输出播放。

#### D1. 依赖

- 仅非 Linux 目标启用 `ironrdp-client/sound`：

```toml
[target.'cfg(not(target_os = "linux"))'.dependencies]
ironrdp-client = { git = "...", rev = "...", features = ["sound"] }
```

- Linux 目标不启用，避免 `cpal`/ALSA 构建依赖；代码用 `cfg` 屏蔽音频开关，并在 `rdp/security` 返回 `audioSupported: false`。
- 需要 `ironrdp-cfg`（同一 rev）以使用 `AudioMode`。

#### D2. 会话（`session.rs`、`credentials.rs`）

- `RdpConnectOptions` 增加 `audio_playback: bool`（默认 true）。
- `ConfigBuilder::with_audio_mode(if audio { AudioMode::RedirectToClient } else { AudioMode::Disabled })`。
- 不支持音频的平台忽略该字段并记录一次日志。

#### D3. 配置与 UI

- `manifest.json` 新增 `audio_playback`（boolean，默认 true）+ zh-CN。
- `rdp/security`（或 attach 响应）返回 `audioSupported`；前端在不支持时隐藏/禁用开关。
- 首版不提供运行时静音/音量，断线重连生效；README 说明。

#### D4. 验证

- macOS 本机连接测试服务器，远程播放提示音/视频，确认本地出声。
- 多会话同时播放不崩溃（cpal 设备争用只记录日志）。
- 关闭开关后服务端不协商 AUDIO_PLAYBACK。

---

### Phase E：版本、文档与发布

- `manifest.json`、`package.json`、`backend/Cargo.toml` 版本统一 `0.3.0`。
- README：
  - 新增“共享文件夹”“音频”“缩放/统计”“真实剪贴板”说明；
  - Scope 移除“audio and drive redirection are out of scope”。
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

- 产出 `dist/io.github.lwclick.rdp-0.3.0-<target>.dbxp` 并记录 SHA-256。

---

## 四、决策与假设

1. **不修改 IronRDP**：本轮不使用 H.264/AVC；EGFX 仍走非 AVC 的 Progressive/ClearCodec。H.264 需要改 `ironrdp-client` 的 `GraphicsPipelineClient::new(..., None)` 传 decoder，放入后续 backlog。
2. **剪贴板以文本为主**：DBX Host API 只有 `readText/writeText`，无法把远程图片写入系统剪贴板；远程图片后续做“另存为 PNG”，不承诺系统剪贴板图片。
3. **文件传输双轨**：保留现有 CLIPRDR 文件流；RDPDR 作为更稳的目录共享，不删除旧路径。
4. **共享目录默认私有**：默认插件数据目录，自定义路径需用户显式填写；不做“映射所有磁盘”。
5. **音频平台差异**：macOS/Windows 启用 cpal；Linux 本轮禁用音频，避免 ALSA 构建依赖。
6. **版本**：新功能按 minor 版本 `0.3.0`，不修改 `agents/versions.json`（不涉及）。
7. **不自动 commit/push**：完成实现与验证后由用户决定。

---

## 五、实施顺序与提交粒度

按以下顺序落地，每阶段结束都跑对应检查，保持可回退：

1. Phase A（剪贴板）→ 单独验证 → 可独立发布 0.3.0-alpha。
2. Phase B（显示管线）→ 单独验证 → 与 A 合并为 0.3.0。
3. Phase C（RDPDR）→ 若 Windows/Linux 构建或真实环境不通过，回退该阶段，不阻塞 A/B 发布。
4. Phase D（音频）→ 若 cpal 在目标平台构建/运行失败，回退该阶段，A/B/C 仍可发布。
5. Phase E（版本、README、打包）。

不自动 `git commit` / `git push`；每个阶段完成后展示改动与验证结果，由用户决定提交时机。

## 六、验证步骤

### 6.1 自动化

```bash
cd /Users/zero/GitHub/dbx-plugin-rdp
cargo fmt --manifest-path backend/Cargo.toml -- --check
cargo clippy --manifest-path backend/Cargo.toml --all-targets -- -D warnings
cargo test --manifest-path backend/Cargo.toml
npm run typecheck
npm test
```

### 6.2 真实 DBX 0.6.52+ 人工验收

先回归 0.2.0 未完成的项：

1. `Cmd+V` / 粘贴按钮注入 1 万字符中文 + 换行 + Tab 到记事本。
2. 拖拽 /「发送文件」→ 远程 Explorer `Ctrl+V`，文件名、扩展名、目录结构正确。
3. 远程复制文件 → 横幅 →「保存到本机」，单文件与多文件 ZIP 各一次。
4. 远程复制纯文本，确认不出现“文件名/无后缀”。
5. 另一台 RDP 客户端顶号 → 提示「会话已在其他位置登录」→「重新连接」可接管 →「关闭标签页」可用。
6. 服务器不支持文件剪贴板时的降级提示。

本轮新增：

7. 本地复制 5 万字符文本 → `Cmd+V` → 记事本内容完整、无逐字延迟；Toast 显示“已通过剪贴板粘贴”。
8. 模拟剪贴板通道不可用（关闭剪贴板同步）→ 同一粘贴自动降级为注入，内容正确。
9. 快速滚动/拖动窗口/播放远程视频 → 统计浮层 fps 与 kbps 无异常尖峰；日志 `rects` 明显大于全帧数。
10. 切换 `100% / 125% / 150%` 缩放 → 画面清晰、鼠标坐标无偏移。
11. 开启共享文件夹 → 远程 Explorer 出现 `\\tsclient\DBX RDP`，双向复制 1 GB 文件成功；关闭后不出现。
12. macOS 远程播放提示音 → 本地出声；关闭音频开关后无声、服务端不协商音频。
13. 断开/重连/关闭标签页后无残留暂存文件、无残留 helper 进程。

### 6.3 回归风险检查

- resize 黑屏修复（0.2.0）不得回退：`skip_black_full`、`last_good` 逻辑保持。
- 固定分辨率 + CSS 缩放模式不受多矩形改动影响。
- 文件剪贴板与 RDPDR 同时开启时，两条路径互不干扰。
- 证书固定、密码提示、断开原因文案不变。

---

## 七、风险与未完成

- **Linux 音频**：本轮禁用；若后续要启用，需要 CI 安装 `libasound2-dev` 并验证 cpal 运行时。
- **RDPDR 路径安全**：`ironrdp-rdpdr-native` 的 Nix 后端直接拼接服务器路径；共享目录必须专用、默认不在 home，避免暴露敏感文件。
- **剪贴板握手时序**：`SendInitiateCopy` 后必须等 `CB_RESPONSE_OK` 再发 `Ctrl+V`；否则远程应用可能粘贴旧内容。超时降级要覆盖。
- **WebView 绘制瓶颈**：多矩形减少的是传输量，`putImageData` 仍是主线程成本；若仍卡顿，下一轮再做 tile 化/压缩，不在本轮猜测优化。
- **包体积**：RDPDR + 音频会增大 sidecar；发布前记录 0.2.0 与 0.3.0 体积对比。
- **未纳入本轮**：RD Gateway、HTTP/SOCKS Proxy、H.264/AVC、多显示器、智能卡、打印机、麦克风、远程图片剪贴板。
- **测试环境清理**：`/tmp/dbx-rdp-dev/connections.json` 仍含明文测试密码，实施前建议删除并轮换测试账号。
