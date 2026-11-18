# RDP 改分辨率后黑屏：分块帧解析缺陷修复计划

## 摘要

根因已定位：**1920×1080 首帧超过 4 MiB，被后端拆成 2 个分块，而前端把每个分块都按“矩形总高度”计算期望字节数，导致第一个分块被判定为长度错误并整帧丢弃**。连接后没有完整首帧可显示，所以黑屏；`fit` 模式约 1244×752（<4 MiB）只有 1 个分块，因此之前正常。

修复只改前端分块解析与消费逻辑，并补多分块回归测试；不改 DBX 宿主。

## 现状与根因

### 后端分块规则

- [protocol.rs](file:///Users/zero/GitHub/dbx-plugin-rdp/backend/src/protocol.rs#L367-L405)：`rows_per_chunk = MAX_CHUNK_BYTES / (width * 4)`，`MAX_CHUNK_BYTES = 4 MiB`。
- 1920×1080：每行 7680 字节，`rows_per_chunk = 546`，`chunk_count = 2`。
  - 第 1 块：`row_start=0`，`rows=546`，payload = 546 × 7680 = 4,193,280 字节。
  - 第 2 块：`row_start=546`，`rows=534`，payload = 534 × 7680 = 4,101,120 字节。
- [protocol.rs](file:///Users/zero/GitHub/dbx-plugin-rdp/backend/src/protocol.rs#L93-L126)：`FrameHeader` 只携带 `rect`、`row_start`、`chunk_index`、`chunk_count`，不携带当前分块的 `rows`。

### 前端错误假设

- [frameProtocol.ts](file:///Users/zero/GitHub/dbx-plugin-rdp/src/frameProtocol.ts#L54-L79)：

```ts
export function chunkRows(header: FrameHeader): number {
  return Math.max(0, header.height - header.rowStart);
}

export function expectedChunkBytes(header: FrameHeader): number {
  return header.width * chunkRows(header) * 4;
}
```

- 对第 1 块：`chunkRows = 1080 - 0 = 1080`，期望 8,294,400 字节；实际只有 4,193,280 字节 → `splitFrame` 抛 `FrameProtocolError`。
- [RemoteCanvas.vue](file:///Users/zero/GitHub/dbx-plugin-rdp/src/RemoteCanvas.vue#L269-L278)：即使 `splitFrame` 通过，也使用 `header.height - header.rowStart` 作为行数，对非最后一块同样错误。
- 结果：首帧两个分块都无法正确组装：

```ts
if (!imageData || !frameMatchesSurface(...)) { ... }
if (pending && header.seq !== pending.seq) { ... }
```

  `handleBinary` 在 `splitFrame` 抛错时直接 `return`；后续重试仍按同样方式失败，画面保持黑色。

### 为什么 fit 模式/开发宿主之前看起来正常

- `fit` 模式约 1244×752：单帧 3.74 MB < 4 MiB，`chunk_count = 1`，`chunkRows` 恰好等于整个矩形高度，所以解析通过。
- 开发宿主在 1920×1080 时首帧虽然被丢弃，但服务器后续的局部重绘（每个脏矩形 <4 MiB）逐步补全了桌面，截图看起来正常；真实 DBX 中如果桌面静止或后续更新同样跨多个分块，就会一直黑屏。

## 变更内容

### 1. `src/frameProtocol.ts`：按 payload 长度计算分块行数

- 删除 `chunkRows()` / `expectedChunkBytes()` 的旧语义，新增：

```ts
export function chunkRowCount(header: FrameHeader, payloadLength: number): number {
  const rowBytes = header.width * 4;
  if (rowBytes <= 0 || payloadLength <= 0 || payloadLength % rowBytes !== 0) {
    throw new FrameProtocolError(`RDP chunk payload is not row-aligned (${payloadLength} bytes)`);
  }
  const rows = payloadLength / rowBytes;
  if (header.rowStart + rows > header.height) {
    throw new FrameProtocolError(
      `RDP chunk rows ${rows} exceed rect height ${header.height} at row ${header.rowStart}`,
    );
  }
  return rows;
}
```

- `splitFrame()` 对 `FRAME_TYPE_BITMAP` 改为：

```ts
const rows = chunkRowCount(header, payload.length);
if (rows === 0 || header.chunkCount === 0) {
  throw new FrameProtocolError("RDP bitmap frame has no rows");
}
```

- 保留 `isFullDesktopFrame`、`frameMatchesSurface` 不变；分块协议版本不变，无需改后端。
- 更新导出，移除 `chunkRows` / `expectedChunkBytes`（仅测试和 `splitFrame` 使用）。

### 2. `src/RemoteCanvas.vue`：用实际 payload 行数绘制

- `handleBinary` 中：

```ts
const rows = chunkRowCount(header, fragment.payload.length);
if (rows <= 0 || header.width === 0) return;
const rowBytes = header.width * 4;
const destination = imageData.data;
...
markDirty(header.x, header.y + header.rowStart, header.width, rows);
```

- 删除原来的 `const rows = header.height - header.rowStart;` 和重复的 `rowBytes` 定义。
- 这样第 1 块绘制 546 行、第 2 块绘制 534 行，`pending.received` 正常累加到 `chunkCount`，首帧组装成功。

### 3. 测试

文件：`tests/frameProtocol.spec.ts`

- 删除 `expectedChunkBytes` 相关断言，改为 `chunkRowCount` 断言。
- 新增：
  - 1920×1080 首块：`rowStart=0`、payload=1920×546×4 → `chunkRowCount == 546`，`splitFrame` 不抛错。
  - 1920×1080 末块：`rowStart=546`、payload=1920×534×4 → `chunkRowCount == 534`。
  - 非行对齐 payload（如 `1920*546*4 - 4`）→ 抛 `FrameProtocolError`。
  - `rowStart + rows > height` → 抛 `FrameProtocolError`。
- 现有单块全帧、行对齐分块、坏 magic/version、resize 帧测试保留并适配。

### 4. 版本与构建

- `manifest.json`、`package.json`、`backend/Cargo.toml`：`0.1.3 → 0.1.4`，更新 `backend/Cargo.lock`。
- 重新执行 `npm run package`，产物：
  `dist/io.github.lwclick.rdp-0.1.4-darwin-arm64.dbxp`。
- README 不需要改（本次是解析缺陷修复，不新增功能）。

## 决策与假设

- **根因修复在前端解析层**：后端分块本身正确，协议头已有 `row_start`/`chunk_count`，payload 长度足以推导当前块行数；不引入新协议字段，不破坏现有帧格式。
- **不改 `MAX_CHUNK_BYTES`**：4 MiB 分块在修复后可正常组装；后续若真实 DBX 的 Tauri base64 桥接仍出现明显延迟，再单独评估降低分块大小，不在本次混入性能改动。
- **不改 DBX 宿主**：黑屏是插件前端解析缺陷，与宿主 tab/保存行为无关。

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
   重点确认新增的多分块首块/末块测试通过。
2. 开发宿主 + 真实服务器（`10.166.135.164`，NLA）：
   - 连接配置使用默认 1920×1080。
   - 连接后立即截图/检查 canvas：首帧应直接显示桌面，不再需要等待局部重绘“补黑屏”。
   - 打开/拖动一个大窗口，触发大面积脏矩形（全帧 2 个分块），画面持续正常。
   - 切换到 `fit` 模式，确认 1244×752 单块路径未回归。
   - 检查 `rdp.log`：`emit full ... 1920x1080` 后画面可见；无 `FrameProtocolError`。
3. 打包并检查：
   - `unzip -l` 确认 `0.1.4`、UI 资源、sidecar 均在包内。
   - 在真实 DBX 中安装 0.1.4 后复测黑屏问题；本机当前安装的是 0.1.2，需要用户手动安装新包。

## 风险与后续

- 本次修复覆盖所有“单帧超过 4 MiB”的分辨率（1920×1080、2560×1440 等）和任意 `chunk_count`。
- 若真实 DBX 仍偶发黑屏，下一步应排查 `dbx-plugin-binary` Tauri 桥接的 base64/IPC 丢帧（宿主侧 `Lagged` 日志），再考虑降低分块大小或改用更小的帧块；不在本次预设。
- 真实验证依赖测试服务器可用；若不可达，至少完成多分块单测与开发宿主验证，并在最终回复中说明。
