use std::collections::HashMap;
use std::io;
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::{Arc, Mutex};

use base64::Engine as _;
use base64::engine::general_purpose::STANDARD as BASE64;
use dbx_plugin_sdk::{PluginEmitter, PluginError, PluginHandler, RequestContext};
use serde_json::{Value, json};
use tokio::runtime::{Builder, Runtime};

use crate::cert::PinStore;
use crate::credentials::RdpConnectOptions;
use crate::files::UPLOAD_CHUNK_BYTES;
use crate::protocol::{CLIPBOARD_CHANNEL, INPUT_CHANNEL, parse_clipboard_chunk, parse_input};
use crate::session::RdpSession;

pub struct RdpPlugin {
    runtime: Arc<Runtime>,
    sessions: Mutex<HashMap<String, Arc<RdpSession>>>,
    sessions_by_id: Mutex<HashMap<u32, Arc<RdpSession>>>,
    emitter: Mutex<Option<PluginEmitter>>,
    next_session_id: AtomicU32,
}

impl RdpPlugin {
    pub fn new() -> io::Result<Self> {
        let runtime = Builder::new_multi_thread()
            .worker_threads(4)
            .enable_all()
            .thread_name("rdp-runtime")
            .build()?;
        Ok(Self {
            runtime: Arc::new(runtime),
            sessions: Mutex::new(HashMap::new()),
            sessions_by_id: Mutex::new(HashMap::new()),
            emitter: Mutex::new(None),
            next_session_id: AtomicU32::new(1),
        })
    }

    fn remember_emitter(&self, emitter: &PluginEmitter) {
        if let Ok(mut slot) = self.emitter.lock()
            && slot.is_none()
        {
            *slot = Some(emitter.clone());
        }
    }

    fn session(&self, connection_id: &str) -> Result<Arc<RdpSession>, PluginError> {
        self.sessions
            .lock()
            .ok()
            .and_then(|sessions| sessions.get(connection_id).cloned())
            .ok_or_else(|| PluginError::new(-32010, "RDP session is not connected"))
    }

    fn remove_session(&self, connection_id: &str) -> Option<Arc<RdpSession>> {
        let removed = self
            .sessions
            .lock()
            .ok()
            .and_then(|mut sessions| sessions.remove(connection_id));
        if let Some(session) = &removed
            && let Ok(mut by_id) = self.sessions_by_id.lock()
        {
            by_id.remove(&session.session_id);
        }
        removed
    }

    fn insert_session(&self, session: Arc<RdpSession>) {
        if let Ok(mut sessions) = self.sessions.lock() {
            sessions.insert(session.connection_id.clone(), Arc::clone(&session));
        }
        if let Ok(mut by_id) = self.sessions_by_id.lock() {
            by_id.insert(session.session_id, session);
        }
    }

    fn connection_id(params: &Value) -> Result<&str, PluginError> {
        params
            .get("connectionId")
            .and_then(Value::as_str)
            .filter(|id| !id.trim().is_empty())
            .ok_or_else(|| PluginError::new(-32602, "Missing connectionId"))
    }

    fn handle_connect(
        &self,
        params: Value,
        emitter: &PluginEmitter,
        test_only: bool,
    ) -> Result<Value, PluginError> {
        let options = RdpConnectOptions::from_lifecycle(&params)?;
        let connection_id = options.connection_id.clone();
        let session_id = self.next_session_id.fetch_add(1, Ordering::Relaxed);
        let session = RdpSession::connect(
            self.runtime.handle().clone(),
            options,
            session_id,
            emitter.clone(),
            !test_only,
        )?;
        if test_only {
            session.disconnect();
            return Ok(json!({ "success": true, "message": "RDP authentication succeeded." }));
        }
        let previous = self.remove_session(&connection_id);
        if let Some(previous) = previous {
            previous.disconnect();
        }
        self.insert_session(Arc::clone(&session));
        Ok(json!({ "success": true }))
    }

    fn handle_action(&self, params: &Value) -> Result<Value, PluginError> {
        let action = params
            .get("action")
            .and_then(|action| action.get("id"))
            .and_then(Value::as_str)
            .ok_or_else(|| PluginError::new(-32602, "Missing connection action id"))?;
        match action {
            "reset-certificate-pin" => {
                let options = RdpConnectOptions::from_lifecycle(params)?;
                PinStore::from_env()
                    .forget(&options.pin_key())
                    .map_err(|error| {
                        PluginError::new(-32000, format!("Cannot reset certificate pin: {error}"))
                    })?;
                Ok(json!({ "success": true, "message": "Certificate trust has been reset." }))
            }
            other => Err(PluginError::new(
                -32602,
                format!("Unknown connection action: {other}"),
            )),
        }
    }

