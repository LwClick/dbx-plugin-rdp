export type Locale = "en" | "zh-CN";

export function detectLocale(locale: string | undefined): Locale {
  return (locale ?? "").toLowerCase().startsWith("zh") ? "zh-CN" : "en";
}

const messages = {
  en: {
    "app.empty": "Open an RDP connection from the sidebar to start a remote desktop.",
    "app.connect": "Connect",
    "app.reconnect": "Reconnect",
    "app.disconnect": "Disconnect",
    "app.connecting": "Connecting…",
    "app.reconnecting": "Reconnecting…",
    "app.connected": "Connected",
    "app.disconnected": "Disconnected",
    "app.failed": "Connection failed",
    "app.retryHint": "Check the host, credentials and network, then reconnect.",
    "app.sendClipboard": "Send clipboard",
    "app.pasteClipboard": "Paste clipboard",
    "app.ctrlAltDel": "Ctrl+Alt+Del",
    "app.security": "Security",
    "app.closeSecurity": "Close security panel",
    "app.clipboardOn": "Clipboard sync on",
    "app.clipboardOff": "Clipboard sync off",
    "app.clipboardCopied": "Copied from remote desktop",
    "app.clipboardSent": "Clipboard sent to remote desktop",
    "app.clipboardEmpty": "Local clipboard is empty",
    "app.clipboardDenied": "Clipboard access was denied",
    "app.resizeFallback": "Reconnecting to apply the new desktop size…",
    "app.fullscreen": "Focus desktop",
    "app.details": "Details",
    "security.nla": "NLA / CredSSP",
    "security.tls": "TLS",
    "security.certificate": "Server certificate",
    "security.fingerprint": "SHA-256",
    "security.subject": "Subject",
    "security.issuer": "Issuer",
    "security.notAfter": "Valid until",
    "security.none": "No certificate details available",
    "security.resetHint": "To trust a different certificate, edit the connection and use “Reset certificate trust”.",
    "error.attach": "The RDP session is not connected.",
    "error.noConnectionId": "This workbench was opened without a connection.",
  },
  "zh-CN": {
    "app.empty": "请从侧栏连接列表打开一个 RDP 连接。",
    "app.connect": "连接",
    "app.reconnect": "重连",
    "app.disconnect": "断开",
    "app.connecting": "正在连接…",
    "app.reconnecting": "正在重连…",
    "app.connected": "已连接",
    "app.disconnected": "已断开",
    "app.failed": "连接失败",
    "app.retryHint": "请检查主机、账号密码和网络后重试。",
    "app.sendClipboard": "发送剪贴板",
    "app.pasteClipboard": "粘贴剪贴板",
    "app.ctrlAltDel": "Ctrl+Alt+Del",
    "app.security": "安全信息",
    "app.closeSecurity": "关闭安全面板",
    "app.clipboardOn": "剪贴板同步已开启",
    "app.clipboardOff": "剪贴板同步已关闭",
    "app.clipboardCopied": "已从远程桌面复制",
    "app.clipboardSent": "已发送到远程桌面",
    "app.clipboardEmpty": "本地剪贴板为空",
    "app.clipboardDenied": "剪贴板读取被拒绝",
    "app.resizeFallback": "正在通过重新连接应用新分辨率…",
    "app.fullscreen": "聚焦桌面",
    "app.details": "详情",
    "security.nla": "NLA / CredSSP",
    "security.tls": "TLS",
    "security.certificate": "服务器证书",
    "security.fingerprint": "SHA-256",
    "security.subject": "主题",
    "security.issuer": "颁发者",
    "security.notAfter": "有效期至",
    "security.none": "暂无证书信息",
    "security.resetHint": "如需信任新证书，请编辑连接并使用「重置证书信任」。",
    "error.attach": "RDP 会话未连接。",
    "error.noConnectionId": "该工作台没有关联连接。",
  },
} as const;

export type MessageKey = keyof (typeof messages)["en"];

export function createTranslator(locale: Locale) {
  const table = messages[locale];
  return (key: MessageKey): string => table[key] ?? messages.en[key] ?? key;
}
