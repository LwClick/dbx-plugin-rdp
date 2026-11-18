import { api } from "./bridge";

export const UPLOAD_CHUNK_BYTES = 512 * 1024;
export const DOWNLOAD_CHUNK_BYTES = 1024 * 1024;

export interface RemoteFileEntry {
  name: string;
  relativePath?: string | null;
  size: number;
}

export interface UploadFile {
  handleId: string;
  name: string;
  size: number;
  relativePath?: string;
}

export interface FileProgress {
  sent: number;
  total: number;
}

export function fileTransferAvailable(): boolean {
  const transfer = api().fileTransfer;
  return Boolean(
    transfer &&
      typeof transfer.pick === "function" &&
      typeof transfer.read === "function" &&
      typeof transfer.beginSave === "function",
  );
}

let tokenCounter = 0;

export function createToken(): string {
  const generator = typeof crypto !== "undefined" ? crypto : undefined;
  if (generator && typeof generator.randomUUID === "function") {
    return generator.randomUUID().replace(/-/g, "");
  }
  tokenCounter += 1;
  const random = Math.floor(Math.random() * 0xffffffff).toString(16);
  return `${Date.now().toString(16)}${tokenCounter.toString(16).padStart(4, "0")}${random.padStart(8, "0")}`;
}

const UNITS = ["B", "KB", "MB", "GB", "TB"] as const;

export function formatBytes(value: number): string {
  if (!Number.isFinite(value) || value <= 0) return "0 B";
  let size = value;
  let unit = 0;
  while (size >= 1024 && unit < UNITS.length - 1) {
    size /= 1024;
    unit += 1;
  }
  const rounded = size >= 10 || unit === 0 ? Math.round(size) : Math.round(size * 10) / 10;
  return `${rounded} ${UNITS[unit]}`;
}

/**
 * Copies the picked or dropped files into the sidecar, then advertises the
 * staged list on the remote clipboard. The user pastes in Explorer afterwards.
 */
export async function uploadFiles(
  connectionId: string,
  files: UploadFile[],
  onProgress?: (progress: FileProgress) => void,
  signal?: AbortSignal,
): Promise<number> {
  const transfer = api().fileTransfer;
  if (!transfer) throw new Error("This DBX host does not support file transfer");
  const token = createToken();
  const total = files.reduce((sum, file) => sum + file.size, 0);
  let sent = 0;
  let count = 0;
  try {
    for (const file of files) {
      if (signal?.aborted) throw new Error("Upload cancelled");
      const { index } = await api().invoke<{ index: number }>("rdp/files/upload/begin", {
        connectionId,
        token,
        name: file.name,
        relativePath: file.relativePath,
        size: file.size,
      });
      let offset = 0;
      for (;;) {
        if (signal?.aborted) throw new Error("Upload cancelled");
        const chunk = await transfer.read(file.handleId, offset, UPLOAD_CHUNK_BYTES);
        if (signal?.aborted) throw new Error("Upload cancelled");
        if (chunk.length > 0) {
          if (!chunk.dataBase64) throw new Error("Empty file chunk");
          await api().invoke("rdp/files/upload/chunk", {
            connectionId,
            token,
            index,
            offset,
            dataBase64: chunk.dataBase64,
          });
          offset += chunk.length;
          sent += chunk.length;
          onProgress?.({ sent, total });
        }
        if (chunk.eof) break;
        if (chunk.length === 0) throw new Error("Empty file chunk");
      }
      await api().invoke("rdp/files/upload/finish", { connectionId, token, index });
      count += 1;
    }
    await api().invoke("rdp/files/advertise", { connectionId, token });
    return count;
  } catch (failure) {
    await api().invoke("rdp/files/upload/cancel", { connectionId, token }).catch(() => undefined);
    throw failure;
  } finally {
    for (const file of files) {
      void transfer.cancel(file.handleId).catch(() => undefined);
    }
  }
}

/**
 * Streams a downloaded file or archive from the sidecar into a host save
 * handle. Returns false when the user dismissed the save dialog.
 */
export async function saveRemoteFile(
  connectionId: string,
  token: string,
  name: string,
  size: number,
  onProgress?: (progress: FileProgress) => void,
): Promise<boolean> {
  const transfer = api().fileTransfer;
  if (!transfer) throw new Error("This DBX host does not support file transfer");
  const target = await transfer.beginSave({ name, size });
  if (!target) {
    await api().invoke("rdp/files/release", { connectionId, token }).catch(() => undefined);
    return false;
  }
  let offset = 0;
  try {
    for (;;) {
      const chunk = await api().invoke<{ dataBase64: string; length: number; eof: boolean }>(
        "rdp/files/read",
        { connectionId, token, offset, length: DOWNLOAD_CHUNK_BYTES },
      );
      if (chunk.length > 0) {
        const written = await transfer.write(target.handleId, offset, chunk.dataBase64);
        offset =
          typeof written.nextOffset === "number" ? written.nextOffset : offset + chunk.length;
        onProgress?.({ sent: offset, total: size });
      }
      if (chunk.eof) break;
      if (chunk.length === 0) throw new Error("Empty download chunk");
    }
    await transfer.finish(target.handleId);
    return true;
  } catch (failure) {
    await transfer.cancel(target.handleId).catch(() => undefined);
    throw failure;
  } finally {
    await api().invoke("rdp/files/release", { connectionId, token }).catch(() => undefined);
  }
}
