/// <reference types="vite/client" />

declare module "*.vue" {
  import type { DefineComponent } from "vue";
  const component: DefineComponent<Record<string, unknown>, Record<string, unknown>, unknown>;
  export default component;
}

interface DbxPluginStorage {
  get<T = unknown>(key: string): Promise<T | null>;
  set(key: string, value: unknown): Promise<void>;
  delete(key: string): Promise<void>;
}

interface DbxPluginClipboard {
  readText(): Promise<string>;
  writeText(text: string): Promise<void>;
  readImage?(): Promise<string>;
}

interface DbxPluginInit {
  capabilities?: Record<string, unknown>;
  locale?: string;
  theme?: { appearance?: string; tokens?: Record<string, string> };
  context?: Record<string, unknown>;
  contributionId?: string;
}

interface DbxPluginFileTransferRead {
  handleId: string;
  name: string;
  size: number;
  contentType?: string;
  relativePath?: string;
}

interface DbxPluginFileTransfer {
  pick(options: { multiple?: boolean }): Promise<{ files: DbxPluginFileTransferRead[] }>;
  read(
    handleId: string,
    offset: number,
    length?: number,
  ): Promise<{ dataBase64: string; length: number; eof: boolean }>;
  beginSave(options: {
    name: string;
    contentType?: string;
    size?: number;
  }): Promise<{ handleId: string; chunkBytes: number } | null>;
  write(handleId: string, offset: number, data: string | ArrayBuffer | Uint8Array): Promise<{ written: number; nextOffset: number }>;
  finish(handleId: string): Promise<void>;
  cancel(handleId: string): Promise<void>;
  onDrop(
    listener: (files: DbxPluginFileTransferRead[], info: { dropId: string; truncated: boolean }) => void,
  ): () => void;
  onDragState(listener: (active: boolean) => void): () => void;
}

interface DbxPluginApi {
  ready: Promise<DbxPluginInit | undefined>;
  contributionId?: string;
  context: Record<string, unknown>;
  locale: string;
  theme: { appearance?: string; tokens?: Record<string, string> };
  capabilities: Record<string, unknown>;
  request<T = unknown>(method: string, params?: unknown): Promise<T>;
  invoke<T = unknown>(method: string, params?: unknown, options?: { timeoutMs?: number }): Promise<T>;
  notify(method: string, params?: unknown): Promise<void>;
  sendBinary(channel: string, data: ArrayBuffer | Uint8Array): Promise<void>;
  onBinary(listener: (payload: { channel: string; data: Uint8Array }) => void): () => void;
  onEvent(listener: (event: { method: string; params: unknown }) => void): () => void;
  onContext(listener: (context: Record<string, unknown>) => void): () => void;
  onInit(listener: (init: DbxPluginInit) => void): () => void;
  storage?: DbxPluginStorage;
  clipboard?: DbxPluginClipboard;
  fileTransfer?: DbxPluginFileTransfer;
  copy?(text: string): Promise<void>;
  reopenConnection?(connectionId: string): Promise<{ ok?: boolean }>;
  setTitle?(title: string | null): Promise<void>;
  closeWorkbench?(): Promise<void>;
}

declare global {
  interface Window {
    dbxPlugin: DbxPluginApi;
  }
}

export {};
