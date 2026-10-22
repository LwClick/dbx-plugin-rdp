<script setup lang="ts">
import type { SecurityInfo } from "./bridge";
import type { MessageKey } from "./i18n";

const props = defineProps<{
  security: SecurityInfo | null;
  t: (key: MessageKey) => string;
}>();

const emit = defineEmits<{ (event: "close"): void }>();
</script>

<template>
  <aside class="security-panel" aria-label="RDP security details">
    <header class="security-panel__header">
      <span>{{ t("app.security") }}</span>
      <button type="button" class="icon-button" :title="t('app.closeSecurity')" @click="emit('close')">×</button>
    </header>
    <div v-if="props.security" class="security-panel__body">
      <div class="security-row">
        <span>{{ props.security.nla ? t("security.nla") : t("security.tls") }}</span>
        <span class="security-value">{{ props.security.host }}:{{ props.security.port }}</span>
      </div>
      <template v-if="props.security.certificate">
        <div class="security-row security-row--column">
          <span>{{ t("security.certificate") }}</span>
          <code class="fingerprint">{{ t("security.fingerprint") }}: {{ props.security.certificate.sha256 }}</code>
          <span v-if="props.security.certificate.subject" class="security-muted">
            {{ t("security.subject") }}: {{ props.security.certificate.subject }}
          </span>
          <span v-if="props.security.certificate.issuer" class="security-muted">
            {{ t("security.issuer") }}: {{ props.security.certificate.issuer }}
          </span>
          <span v-if="props.security.certificate.not_after" class="security-muted">
            {{ t("security.notAfter") }}: {{ props.security.certificate.not_after }}
          </span>
        </div>
      </template>
      <p v-else class="security-muted">{{ t("security.none") }}</p>
      <p class="security-hint">{{ t("security.resetHint") }}</p>
    </div>
    <div v-else class="security-panel__body">
      <p class="security-muted">{{ t("security.none") }}</p>
    </div>
  </aside>
</template>

<style scoped>
.security-panel {
  position: absolute;
  top: 0;
  right: 0;
  z-index: 5;
  display: flex;
  flex-direction: column;
  width: min(360px, 90%);
  height: 100%;
  border-left: 1px solid var(--color-border, #2c2c2c);
  background: color-mix(in srgb, var(--color-background, #111) 94%, transparent);
  backdrop-filter: blur(8px);
  box-shadow: -8px 0 24px rgba(0, 0, 0, 0.35);
}

.security-panel__header {
  display: flex;
  align-items: center;
  justify-content: space-between;
  padding: 12px 14px;
  border-bottom: 1px solid var(--color-border, #2c2c2c);
  font-weight: 600;
}

.security-panel__body {
  display: flex;
  flex-direction: column;
  gap: 14px;
  padding: 14px;
  overflow: auto;
  font-size: 12px;
}

.security-row {
  display: flex;
  align-items: center;
  justify-content: space-between;
  gap: 12px;
}

.security-row--column {
  flex-direction: column;
  align-items: flex-start;
  gap: 6px;
}

.security-value {
  color: var(--color-muted-foreground, #a3a3a3);
}

.fingerprint {
  display: block;
  max-width: 100%;
  overflow-wrap: anywhere;
  padding: 6px 8px;
  border-radius: 6px;
  background: var(--color-muted, #1f1f1f);
  color: var(--color-foreground, #ededed);
  font-family: ui-monospace, SFMono-Regular, Menlo, Consolas, monospace;
  font-size: 11px;
  line-height: 1.5;
}

.security-muted {
  margin: 0;
  color: var(--color-muted-foreground, #a3a3a3);
  overflow-wrap: anywhere;
}

.security-hint {
  margin: 0;
  color: var(--color-muted-foreground, #a3a3a3);
  line-height: 1.5;
}

.icon-button {
  display: inline-flex;
  align-items: center;
  justify-content: center;
  width: 26px;
  height: 26px;
  border: 1px solid transparent;
  border-radius: 6px;
  background: transparent;
  color: inherit;
  font-size: 18px;
  line-height: 1;
  cursor: pointer;
}

.icon-button:hover {
  background: var(--color-muted, #1f1f1f);
  border-color: var(--color-border, #2c2c2c);
}
</style>
