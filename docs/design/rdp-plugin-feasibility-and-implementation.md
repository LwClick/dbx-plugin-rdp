# DBX RDP 插件可行性与实现计划

## 1. 结论

可行。实现方式为「DBX 插件工作台 + Rust 原生 Sidecar」：

- Sidecar 内嵌 [IronRDP](https://github.com/Devolutions/IronRDP) 作为纯 Rust RDP 客户端，负责 TCP、TLS、CredSSP/NLA、图形解码、剪贴板与输入编码。
- 插件 UI 在 DBX 沙箱 iframe 中用 `<canvas>` 显示远程桌面，通过 Host API 的 `sendBinary` / `onBinary` 与 Sidecar 交换帧和输入事件。
- 同一份插件源码按平台构建原生二进制，覆盖 macOS（arm64/x64）、Windows x64、Linux x64/arm64；单个 `.dbxp` 只包含当前平台二进制，商店按平台选择产物。
- 密码不经过 Workbench UI，也不进入连接 JSON：使用 Manifest 的 `binding: "secret"`，由 DBX Secret Store 以 `dbxenc1`（AES-256-GCM）信封加密，密钥放在系统钥匙串（macOS Keychain / Windows Credential Manager / Linux Secret Service）。
- 证书默认严格校验；自签名证书通过 Sidecar 调用 `host/requestUserInput` 让用户确认指纹，并按 `host:port` 固定（TOFU），指纹变化时失败关闭。
- 首版包含：画面、键鼠、动态分辨率、NLA、证书指纹确认、文本剪贴板双向；不包含多显示器、音频、文件传输/磁盘映射。

`mstsc` / Windows App / `xfreerdp` 外置客户端方案也可用，但无法嵌入 DBX 标签页，三平台体验不一致，且临时 `.rdp` 文件容易泄露凭据，因此不采用。`ironrdp-web`（WASM + WebSocket 代理）需要插件 UI 直连本地 WebSocket，而插件沙箱只允许显式声明的 HTTPS `connect-src`，也不采用。

## 2. 现状与依据

### 2.1 源码基线

- 本机已安装 DBX `0.6.51`（`/Applications/DBX.app`）。
- `~/GitHub/dbx` 工作区原基线为 `0.5.59`，且当前分支 `codex/feat/sql-quoted-variable-substitution` 有未提交改动。
- 已执行 `git fetch origin main`：`origin/main` = `b160d130`，`package.json` 版本 `0.6.51`，与已安装应用一致。
- 新插件框架只存在于 0.6.x 上游：
  - `plugins/sdk/rust/dbx-plugin-sdk`：Sidecar SDK，支持 `stdio-framed`、`PluginEmitter::binary`、`handle_binary`、`host/requestUserInput`。
  - `plugins/sdk/cli`：`dbx-plugin create/dev/package/keygen`，npm 包 `@dbx-app/plugin-cli@0.1.9`。
  - `plugins/examples/hello-workbench`：完整的 connection-provider + Sidecar + Workbench + secret 示例。
  - `crates/dbx-plugin-runtime`：Host 侧插件运行时。
  - `apps/desktop/src/lib/plugins/pluginHostBridge.ts`：UI 桥、二进制桥、`reopenConnection`。
- 本地 `~/GitHub/dbx` 不包含上述新框架（仅有旧 JDBC 插件），因此插件不能基于当前工作区源码开发。

### 2.2 插件协议关键约束

- Manifest v1 字段不接受未声明字段；`entrypoints.backend.transport` 使用 `stdio-framed` 才能传二进制。
- UI 桥单条二进制上限 **8 MiB**；Sidecar 单帧上限 **64 MiB**；超过需分块。
- Sidecar 是「每插件一个进程」，不是每标签页一个；插件自行管理连接会话表。
- `connection/connect` 的 `connection` 参数才会带补齐后的 secrets；Workbench context 只含 `connectionId` / `providerId` / `connectionType`，不含密码。
- `window.dbxPlugin.reopenConnection(connectionId)` 可由 Workbench UI 触发宿主完整重连流程（允许弹密码框），适合「重连」按钮。
- `window.dbxPlugin.clipboard.readText()` 需要 `host.clipboard:read` 权限并有用户确认；写剪贴板 `copy` 不需要权限。
- 证书/密码等交互可用 `host/requestUserInput`；宿主会暂停该请求的超时计时。

参考：
- [插件开发文档](https://dbxio.com/cn/docs/plugin-development)
- [plugins/README.md](https://github.com/t8y2/dbx/blob/main/plugins/README.md)
- [hello-workbench](https://github.com/t8y2/dbx/tree/main/plugins/examples/hello-workbench)
- [pluginHostBridge.ts](https://github.com/t8y2/dbx/blob/main/apps/desktop/src/lib/plugins/pluginHostBridge.ts)

### 2.3 Secret 存储现状（0.6.51）

上游 `crates/dbx-core/src/persistence/secret_codec.rs`：

- 新值以 `dbxenc1...` 信封保存，使用 **AES-256-GCM**。
- 32 字节本地密钥保存在操作系统凭据库：macOS Keychain、Windows Credential Manager、Linux Secret Service。
- Headless/特殊部署支持 `DBX_SECRET_KEY`、`DBX_SECRET_KEY_FILE`；旧安装有 per-user key file 兼容回退。
- 插件连接 secret 使用 `plugin_connection.<key>` 前缀，写入 `connection_secrets`，不写入 `config_json`；云同步默认只同步脱敏占位，只有配置同步口令时才进入加密同步载荷。

因此 RDP 密码应走 `binding: "secret"`，插件不应自行落盘密码。

### 2.4 IronRDP 能力确认

- `ironrdp-client` 0.1.0（2026-07 发布）是 **library-only** 可嵌入客户端：
  - `RdpClient::new(config, output_tx)` + `with_desktop_updates()` 输出 `DesktopUpdate`（紧凑脏矩形，`0x00RRGGBB`）。
  - `RdpInputSender` 支持 fast-path 键鼠、`Resize`、`MonitorLayout`、`Clipboard`。
  - `with_cliprdr_backend_factory` 可注入自定义 CLIPRDR 后端（我们用它把剪贴板桥到插件 UI，不依赖 OS 剪贴板）。
  - `ConfigBuilder` 支持 `with_credssp`、`with_tls`、`with_certificate_validation(Strict)`、`with_certificate_validation_callback`、`with_pointer_software_rendering`、`with_auto_reconnect`。
  - 动态分辨率走 Display Control；不可用时返回 `DisplayResizeFallback`。
- 纯 Rust、MIT/Apache-2.0，可在 macOS/Windows/Linux 编译；NLA 由 `sspi` 实现。
- 输出帧为 32 位 `0x00RRGGBB` 像素；插件转换为 RGBA8 后发送。1080p 全屏约 **8.29 MiB**，略高于 UI 桥 8 MiB，因此必须分块。

## 3. 选定方案

| 决策 | 选择 |
| --- | --- |
| 客户端形态 | 内嵌 RDP（IronRDP Sidecar + Canvas） |
| 首版范围 | 画面、键鼠、动态分辨率、NLA、证书指纹确认、文本剪贴板双向 |
| 仓库 | 独立仓库 `~/GitHub/dbx-plugin-rdp` |
| 当前 dbx 仓库 | 只 fetch + 可选 worktree 参考，不改当前分支/未提交改动 |
| UI 技术 | Vue 3 + TypeScript + Vite（构建输出到 `ui/`） |
| 传输 | Sidecar `stdio-framed`；固定二进制通道 + 32 字节头 |
| 加密 | DBX Secret Store（AES-256-GCM + OS Keychain） |
| 证书 | 默认 `Strict`；无效证书 TOFU 指纹确认并固定 |
| 分发 | 各平台 `.dbxp`，GitHub Actions 矩阵构建；官方商店由 dbx-store 审核签名 |

## 4. 目标架构与数据流

```text
┌──────────────────────────── DBX Host (0.6.51) ────────────────────────────┐
│ Connection dialog ──secret──> connection_secrets (AES-256-GCM + Keychain)  │
│ Workbench iframe (sandbox)                      PluginHostBridge           │
│  ┌──────────────────────────┐   invoke/notify    ┌──────────────────────┐  │
│  │ Vue UI                   │<──────────────────>│ dbx-plugin-runtime   │  │
│  │  canvas / input / status │   binary (<=8 MiB) │  JSONRPC + framed    │  │
│  └──────────────────────────┘                    └──────────┬───────────┘  │
└─────────────────────────────────────────────────────────────┼──────────────┘
                                                              │ stdin/stdout
                                                   ┌──────────▼───────────┐
                                                   │ Rust Sidecar         │
                                                   │  session registry    │
                                                   │  IronRDP RdpClient   │
                                                   │  shadow framebuffer  │
                                                   │  cert pin store      │
                                                   └──────────┬───────────┘
                                                              │ TCP 3389 (可直接或经 DBX SSH/SOCKS 隧道)
                                                       ┌──────▼──────┐
                                                       │ Windows RDP │
                                                       └─────────────┘
```

关键流程：

1. 用户保存连接：DBX 将 `password` 写入 `connection_secrets`，`config_json` 不含密码。
2. 用户「保存并打开」/侧栏连接：DBX 调用 Sidecar `connection/test` → `connection/connect`，payload 含补齐后的连接与 `runtime.host/port`（DBX 传输层之后的端点）。
3. Sidecar 用 `runtime.host/port` 建 RDP 会话，等待 `RdpOutputEvent::Connected` 后返回成功；会话保存在 Sidecar 的 registry 中。
4. DBX 打开 Workbench，context 只有 `connectionId`。UI 调 `rdp/attach` 获取 `sessionId`、桌面尺寸和证书信息。
5. Sidecar 的帧泵把脏矩形转成 RGBA，按 32 字节头 + ≤4 MiB 分块经 `rdp-frame-v1` 发出；UI 写入持久 `ImageData`，rAF 中 `putImageData` 到 canvas。
6. UI 把键鼠事件编码为 `rdp-input-v1` 二进制；Sidecar 解码为 `ironrdp_input::Operation`，`Database::apply` 生成 fast-path 事件交给 `RdpInputSender`。
7. 远程复制：CLIPRDR 后端 `on_remote_copy` → `SendInitiatePaste` → `on_format_data_response` 提取 Unicode 文本 → JSON 事件 `rdp/clipboard` → UI `dbxPlugin.copy(text)`。
8. 本地粘贴：UI 读本地剪贴板 → `rdp/paste` → Sidecar 更新本地文本缓存、发 `SendInitiateCopy`、等待约 120 ms 后发送 Ctrl+V。

## 5. 详细实施步骤

### Phase 0：基线更新与工具链

不修改当前 `~/GitHub/dbx` 工作树（保留未提交改动）。

1. 已完成的 fetch 保留；创建只读参考工作树（可选但推荐）：
   ```bash
   git -C /Users/zero/GitHub/dbx worktree add /Users/zero/GitHub/dbx-main origin/main
   ```
   用于查阅 `plugins/sdk`、`crates/dbx-plugin-runtime`、`docs/content/docs/plugin-development.cn.mdx`。
2. 安装 Node 22（当前 mise 只有 Node 16）；在插件仓库用 `.mise.toml` / `.nvmrc` 固定版本，避免影响其他项目：
   ```bash
   mise install node@22.13.0
   # 在 ~/GitHub/dbx-plugin-rdp/.mise.toml 写： [tools] node = "22.13.0"
   mise exec node@22.13.0 -- node -v   # v22.13.0
   ```
3. 安装插件 CLI（用 Node 22）：
   ```bash
   mise exec node@22.13.0 -- npm install --global @dbx-app/plugin-cli
   mise exec node@22.13.0 -- dbx-plugin --version
   ```
4. Rust 工具链：本机 `~/.cargo/bin/cargo 1.97.0`，满足 IronRDP 的 1.94+ 要求；如 PATH 未包含，使用绝对路径或在插件仓库 `rust-toolchain.toml` 固定 `1.97.0`。
5. 创建独立仓库：
   ```bash
   mkdir -p ~/GitHub/dbx-plugin-rdp
   cd ~/GitHub/dbx-plugin-rdp
   git init
   ```
6. 用官方模板生成骨架（生成后覆盖 UI/Manifest）：
   ```bash
   dbx-plugin create ~/GitHub/dbx-plugin-rdp \
     --template rust \
     --id io.github.lwclick.rdp \
     --name "DBX RDP" \
     --publisher lwclick \
     --description "Remote Windows desktops over RDP inside DBX." \
     --version 0.1.0 --yes
   ```

### Phase 1：插件骨架与 Manifest

目标目录：

```text
dbx-plugin-rdp/
├── manifest.json
├── dbx-plugin.toml
├── package.json                 # UI 构建（Vue + Vite）
├── index.html                   # Vite 源入口
├── vite.config.ts
├── tsconfig.json
├── assets/
│   ├── plugin.svg
│   └── connection.svg
├── ui/                          # Vite 输出，gitignore
├── src/                         # UI 源码
├── backend/
│   ├── Cargo.toml
│   ├── rust-toolchain.toml
│   └── src/
├── tests/                       # UI 单测
└── .github/workflows/plugin-release.yml
```

`dbx-plugin.toml`：

```toml
schema_version = 1

[backend]
language = "rust"
directory = "backend"
binary = "dbx-rdp-sidecar"

[package]
include = ["assets", "ui"]

[dev]
ui_build = ["npm", "run", "build"]
ui_watch = ["npm", "run", "build:watch"]
```

`manifest.json` 关键内容：

- `id`: `io.github.lwclick.rdp`
- `name`: `DBX RDP`
- `version`: `0.1.0`
- `publisher`: `lwclick`
- `engines`: `{ "dbx": ">=0.6.51", "host_api": "^1.0" }`
- `permissions`: `["host.binary", "host.events", "host.clipboard:read", "host.storage"]`
- `entrypoints.backend`: `{ "protocol_versions": [1], "transport": "stdio-framed", "executable": "bin/dbx-rdp-sidecar" }`
- `entrypoints.ui`: `{ "root": "ui", "entry": "ui/index.html" }`
- `contributions`:
  1. `connection-provider` `io.github.lwclick.rdp.connection`
     - `database_type`: `rdp`
     - `workbench`: `io.github.lwclick.rdp.workbench`
     - `capabilities`: `["test", "connect", "disconnect"]`
     - 字段见下表
     - `actions`: `reset-certificate-pin`（`when: "always"`, `requires_valid_form: false`, `variant: "secondary"`）
  2. `workbench` `io.github.lwclick.rdp.workbench`

连接字段：

| key | 类型 | binding | 默认 | 说明 |
| --- | --- | --- | --- | --- |
| `display_name` | text | name | Windows RDP | 连接名 |
| `host` | text | host | — | 主机/IP |
| `port` | number | port | 3389 | 端口 |
| `username` | text | username | — | 支持 `DOMAIN\user`、`user@domain`、纯用户名 |
| `password` | password | secret | — | 留空则连接时询问 |
| `domain` | text | config | — | 可选 NetBIOS 域 |
| `ask_password` | boolean | config | false | 每次连接都询问，不读取已存密码 |
| `security_mode` | select | config | `nla` | `nla` / `tls`（legacy，仅证书登录） |
| `certificate_policy` | select | config | `prompt` | `prompt`（指纹确认）/ `strict` |
| `clipboard_sync` | boolean | config | true | 文本剪贴板双向同步 |
| `performance_profile` | select | config | `balanced` | `balanced` / `quality` / `speed` |
| `connect_timeout_secs` | number | config | 20 | 宿主会同步该超时到 `connection/test`/`connect` |

`localizations.zh-CN` 覆盖名称、字段、动作文案；后端弹窗使用中英双语（后端拿不到 UI locale 的保证）。

### Phase 2：Rust Sidecar

#### 2.1 依赖

`backend/Cargo.toml`：

```toml
[package]
name = "dbx-rdp-sidecar"
version = "0.1.0"
edition = "2024"
license = "Apache-2.0"

[workspace]

[dependencies]
dbx-plugin-sdk = "..."          # 由 dbx-plugin create 生成，保持模板给出的解析方式
ironrdp-client = { version = "0.1", features = ["rustls", "clipboard"] }
ironrdp-input = "0.7"
ironrdp-pdu = "0.9"
ironrdp-cliprdr = "0.7"
serde = { version = "1", features = ["derive"] }
serde_json = "1"
sha2 = "0.10"
x509-cert = "0.3"
zeroize = "1"
tokio = { version = "1", features = ["rt-multi-thread", "sync", "time", "net", "macros"] }
tracing = "0.1"
tracing-subscriber = { version = "0.3", features = ["env-filter"] }
thiserror = "2"

[profile.release]
strip = true
lto = true
codegen-units = 1
```

注意：直接依赖必须与 `ironrdp-client` 0.1 的传递版本一致（pdu 0.9 / input 0.7 / cliprdr 0.7），用 `Cargo.lock` 锁定，避免类型不匹配。

#### 2.2 模块划分

- `main.rs`：安装 `tracing-subscriber`（日志只写 stderr），启动 `PluginServer`，注册 `RdpPlugin`。
- `plugin.rs`：实现 `PluginHandler::handle` / `handle_binary`；持有全局 `PluginState`。
- `state.rs`：`HashMap<connection_id, Arc<RdpSession>>`、`AtomicU32` sessionId、全局 Tokio runtime、首次调用时保存 `PluginEmitter` 克隆。
- `session.rs`：`RdpSession` 生命周期、`RdpClient` 配置、输出事件消费、阴影帧缓冲、帧泵。
- `protocol.rs`：帧头与输入消息的编码/解码、分块、常量。
- `clipboard.rs`：`CliprdrBackendFactory` / `CliprdrBackend` 文本实现。
- `cert.rs`：指纹计算、pin store、`host/requestUserInput` 确认。
- `credentials.rs`：用户名/域解析、密码提示、`zeroize`。
- `errors.rs`：`ConnectorError` → 用户可读错误码/文案；不包含凭据。

#### 2.3 RPC 方法

| 方法 | 参数 | 返回/行为 |
| --- | --- | --- |
| `connection/test` | 标准生命周期 payload | 临时建立 RDP 会话并立即关闭；`{success,message}`；超时 `connect_timeout_secs` |
| `connection/connect` | 标准生命周期 payload，secrets 已补齐 | 建立会话并等待 `Connected`；`{success:true}` |
| `connection/disconnect` | `{connection:{id}}` | `request_close()` + 等待退出；`{success:true}` |
| `connection/action` | `action.id == "reset-certificate-pin"` | 删除 `host:port` 的 pin；`{success:true,message}` |
| `rdp/attach` | `{connectionId, viewport:{width,height}, scaleFactor?}` | `{sessionId, width, height, certificate?}`；无会话返回 `-32010` |
| `rdp/detach` | `{connectionId, sessionId}` | 停止向该 viewer 推帧 |
| `rdp/resize` | `{connectionId, width, height}` | 夹到 200–8192、宽度取偶；发送 `RdpInputEvent::Resize`；`{accepted,fallback?}` |
| `rdp/requestFullFrame` | `{connectionId}` | 标记全屏脏区，下一帧发完整帧 |
| `rdp/paste` | `{connectionId, text}` | 更新本地文本缓存 → `SendInitiateCopy` → 约 120 ms 后发 Ctrl+V |
| `rdp/clipboardText` | `{connectionId, text}` | 只更新缓存并 `SendInitiateCopy`，不发送按键 |
| `rdp/security` | `{connectionId}` | 返回 `{nla, tlsVersion?, cipher?, certificate?}`，用于安全面板 |

事件（Sidecar → UI，需 `host.events`）：

- `rdp/state` `{connectionId, state: "connecting"|"connected"|"reconnecting"|"disconnected"|"failed", error?, errorCode?}`
- `rdp/clipboard` `{connectionId, text}`
- `rdp/desktop` `{connectionId, width, height}`（与二进制 resize 帧互为补充）

错误码约定：

- `-32010` 会话不存在/未连接
- `-32011` 参数非法
- `-32012` 证书被拒绝或指纹不匹配
- `-32013` 认证失败（密码/域/账号）
- `-32014` 连接超时
- `-32015` 不支持的安全模式

#### 2.4 会话建立

`connection/connect` 内：

1. 解析 `connection.id`、`host`、`port`、`username`、`domain`、`password`、`external_config`。
2. 若 `ask_password == true` 或密码为空：调用 `host_client().request_user_input(UserInputPrompt::secret(...))`；取消/超时按失败关闭。
3. 用户名解析：
   - `DOMAIN\user` → `username=user`, `domain=DOMAIN`
   - `user@domain` → `with_username("user@domain")`，不另设 domain
   - 否则 `with_username(user)` + 可选 `with_domain(domain)`
4. `ConfigBuilder`：
   - `Destination::from_parts(host, port)`，必须使用 `runtime.host/port`，不要自己重建 DBX 隧道。
   - `with_credssp(security_mode == nla)`；`with_tls(security_mode == tls)`。
   - `with_certificate_validation(CertificateValidation::Strict)`。
   - `with_certificate_validation_callback(...)`（见 2.7）。
   - `with_desktop_width/height`：默认 1280×800，attach 后按 viewport 调整。
   - `with_desktop_scale_factor(100)`。
   - `with_client_name("DBX-RDP")`、`with_autologon(true)`。
   - `with_pointer_software_rendering(true)`（光标合成进帧，避免处理 pointer shape/overlay）。
   - `with_clipboard(if clipboard_sync { Enable } else { Disable })`。
   - `performance_profile`：`quality` 关闭降级，`balanced` 关闭壁纸/动画，`speed` 再关闭字体平滑/主题。
5. 创建 `output_channel(32)`，`RdpClient::new(...)`，随后：
   - `let input_sender = client.input_sender();`
   - 如启用剪贴板：`client.with_cliprdr_backend_factory(Box::new(RdpClipboardFactory::new(input_sender.clone(), clipboard_event_tx)))`
   - `client.with_desktop_updates().with_auto_reconnect(3).run()`
6. 输出消费者等待 `Connected` / `ConnectionFailure` / `Terminated`，通过 `std::sync::mpsc` 把结果返回给同步的 `handle` 调用（带 `connect_timeout_secs` 超时）。
7. 密码副本在构建 Config 后 `zeroize`；Config 内的凭据在会话销毁时释放（自动重连期间保留）。

自动重连与终止：

- 输出消费者收到 `RdpOutputEvent::AutoReconnecting { response, attempt, maximum_attempts }`：5 秒内回复 `AutoReconnectDecision::Continue`（最多 3 次），并向 UI 发 `rdp/state: reconnecting`。
- `AutoReconnected` → `rdp/state: connected`；`ConnectionFailure` / `Terminated` → `rdp/state: failed` 或 `disconnected`，保留错误供 UI 展示。
- `connection/disconnect` 或 UI 关闭时调用 `input_sender.request_close()`，等待 `Terminated`；超时 2 秒后 abort 任务。
- `connection/connect` 进行中收到 disconnect → 通过 close signal 取消，并返回取消错误。

Viewer 约定：每个连接首版只允许一个活动 viewer；新的 `rdp/attach` 替换旧 viewer。旧标签页收到 `rdp/state: disconnected` 或 `-32010`，提示在新标签页中查看。

`connection/test` 复用同一 helper，但使用随机/临时 sessionId，成功后 `request_close()` 并等待退出，不写入 registry。

#### 2.5 帧管线

阴影帧缓冲：

```rust
struct FrameState {
    width: u16,
    height: u16,
    rgba: Vec<u8>,        // width * height * 4
    dirty: Option<Rect>,
    full: bool,
    seq: u32,
    generation: u64,      // resize 后递增，旧帧作废
}
```

- 输出消费者收到 `RdpOutputEvent::DesktopUpdate(update)`：
  - 从 `update.buffer()` 的 `0x00RRGGBB` 转 RGBA，写入 `rgba` 对应区域。
  - 扩展 `dirty` 并合并矩形；若区域超过屏幕 60%，标记 `full`。
  - 收到 `Image`（未启用 desktop updates 时的兜底）→ 全屏写入并标记 `full`。
- 帧泵每 33 ms：
  - 无 viewer 或 `dirty` 为空 → 跳过。
  - 取脏区（或全屏），按行切成 ≤4 MiB 的块；每块从 `rgba` 复制。
  - 通过 `PluginEmitter::binary("rdp-frame-v1", &chunk)` 发送；发送失败/无 viewer → 丢弃并等待下一次全量请求。
  - `seq` 自增；`chunkIndex/chunkCount` 标识分块。
- 新 viewer attach、`rdp/requestFullFrame`、resize、UI 报告丢帧 → `full = true`。
- 旧 viewer 的 `sessionId` 不匹配时 UI 丢弃帧。

二进制帧协议（`rdp-frame-v1`，大端，32 字节头）：

| 偏移 | 类型 | 字段 |
| --- | --- | --- |
| 0 | u8 | magic `0xD1` |
| 1 | u8 | version `1` |
| 2 | u8 | type：1=bitmap，2=resize，3=desktop-info |
| 3 | u8 | flags（保留，0） |
| 4 | u32 | sessionId |
| 8 | u32 | seq |
| 12 | u16 | desktopWidth |
| 14 | u16 | desktopHeight |
| 16 | u16 | x |
| 18 | u16 | y |
| 20 | u16 | width |
| 22 | u16 | height |
| 24 | u16 | rowStart（相对 y） |
| 26 | u16 | chunkIndex |
| 28 | u16 | chunkCount |
| 30 | u16 | reserved |
| 32 之后 | bytes | RGBA8，`width × rows`，`rows = height - rowStart` |

- 分块按整行切分，单块 payload ≤ 4 MiB（1080p 全屏约 2–3 块）。
- 传输层已给出整帧长度，因此头里不再放 `payloadLen`；接收端校验 `data.length - 32 == width × rows × 4`。
- `type=2` 时 payload 为空，`desktopWidth/Height` 为新尺寸；UI 重建 canvas/ImageData。
- `type=3` 时 payload 为 UTF-8 JSON（桌面名、证书指纹、TLS 信息）。

#### 2.6 输入管线

二进制通道 `rdp-input-v1`，头 8 字节：

| 偏移 | 类型 | 字段 |
| --- | --- | --- |
| 0 | u8 | magic `0xD2` |
| 1 | u8 | version `1` |
| 2 | u8 | type |
| 3 | u8 | flags |
| 4 | u32 | sessionId |

type：

1. key：`[u8 code, u8 extended, u8 pressed]`
2. mouse move：`[u16 x, u16 y]`
3. mouse button：`[u8 button, u8 pressed, u16 x, u16 y]`
4. wheel：`[u8 vertical, i16 delta, u16 x, u16 y]`
5. unicode：`[u32 codepoint, u8 pressed]`
6. releaseAll：无 payload
7. focus sync：`[u8 scrollLock, u8 numLock, u8 capsLock, u8 kanaLock]`

Sidecar 处理：

- 校验 sessionId 与长度；非法消息丢弃并限流日志。
- 映射为 `ironrdp_input::Operation`：
  - key → `KeyPressed/KeyReleased(Scancode::from_u8(extended, code))`
  - mouse → `MouseMove/MouseButtonPressed/MouseButtonReleased/WheelRotations`
  - unicode → `UnicodeKeyPressed/UnicodeKeyReleased`
- `Database::apply([op])` 得到 `SmallVec<[FastPathInputEvent; 2]>`，通过 `input_sender.try_send(RdpInputEvent::FastPath(events))`。
- 队列满时丢弃鼠标移动并记限流日志；键事件尽量重试一次。
- `releaseAll` 调 `Database::release_all()`，用于窗口失焦，避免卡键。
- 鼠标绝对坐标由 UI 按远端分辨率换算；Sidecar 不再做缩放。

#### 2.7 证书校验与指纹固定

- `certificate_policy == "strict"`：标准链/主机名校验失败即拒绝，不弹窗。
- `certificate_policy == "prompt"`：使用 `with_certificate_validation(Strict)` + 回调：
  1. 解析 DER（`x509-cert`）取 subject/issuer/validity；SHA-256 指纹。
  2. 读 `$DBX_PLUGIN_DATA_DIR/known_hosts.json`（0600，原子写：临时文件 + rename）。
  3. 已有 pin 且匹配 → 接受。
  4. 已有 pin 但不匹配 → 拒绝，错误 `-32012`，提示「证书已变化，请确认后重置信任」。
  5. 无 pin → `host_client().request_user_input(UserInputPrompt::choice(...))`，展示：
     ```
     RDP 服务器证书未受信任 / Untrusted RDP server certificate
     Host: host:port
     SHA-256: AA:BB:...
     Subject: ...
     Issuer: ...
     Valid until: ...
     ```
     选项 `accept`（信任并继续 / Trust and continue）、`reject`（取消 / Cancel）。
  6. 用户接受 → 写入 pin 并接受本次握手；拒绝/取消/无 UI（`-32001`）→ 拒绝。
- pin 结构（key 为 `host:port`，IPv6 用 `[addr]:port`）：
  ```json
  {
    "version": 1,
    "entries": {
      "host:port": {
        "sha256": "hex",
        "subject": "...",
        "issuer": "...",
        "acceptedAt": "2026-10-22T00:00:00Z"
      }
    }
  }
  ```
- `reset-certificate-pin` 动作删除对应条目并返回提示。
- 回调运行在 RDP 连接任务中；使用 multi-thread Tokio runtime（≥2 worker），避免阻塞整个 runtime。`request_user_input` 的阻塞有 300 s 上限，宿主在弹窗期间暂停连接超时。

#### 2.8 文本剪贴板

实现 `CliprdrBackendFactory` + `CliprdrBackend`（仅文本）：

- `client_capabilities()`：只声明文本相关能力（不声明文件/图片）。
- `temporary_directory()`：`$DBX_PLUGIN_DATA_DIR/clipboard`（创建，0600；文件传输 v1 不支持）。
- `on_ready` / `on_request_format_list`：如果已有本地文本缓存，发送 `ClipboardMessage::SendInitiateCopy(vec![CF_UNICODETEXT])`。
- `on_remote_copy(formats)`：若含 `CF_UNICODETEXT`，发送 `SendInitiatePaste(CF_UNICODETEXT)`。
- `on_format_data_request`：用缓存文本构造 `FormatDataResponse::new_unicode_string`，发送 `SendFormatData`。
- `on_format_data_response`：解析 Unicode 文本 → 通过 `std::sync::mpsc`/`tokio::mpsc` 交给会话任务 → `emitter.event("rdp/clipboard", {connectionId, text})`。
- `on_file_contents_*` / lock/unlock：返回错误或忽略，不实现文件传输。
- 文件/图片格式不声明，避免远端误以为支持。
- UI 收到 `rdp/clipboard` 且 `clipboard_sync` 开启 → `dbxPlugin.copy(text)`，toast 提示。
- 本地 → 远程：
  - 工具栏「发送剪贴板」→ 读本地文本 → `rdp/clipboardText`。
  - `Ctrl+V`（canvas 聚焦且同步开启）→ `preventDefault` → `readText()` → `rdp/paste`；失败则回退发送普通 Ctrl+V。
  - `rdp/paste` 后端先更新缓存、`SendInitiateCopy`，延迟 120 ms 再发 Ctrl+V；延迟值在真实 Windows 上验证后可调。
- 若 `clipboard_sync == false`：`ClipboardType::Disable`，UI 隐藏剪贴板控件，不申请 `host.clipboard:read` 的读取（权限仍在 manifest 中，但未调用）。

#### 2.9 日志与错误

- Sidecar 的 stdout 只允许协议帧；禁止 `println!`、`dbg!` 或其他 stdout 输出，否则会破坏 framed 流。
- `tracing` 只写 stderr；默认 `info`，不输出 `connection_secrets`、密码、剪贴板文本、帧内容。
- 错误映射：
  - NLA/凭据失败 → 「用户名、密码或域不正确，或账号无远程登录权限」
  - 连接拒绝 → 「目标未开启远程桌面或端口不可达」
  - 超时 → 「连接超时，请检查网络/防火墙/跳板」
  - 证书 → 「证书不受信任/指纹变化」
  - 会话数限制 → 「已达到远程会话数上限」
- 未知错误保留技术细节在 `details` 字段，UI 可折叠展示；不得包含凭据。

### Phase 3：Vue UI

#### 3.1 文件

```text
index.html
src/main.ts
src/App.vue
src/bridge.ts            # window.dbxPlugin 封装、能力探测
src/frameProtocol.ts     # 32 字节头解析、分块、丢帧检测
src/RemoteCanvas.vue     # canvas、ImageData、rAF 绘制、缩放
src/input.ts             # 键鼠/滚轮/IME → rdp-input-v1
src/scancodes.ts         # 复用 IronRDP Web 的 MIT/Apache 映射表并注明来源
src/clipboard.ts         # 本地/远程剪贴板流程
src/SecurityPanel.vue    # NLA/TLS/证书指纹/重置入口提示
src/i18n.ts              # zh-CN/en
src/styles.css           # 使用 dbxPlugin.theme.tokens
tests/*.spec.ts
```

#### 3.2 初始化

```ts
await window.dbxPlugin.ready;
const context = window.dbxPlugin.context; // { connectionId, providerId, connectionType, surface }
const capabilities = window.dbxPlugin.capabilities;
```

- `connectionId` 不存在：显示「请从连接列表打开 RDP 连接」。
- `rdp/attach` 返回 `-32010`：显示「连接已断开」并提供「重连」按钮：
  ```ts
  await window.dbxPlugin.reopenConnection(connectionId);
  // 轮询/重试 rdp/attach，最多 3 次，间隔 500ms
  ```
- `onContext` 更新连接时重新 attach；`onInit` 后重新 attach（iframe 重载由宿主 repush 连接）。
- `window.addEventListener("beforeunload", () => notify("rdp/detach", ...))`。

#### 3.3 Canvas 渲染

- 维护：
  - `remoteWidth/Height`
  - `ImageData`（RGBA，大小 = remoteWidth × remoteHeight）
  - `dirty` 矩形
  - `lastSeq`
- 收到 `rdp-frame-v1`：
  1. 校验 magic/version/type/长度/sessionId。
  2. `seq` 不连续（且不是首帧）→ 设 `awaitingFull=true`，调用 `rdp/requestFullFrame`，丢弃当前分块。
  3. 同一 `seq` 的分块未全部到齐就收到新 `seq` → 同样请求全量。
  4. 分块写入 `imageData.data` 的对应偏移。
  5. 合并 dirty，`requestAnimationFrame` 中 `ctx.putImageData(imageData, 0, 0, dirty.x, dirty.y, dirty.w, dirty.h)`。
- `type=2`：重建 canvas backing store 和 `ImageData`，清空 dirty，重置 `lastSeq=0`。
- `type=3`：更新安全面板信息。
- CSS：canvas 按容器等比缩放（`object-fit: contain` 等价布局），黑底；`cursor: none` 当远程光标合成在帧内且指针在 canvas 上。
- 尺寸策略：
  - attach 时取容器 CSS 像素（clamp 1024×768 – 2560×1600）。
  - ResizeObserver 300 ms 防抖 → `rdp/resize`；宽度取偶、高度 ≥200。
  - `DisplayResizeFallback` 事件 → 提示「服务器不支持动态分辨率，保持当前分辨率」，不自动重连。
- 缩放模式：`fit`（默认）/ `100%`；设置写入 `dbxPlugin.storage`。

#### 3.4 输入

- canvas 可聚焦；点击后 focus，失焦时发送 `releaseAll` + focus sync。
- `keydown/keyup`：
  - `event.isComposing` 或 `compositionend` → Unicode 路径（发送每个码点 press/release）。
  - 已知 `event.code` → `scancodes.ts` 转 u16（bit 0xE000 表示 extended）→ type 1。
  - 未知 code 且 `key.length === 1` → Unicode。
  - 对已处理按键 `preventDefault()`；保留 F12 等开发键。
- `mousemove` 用 rAF 节流；坐标 = `(clientX - rect.left) / rect.width * remoteWidth`，clamp。
- `mousedown/mouseup` 映射 0/1/2/3/4 → Left/Middle/Right/X1/X2。
- `wheel`：`deltaY` 归一化为 ±120/档；Shift+滚轮 → 水平。
- 提供工具栏按钮：`Ctrl+Alt+Del`、`Win`、`Esc`、`Tab`（远程全屏应用退出用）。
- IME：隐藏 `<textarea>` 捕获 composition；`compositionend` 后逐码点发送 Unicode，避免中文输入丢失。

#### 3.5 剪贴板

- 初始化时探测 `capabilities.clipboardRead`；无权限/不支持则禁用本地→远程读取，保留远程→本地写入。
- `rdp/clipboard` 事件：
  - 同步开启 → `dbxPlugin.copy(text)`；toast「已从远程复制」。
  - 文本长度上限 1 MiB，超出截断并提示。
- 本地→远程：
  - 工具栏按钮调用 `clipboard.readText()`。
  - `Ctrl+V` 拦截逻辑见 2.8；`event.ctrlKey && event.key.toLowerCase() === "v"`。
  - 读取失败（权限拒绝/空）→ 回退发送普通 Ctrl+V。
- 设置项「剪贴板同步」读写 `dbxPlugin.storage`，并在 UI 上说明读取本地剪贴板需要授权。

#### 3.6 状态与安全面板

- 顶栏：连接名、状态点、分辨率、延迟（RPC 往返，可选）、重连、断开（调用 `connection/disconnect`? UI 无直接 RPC；可 `dbxPlugin.notify`? 没有 disconnect API；用 `reopenConnection` 或关闭标签。MVP：只提供「重连」和「关闭」；断开从侧栏连接菜单执行）。
- 安全面板：显示 `security_mode`、NLA 是否启用、TLS 版本/加密套件、证书 SHA-256/Subject/Issuer/有效期；提供「重置证书信任」提示（实际动作在连接编辑对话框）。
- 错误覆盖层：错误标题 + 可展开 details + 「重连」/「复制错误」。
- 主题：读取 `dbxPlugin.theme.tokens`，映射 `--color-*` 到 CSS 变量；监听 `onInit` 的主题更新。

#### 3.7 i18n

- `zh-CN` / `en`；`window.dbxPlugin.locale` 决定默认；设置里可切换。
- 后端弹窗中英双语；Manifest `localizations` 覆盖字段/贡献点。

### Phase 4：打包与 CI

- `ui/` 加入 `.gitignore`；`npm run build` 生成。
- `package.json` scripts：
  ```json
  {
    "build": "vite build",
    "build:watch": "vite build --watch",
    "typecheck": "vue-tsc --noEmit",
    "test": "vitest run",
    "package": "npm run build && dbx-plugin package ."
  }
  ```
- Vite 配置：
  ```ts
  build: { outDir: "ui", emptyOutDir: true },
  plugins: [vue(), { name: "dbx-build-signal", closeBundle() { console.log("DBX_UI_BUILD_SUCCESS"); } }]
  ```
- GitHub Actions 使用 `plugins/sdk/cli/templates/common/github/plugin-release.yml` 的可复用工作流，矩阵：
  - `macos-14`（darwin-arm64）
  - `macos-13`（darwin-x64）
  - `windows-2022`（windows-x64）
  - `ubuntu-22.04`（linux-x64）
  - `ubuntu-22.04-arm`（linux-arm64，如 runner 可用）
- `package-command`：`npm ci && npm run build && dbx-plugin package .`
- 发布前本地验证：
  ```bash
  npm ci
  npm run typecheck && npm test && npm run build
  cd backend && cargo fmt --check && cargo clippy --all-targets -- -D warnings && cargo test
  cd .. && dbx-plugin package .
  ```
- 不提交 `dist/`、`ui/`、`.dbx-dev/`、签名私钥。

### Phase 5：安全加固清单

1. 密码只存 `connection_secrets.password`；`config_json`、云同步明文、日志、事件、错误中均不得出现。
2. `ask_password` 或空密码时使用 `host/requestUserInput`（`echo=false`），取消/超时失败关闭。
3. 连接建立后对密码副本 `zeroize`；避免 `Debug`/`Clone` 泄露；Sidecar 不写任何凭据文件。
4. `security_mode` 默认 NLA；Legacy TLS 需用户显式选择并给出警告；不提供无 TLS 的 RDP Security。
5. 证书默认 `Strict`；自签名必须用户确认指纹；pin 不匹配失败关闭；pin 文件 0600。
6. 剪贴板默认开启但只支持文本；本地读取需要 `host.clipboard:read` 和宿主确认；不记录剪贴板内容。
7. 权限最小化：`host.binary`、`host.events`、`host.clipboard:read`、`host.storage`；不申请 `host.data:read`、`host.ai`、文件系统、网络 origin。
8. 帧/输入消息做长度、类型、sessionId、尺寸上限校验；单块 ≤4 MiB，桌面 ≤8192×8192。
9. Sidecar 崩溃/异常退出由宿主传播；UI 显示错误并提供重连；不自动无限重试（最多 3 次）。
10. 分发：开发期只安装未签名本地包；正式包由 dbx-store 审核并用 Ed25519 仓库密钥签名；文档提示 native sidecar 以当前用户权限运行，只安装可信插件。
11. macOS/Windows 发布包尽量做代码签名/公证；至少提供 SHA-256 校验。

### Phase 6：测试与验证

#### 6.1 Rust 单元测试

- `protocol.rs`：帧头编解码、分块行对齐、payload 长度校验、非法尺寸。
- `input.rs`：七种输入消息编解码、越界/截断、releaseAll。
- `session.rs`：脏矩形合并、全屏阈值、u32→RGBA 转换、seq 递增。
- `cert.rs`：pin store 读写/原子替换、匹配/不匹配、0600。
- `credentials.rs`：`DOMAIN\user`、`user@domain`、纯用户名、空密码。
- `clipboard.rs`：Unicode 文本编码/解码、文件格式拒绝。

#### 6.2 Rust 集成测试（不依赖真实 Windows）

- dev-only `rdp-test-server` 二进制，使用 `ironrdp-server`：
  - 自签名证书（`rcgen`），合成帧（渐变 + 移动方块），记录键鼠事件。
  - 侧车以 `certificate_validation=prompt` + 测试钩子自动接受，连接 `127.0.0.1:<port>`。
  - 断言：`Connected`、收到全屏帧、键鼠事件到达、`rdp/resize` 触发服务器尺寸变化、剪贴板文本往返。
- 该测试可在 macOS/Linux/Windows CI 运行，作为真实 Windows 之外的回归保护。

#### 6.3 UI 单元测试（Vitest）

- `frameProtocol`：32 字节头、分块、seq 缺口 → 请求全量、resize 重建。
- `input`：坐标缩放（fit/100%）、滚轮方向、Ctrl+V 拦截/回退。
- `scancodes`：代表性键（字母、方向、F1–F12、修饰键、扩展键）。
- `clipboard`：权限拒绝、空文本、超长文本。
- `i18n`：中英文 key 完整。

#### 6.4 真实环境验收（必须）

在 DBX 0.6.51 中安装 `dist/*.dbxp`（开启「允许安装未签名开发包」），连接真实 Windows：

1. 本地账号 + 域账号 NLA 登录成功；错误密码/域返回可读错误。
2. 自签名证书首次弹窗；接受后重连不再提示；更换证书后连接失败；重置 pin 后重新提示。
3. 记事本输入英文/中文（IME）、快捷键、方向键、F 键、Ctrl+Alt+Del 按钮。
4. 鼠标移动/点击/右键/滚轮/横向滚动坐标准确。
5. 拖拽窗口改变 DBX 标签尺寸，远程分辨率动态变化；不支持时提示 fallback。
6. 远程复制中文 → 本地粘贴；本地复制中文 → 远程 Ctrl+V；关闭同步后剪贴板隔离。
7. 关闭/重开 Workbench，连接保持；点击重连恢复；侧栏断开后 Workbench 显示断开并可重连。
8. 杀掉 Sidecar 进程：宿主报错，UI 显示失败，重连成功。
9. 30 分钟滚动/视频/大窗口拖动：内存稳定、无持续增长、无卡键。
10. 检查 `config.json` 无密码；检查 Secret Store 值为 `dbxenc1...`；搜索 Sidecar stderr/DBX 日志无密码、无剪贴板内容。
11. Windows 与 Linux 客户端至少各做一次真实连接（可用 CI 产物 + 手动安装）；macOS 本地必须完成。

#### 6.5 回归检查

- 插件安装/卸载不影响 DBX 已有连接、SQL、SSH、AI 等功能。
- 未安装插件时 DBX 启动/连接流程不变。
- 插件不作为 DBX 主仓库改动，无需回归主应用测试；但需验证 0.6.51 宿主兼容（`engines.dbx` 下限）。
- 若后续修改 DBX Host/SDK/文档，才在 `t8y2/dbx` 提 PR；插件功能代码留在独立仓库。

## 6. 假设与默认值

| 项 | 值 |
| --- | --- |
| 插件 ID / publisher | `io.github.lwclick.rdp` / `lwclick`（可在发布前改） |
| 连接类型 | `rdp` |
| 默认端口 | 3389 |
| 默认分辨率 | 1280×800，attach 后按容器调整，clamp 1024×768–2560×1600 |
| 帧率 | 30 fps（脏区驱动） |
| 单块大小 | 4 MiB（UI 桥 8 MiB 上限的一半） |
| 连接超时 | 20 s（`connect_timeout_secs`） |
| 自动重连 | 3 次（IronRDP `with_auto_reconnect(3)`） |
| 证书策略 | `prompt` |
| 安全模式 | `nla` |
| 剪贴板 | 文本双向，默认开启 |
| 日志级别 | `info`，仅 stderr |
| UI 构建 | Vue 3 + Vite，输出 `ui/` |
| 支持平台 | macOS arm64/x64、Windows x64、Linux x64/arm64 |

## 7. 风险与缓解

| 风险 | 影响 | 缓解 |
| --- | --- | --- |
| `ironrdp-client` 0.1 较新，API 可能变动 | 编译/行为变化 | 锁定 0.1.x + Cargo.lock；封装在 `session.rs`，必要时回退到 `ironrdp-session` 自建循环 |
| Linux WebKitGTK 大帧性能 | 4K 卡顿 | 默认 1080p/30fps、脏区、4 MiB 分块、rAF；提供 100%/fit 缩放 |
| NLA/Kerberos 环境差异 | 部分企业环境登录失败 | 优先 NTLM/NLA；明确错误提示；Kerberos/RD Gateway 作为二期 |
| 证书回调阻塞等待用户 | 连接线程阻塞 | multi-thread runtime；宿主弹窗暂停超时；300 s 上限；无 UI 失败关闭 |
| 剪贴板粘贴时序 | 首次 Ctrl+V 可能为空 | 先 `SendInitiateCopy` 再延迟 120 ms 发送按键；真实 Windows 调参；失败回退 |
| UI 桥 8 MiB / Sidecar 64 MiB | 大帧被拒 | 32 字节头 + 4 MiB 分块；丢帧检测 + 全量重发 |
| 无真实 Windows 自动化环境 | 回归不足 | `ironrdp-server` 回环测试 + 手动真实 Windows 验收；CI 覆盖三平台编译/单测 |
| 原生 Sidecar 权限 | 插件可访问用户文件/网络 | 最小权限、代码审查、官方商店签名；文档提示只装可信插件 |
| 未签名开发包 | 供应链风险 | 仅本地测试；正式分发走 dbx-store 审核签名 |
| 多显示器/音频/文件传输缺失 | 功能不完整 | 明确 v1 范围；后续按 IronRDP 能力迭代 |

## 8. v1 不做

- 多显示器布局、音频重定向、RDPDR 磁盘映射、CLIPRDR 文件复制。
- RemoteApp/RAIL、RD Gateway、智能卡、摄像头、USB、打印、动态 DPI 缩放因子。
- RDP UDP/Soft-Sync、会话录制、凭据托管/轮换、JumpServer 等堡垒机专用认证。
- 现有 DBX 内置连接类型改造（不新增 DBX 数据库枚举，不改 Host）。

## 9. 实施顺序（可执行）

1. Phase 0：Node 22、CLI、独立仓库、`dbx-plugin create --template rust`。
2. 替换 Manifest / `dbx-plugin.toml` / UI 构建配置；先跑通纯 UI 在 `dbx-plugin dev` 中打开。
3. 后端最小闭环：`connection/test` + `connection/connect` + `rdp/attach` + 全屏帧（静态色块）→ UI canvas 显示。
4. 接入 IronRDP `DesktopUpdate` 脏区 + 分块 + 输入键鼠。
5. 证书指纹确认 + pin store + `reset-certificate-pin`。
6. 文本剪贴板双向。
7. 动态分辨率、重连、错误映射、安全面板、i18n。
8. 单元/集成测试、打包、在 DBX 0.6.51 中安装并做真实 Windows 验收。
9. 配置 GitHub Actions 三平台矩阵，产出各平台 `.dbxp` 候选包。
