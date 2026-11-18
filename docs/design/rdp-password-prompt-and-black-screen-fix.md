# RDP 插件：重复密码提示与黑屏修复计划

## 结论摘要

两个问题都是**真实 bug**，不是你的设置问题：

1. **重复提示密码**：DBX 0.6.51 传给插件的 `connection.password` 是**空字符串**，真实密码在 `connection.connection_secrets.password`。当前代码用 `connection.get("password")...or_else(secrets...)`，空字符串会命中 `Some("")`，导致 `or_else` 不执行，密码被当成空值，插件因此每次都走 `host/requestUserInput` 询问密码。
   - 证据：本机 DBX 数据库中该连接存在 `plugin_connection.password` 密文，解密后长度与 16 位密码一致；`config_json` 中 `"password":""`。
2. **连接后黑屏**：IronRDP 的 `DesktopUpdate` 是**按脏矩形紧凑打包**的像素，当前已安装的 0.1.0 构建仍按整屏 stride 读取局部更新，导致除首帧外的更新被丢弃/错位。真实 Windows 桌面大量使用局部更新，因此停在首帧的黑色画面。
   - 证据：工作区已修复该问题（`apply_pixels` 按矩形读取），但本机安装的 sidecar 构建于修复之前（安装 10:35，修复后重新打包 10:39，二进制哈希不同）。

## 现状与证据

- 本机 DBX 版本 0.6.51；插件版本 0.1.0 已安装，sidecar 进程仍在运行。
- 连接配置：`security_mode = nla`、`certificate_policy = prompt`、`ask_password = false`、`save_password = true`。
- `known_hosts.json` 已固定目标服务器证书，测试时可复用该指纹，不触发证书确认。
- 目标服务器 3389 端口可达。
- DBX 0.6.51 源码确认：`plugin_connection_params()` 把整个 `ConnectionConfig` 序列化为 `params.connection`，其中 `password` 为空字符串、`connection_secrets` 为敏感值映射。

## 代码修改

### 1. 修复密码回退逻辑

文件：`backend/src/credentials.rs`（约 109-114 行）

把“非空密码字段”作为回退条件，空字符串和 DBX 占位符都视为缺失：

```rust
let password = connection
    .get("password")
    .and_then(Value::as_str)
    .filter(|value| !is_absent_password(value))
    .or_else(|| {
        secrets
            .get("password")
            .and_then(Value::as_str)
            .filter(|value| !value.is_empty())
    })
    .unwrap_or_default()
    .to_string();
```

新增辅助函数：

```rust
fn is_absent_password(value: &str) -> bool {
    value.is_empty() || value.starts_with("__DBX_SAVED_CREDENTIAL__:")
}
```

### 2. 补回归测试

文件：`backend/src/credentials.rs` 的 `tests` 模块：

- `prefers_saved_secret_when_password_field_is_empty`：`connection.password = ""` + `connection_secrets.password = "secret"`，断言取到 secret。
- `ignores_saved_credential_placeholder`：`connection.password = "__DBX_SAVED_CREDENTIAL__:x"` + secret，断言取到 secret。
- 保留现有 `DOMAIN\user`、UPN、IPv6 pin key 测试。

### 3. 确认并保留黑屏修复

文件：`backend/src/session.rs` 的 `FrameState::apply_pixels`：

- 源数据按 `rect.width` 行宽读取（区域紧凑打包），目标按桌面 stride 写入。
- 已有回归测试 `packed_region_is_written_at_its_offset`，本次继续保留。

### 4. 版本升级

为避免 DBX 因同版本包不可覆盖而拒绝安装：

- `manifest.json`：`0.1.0` → `0.1.1`
- `package.json`：`0.1.0` → `0.1.1`
- `backend/Cargo.toml`：`0.1.0` → `0.1.1`

不改 `agents/versions.json`（不涉及 DBX 官方驱动）。

## 真实服务器验证

凭据只通过环境变量传入临时测试脚本，**不写入仓库、不写入日志、不出现在计划文件**。测试账号是管理员，只做连接、查看画面、右键、截图、断开，不做任何修改操作。