    fn handle_rdp(&self, method: &str, params: &Value) -> Result<Value, PluginError> {
        let connection_id = Self::connection_id(params)?;
        let session = self.session(connection_id)?;
        match method {
            "rdp/attach" => {
                let viewport = params.get("viewport").and_then(|viewport| {
                    let width = viewport
                        .get("width")
                        .and_then(Value::as_u64)
                        .and_then(|value| u16::try_from(value).ok())?;
                    let height = viewport
                        .get("height")
                        .and_then(Value::as_u64)
                        .and_then(|value| u16::try_from(value).ok())?;
                    Some((width, height))
                });
                Ok(session.attach(viewport))
            }
            "rdp/detach" => {
                let session_id = params
                    .get("sessionId")
                    .and_then(Value::as_u64)
                    .unwrap_or_default();
                if session_id != u64::from(session.session_id) {
                    return Err(PluginError::new(
                        -32011,
                        "Viewer belongs to a different RDP session",
                    ));
                }
                session.detach();
                Ok(json!({ "ok": true }))
            }
            "rdp/resize" => {
                let width = params
                    .get("width")
                    .and_then(Value::as_u64)
                    .and_then(|value| u16::try_from(value).ok())
                    .ok_or_else(|| PluginError::new(-32602, "Invalid resize width"))?;
                let height = params
                    .get("height")
                    .and_then(Value::as_u64)
                    .and_then(|value| u16::try_from(value).ok())
                    .ok_or_else(|| PluginError::new(-32602, "Invalid resize height"))?;
                session.resize(width, height)
            }
            "rdp/requestFullFrame" => {
                session.request_full_frame();
                Ok(json!({ "ok": true }))
            }
            "rdp/paste" => {
                let text = params
                    .get("text")
                    .and_then(Value::as_str)
                    .unwrap_or_default()
                    .to_string();
                Ok(session.paste(text))
            }
            "rdp/clipboard/local" => {
                let text = params
                    .get("text")
                    .and_then(Value::as_str)
                    .unwrap_or_default()
                    .to_string();
                session.advertise_local_text(text)
            }
            "rdp/clipboard/content/paste" => {
                let token = params
                    .get("token")
                    .and_then(Value::as_u64)
                    .and_then(|value| u32::try_from(value).ok())
                    .ok_or_else(|| PluginError::new(-32602, "Invalid clipboard image token"))?;
                let text = params
                    .get("text")
                    .and_then(Value::as_str)
                    .map(str::to_string);
                session.paste_image(token, text)
            }
            "rdp/clipboard/image/read" => {
                let token = required_str(params, "token")?;
                let offset = params
                    .get("offset")
                    .and_then(Value::as_u64)
                    .unwrap_or_default();
                let length = params
                    .get("length")
                    .and_then(Value::as_u64)
                    .unwrap_or(u64::from(crate::files::DOWNLOAD_CHUNK_BYTES));
                session.read_clipboard_image(token, offset, length)
            }
            "rdp/clipboard/image/release" => {
                let token = required_str(params, "token")?;
                session.release_clipboard_image(token);
                Ok(json!({ "ok": true }))
            }
            "rdp/files/upload/begin" => {
                let token = required_str(params, "token")?;
                let name = required_str(params, "name")?;
                let relative_path = params.get("relativePath").and_then(Value::as_str);
                let size = params
                    .get("size")
                    .and_then(Value::as_u64)
                    .unwrap_or_default();
                let index = session.begin_upload(token, name, relative_path, size)?;
                Ok(json!({ "index": index }))
            }
            "rdp/files/upload/chunk" => {
                let token = required_str(params, "token")?;
                let index = params
                    .get("index")
                    .and_then(Value::as_u64)
                    .and_then(|value| usize::try_from(value).ok())
                    .ok_or_else(|| PluginError::new(-32602, "Invalid file index"))?;
                let offset = params
                    .get("offset")
                    .and_then(Value::as_u64)
                    .ok_or_else(|| PluginError::new(-32602, "Missing chunk offset"))?;
                let encoded = required_str(params, "dataBase64")?;
                if encoded.len() > UPLOAD_CHUNK_BYTES * 4 / 3 + 8 {
                    return Err(PluginError::new(-32602, "Chunk is too large"));
                }
                let data = BASE64
                    .decode(encoded)
                    .map_err(|_| PluginError::new(-32602, "Invalid base64 chunk"))?;
                if data.len() > UPLOAD_CHUNK_BYTES {
                    return Err(PluginError::new(-32602, "Chunk is too large"));
                }
                let written = session.write_upload_chunk(token, index, offset, &data)?;
                Ok(json!({ "written": written }))
            }
            "rdp/files/upload/finish" => {
                let token = required_str(params, "token")?;
                let index = params
                    .get("index")
                    .and_then(Value::as_u64)
                    .and_then(|value| usize::try_from(value).ok())
                    .ok_or_else(|| PluginError::new(-32602, "Invalid file index"))?;
                session.finish_upload(token, index)?;
                Ok(json!({ "ok": true }))
            }
            "rdp/files/upload/cancel" => {
                let token = required_str(params, "token")?;
                session.cancel_upload(token);
                Ok(json!({ "ok": true }))
            }
            "rdp/files/advertise" => {
                let token = required_str(params, "token")?;
                session.advertise_files(token)
            }
            "rdp/files/download" => session.start_download(),
            "rdp/files/read" => {
                let token = required_str(params, "token")?;
                let offset = params
                    .get("offset")
                    .and_then(Value::as_u64)
                    .unwrap_or_default();
                let length = params
                    .get("length")
                    .and_then(Value::as_u64)
                    .unwrap_or(u64::from(crate::files::DOWNLOAD_CHUNK_BYTES));
                session.read_download(token, offset, length)
            }
            "rdp/files/release" => {
                let token = required_str(params, "token")?;
                session.release_download(token);
                Ok(json!({ "ok": true }))
            }
            "rdp/security" => Ok(session.security()),
            "rdp/disconnect" => {
                // Cancels the sidecar-side reconnect loop; the host can create
                // a fresh session through connection/connect on the next try.
                if let Some(session) = self.remove_session(connection_id) {
                    session.disconnect();
                }
                Ok(json!({ "ok": true }))
            }
            other => Err(PluginError::method_not_found(other)),
        }
    }
}

