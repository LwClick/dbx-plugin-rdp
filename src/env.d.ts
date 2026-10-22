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
}

interface DbxPluginInit {
  capabilities?: Record<string, boolean | undefined>;
  locale?: string;
  theme?: { appearance?: string; tokens?: Record<string, string> };
  context?: Record<string, unknown>;
  contributionId?: string;
}

interface DbxPluginApi {
  ready: Promise<DbxPluginInit | undefined>;
  contributionId?: string;
  context: Record<string, unknown>;
  locale: string;
  theme: { appearance?: string; tokens?: Record<string, string> };
  capabilities: Record<string, boolean | undefined>;
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
  copy?(text: string): Promise<void>;
  reopenConnection?(connectionId: string): Promise<{ ok?: boolean }>;
  setTitle?(title: string | null): Promise<void>;
}

declare global {
  interface Window {
    dbxPlugin: DbxPluginApi;
  }
}

export {};