### A. Sidecar 级别前后对比（核心验证）

临时脚本：`/tmp/rdp_live_e2e.mjs`（复用现有 `/tmp/rdp_e2e.mjs` 的帧解析逻辑，不提交）。

测试数据目录：`/tmp/dbx-rdp-live`，预置从本机 DBX `known_hosts.json` 复制的证书 pin（不打印）。

用例：

1. **旧构建复现密码 bug**：已安装的 0.1.0 sidecar + `connection.password=""` + `connection_secrets.password=<env>` → 预期返回 `-32001`（无 UI 可提示密码），证明密码未从 secret 读取。
2. **新构建验证修复**：新 0.1.1 sidecar + 同样的 payload → 预期 `connection/connect` 成功，无密码提示。
3. **黑屏前后对比**：
   - 旧构建 + `connection.password=<env>`（绕过密码 bug）连接，等待 10-15 秒，记录帧；预期首帧后无有效局部更新（全黑/停在首帧）。
   - 新构建同样流程；预期持续收到局部帧，且至少一个非全屏帧的采样像素非黑。
4. **输入与右键**：新构建发送鼠标移动/右键，等待局部更新；验证右键菜单区域产生非黑局部帧。
5. **证书固定**：`rdp/security` 返回的 SHA-256 必须与预置 pin 完全一致。
6. **NLA**：先按 `security_mode=nla` 测试；若 CredSSP 被服务器拒绝，再以 `tls` 做隔离测试，并在报告中区分是服务器策略还是插件问题。
7. `connection/disconnect` 正常返回，进程退出。

### B. UI 级别验证（DBX 开发宿主）

1. 用 `dbx-plugin dev`（Node 22，端口 5190）启动本项目，使用临时数据目录。
2. Playwright 打开开发宿主，新建指向目标服务器的 RDP 连接，密码填入表单。
3. 连接后截图，确认不是黑屏；右键画布，确认出现远程上下文菜单（验证输入坐标与局部帧渲染）。
4. 验证完成后停止开发宿主，删除临时数据目录（含明文开发凭据）。

### C. 本机 DBX 安装验证

1. 重新打包：`dist/io.github.lwclick.rdp-0.1.1-darwin-arm64.dbxp`。
2. 交付用户在插件中心安装 0.1.1（覆盖 0.1.0）。
3. 安装后断开/重连一次，确认：
   - 不再询问密码；
   - 画面正常刷新，不再黑屏；
   - 证书 pin 仍然生效（无需重新确认）。
4. 如 DBX 缓存旧 sidecar，重启 DBX 后再验证。

## 验证命令

```bash
# Rust
PATH="$HOME/.cargo/bin:$PATH" cargo fmt --check
PATH="$HOME/.cargo/bin:$PATH" cargo clippy --all-targets
PATH="$HOME/.cargo/bin:$PATH" cargo test

# UI
PATH="$HOME/.local/share/mise/installs/node/22.13.0/bin:$PATH" npm run typecheck
PATH="$HOME/.local/share/mise/installs/node/22.13.0/bin:$PATH" npm test -- --run
PATH="$HOME/.local/share/mise/installs/node/22.13.0/bin:$PATH" npm run package
```

## 决策与约束

- 不修改 DBX 主仓库，只改插件仓库 `~/GitHub/dbx-plugin-rdp`。
- 不自动 commit/push；验证通过后展示 diff，等你确认再提交。
- 测试凭据仅用于本次验证，结束后删除临时脚本、pin 副本、开发宿主数据目录。
- 若真实服务器要求交互式登录或策略限制导致自动化无法继续，会立即停止并报告，不尝试绕过。

## 风险

- NLA/CredSSP 行为依赖服务器配置；若服务器只允许特定安全层，会在报告中明确。
- 真实服务器画面可能停在锁屏/登录页，右键局部帧断言可能不适用；此时以“持续收到非黑全帧 + UI 截图”为准，并说明限制。
- 管理员账号测试有权限风险，整个验证只做只读观察。