impl PluginHandler for RdpPlugin {
    fn handle(
        &self,
        _context: RequestContext,
        method: &str,
        params: Value,
        emitter: &PluginEmitter,
    ) -> Result<Value, PluginError> {
        self.remember_emitter(emitter);
        match method {
            "connection/test" => self.handle_connect(params, emitter, true),
            "connection/connect" => self.handle_connect(params, emitter, false),
            "connection/disconnect" => {
                let connection_id = params
                    .get("connection")
                    .and_then(|connection| connection.get("id"))
                    .and_then(Value::as_str)
                    .ok_or_else(|| PluginError::new(-32602, "Missing connection id"))?;
                if let Some(session) = self.remove_session(connection_id) {
                    session.disconnect();
                }
                Ok(json!({ "success": true }))
            }
            "connection/action" => self.handle_action(&params),
            method if method.starts_with("rdp/") => self.handle_rdp(method, &params),
            other => Err(PluginError::method_not_found(other)),
        }
    }

    fn handle_binary(
        &self,
        channel: &str,
        data: Vec<u8>,
        _emitter: &PluginEmitter,
    ) -> Result<(), PluginError> {
        if channel == CLIPBOARD_CHANNEL {
            let chunk = parse_clipboard_chunk(&data)
                .map_err(|error| PluginError::new(-32602, error.to_string()))?;
            let session = self
                .sessions_by_id
                .lock()
                .ok()
                .and_then(|sessions| sessions.get(&chunk.session_id).cloned());
            return match session {
                Some(session) => session.handle_clipboard_chunk(chunk),
                None => Ok(()),
            };
        }
        if channel != INPUT_CHANNEL {
            return Err(PluginError::new(
                -32601,
                format!("Unknown binary channel: {channel}"),
            ));
        }
        let packet =
            parse_input(&data).map_err(|error| PluginError::new(-32602, error.to_string()))?;
        let session = self
            .sessions_by_id
            .lock()
            .ok()
            .and_then(|sessions| sessions.get(&packet.session_id).cloned());
        match session {
            Some(session) => session.handle_input(packet),
            None => {
                tracing::debug!(
                    session_id = packet.session_id,
                    "dropping input for unknown RDP session"
                );
                Ok(())
            }
        }
    }
}

impl Drop for RdpPlugin {
    fn drop(&mut self) {
        if let Ok(mut sessions) = self.sessions.lock() {
            for session in sessions.values() {
                session.disconnect();
            }
            sessions.clear();
        }
        if let Ok(mut by_id) = self.sessions_by_id.lock() {
            by_id.clear();
        }
    }
}

fn required_str<'a>(params: &'a Value, key: &str) -> Result<&'a str, PluginError> {
    params
        .get(key)
        .and_then(Value::as_str)
        .filter(|value| !value.is_empty())
        .ok_or_else(|| PluginError::new(-32602, format!("Missing {key}")))
}
