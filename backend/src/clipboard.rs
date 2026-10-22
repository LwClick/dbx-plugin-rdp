use std::sync::{Arc, Mutex};

use dbx_plugin_sdk::PluginEmitter;
use ironrdp::client::rdp::RdpInputSender;
use ironrdp::cliprdr::backend::{ClipboardMessage, CliprdrBackend, CliprdrBackendFactory};
use ironrdp::cliprdr::pdu::{
    ClipboardFormat, ClipboardFormatId, ClipboardGeneralCapabilityFlags, FileContentsRequest,
    FileContentsResponse, FormatDataRequest, FormatDataResponse, LockDataId,
};
use ironrdp::core::{IntoOwned as _, impl_as_any};
use serde_json::json;

const MAX_CLIPBOARD_CHARS: usize = 1024 * 1024;

pub struct RdpClipboardFactory {
    input: RdpInputSender,
    local_text: Arc<Mutex<Option<String>>>,
    emitter: PluginEmitter,
    connection_id: String,
    session_id: u32,
    temp_dir: String,
}

impl core::fmt::Debug for RdpClipboardFactory {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.debug_struct("RdpClipboardFactory")
            .field("connection_id", &self.connection_id)
            .finish_non_exhaustive()
    }
}

impl RdpClipboardFactory {
    pub fn new(
        input: RdpInputSender,
        local_text: Arc<Mutex<Option<String>>>,
        emitter: PluginEmitter,
        connection_id: String,
        session_id: u32,
        temp_dir: String,
    ) -> Self {
        Self {
            input,
            local_text,
            emitter,
            connection_id,
            session_id,
            temp_dir,
        }
    }
}

impl CliprdrBackendFactory for RdpClipboardFactory {
    fn build_cliprdr_backend(&self) -> Box<dyn CliprdrBackend> {
        Box::new(RdpClipboardBackend {
            input: self.input.clone(),
            local_text: Arc::clone(&self.local_text),
            emitter: self.emitter.clone(),
            connection_id: self.connection_id.clone(),
            session_id: self.session_id,
            temp_dir: self.temp_dir.clone(),
        })
    }
}

struct RdpClipboardBackend {
    input: RdpInputSender,
    local_text: Arc<Mutex<Option<String>>>,
    emitter: PluginEmitter,
    connection_id: String,
    session_id: u32,
    temp_dir: String,
}

impl core::fmt::Debug for RdpClipboardBackend {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.debug_struct("RdpClipboardBackend")
            .field("connection_id", &self.connection_id)
            .finish_non_exhaustive()
    }
}

impl_as_any!(RdpClipboardBackend);

impl RdpClipboardBackend {
    fn advertise_local_text(&self) {
        let has_text = self
            .local_text
            .lock()
            .ok()
            .and_then(|text| text.as_ref().map(|text| !text.is_empty()))
            .unwrap_or(false);
        if has_text {
            let formats = vec![ClipboardFormat::new(ClipboardFormatId::CF_UNICODETEXT)];
            let _ = self
                .input
                .send_clipboard(ClipboardMessage::SendInitiateCopy(formats));
        }
    }
}

impl CliprdrBackend for RdpClipboardBackend {
    fn temporary_directory(&self) -> &str {
        &self.temp_dir
    }

    fn client_capabilities(&self) -> ClipboardGeneralCapabilityFlags {
        ClipboardGeneralCapabilityFlags::empty()
    }

    fn on_ready(&mut self) {
        self.advertise_local_text();
    }

    fn on_request_format_list(&mut self) {
        self.advertise_local_text();
    }

    fn on_process_negotiated_capabilities(
        &mut self,
        capabilities: ClipboardGeneralCapabilityFlags,
    ) {
        tracing::debug!(?capabilities, "clipboard capabilities negotiated");
    }

    fn on_remote_copy(&mut self, available_formats: &[ClipboardFormat]) {
        if available_formats
            .iter()
            .any(|format| format.id() == ClipboardFormatId::CF_UNICODETEXT)
        {
            let _ = self
                .input
                .send_clipboard(ClipboardMessage::SendInitiatePaste(
                    ClipboardFormatId::CF_UNICODETEXT,
                ));
        }
    }

    fn on_format_data_request(&mut self, request: FormatDataRequest) {
        if request.format != ClipboardFormatId::CF_UNICODETEXT {
            return;
        }
        let text = self
            .local_text
            .lock()
            .ok()
            .and_then(|text| text.clone())
            .unwrap_or_default();
        let response = FormatDataResponse::new_unicode_string(&text).into_owned();
        let _ = self
            .input
            .send_clipboard(ClipboardMessage::SendFormatData(response));
    }

    fn on_format_data_response(&mut self, response: FormatDataResponse<'_>) {
        if response.is_error() {
            return;
        }
        let Ok(text) = response.to_unicode_string() else {
            tracing::warn!("ignoring non-Unicode clipboard payload");
            return;
        };
        let text = truncate_chars(&text, MAX_CLIPBOARD_CHARS);
        if text.is_empty() {
            return;
        }
        if let Err(error) = self
            .emitter
            .event("rdp/clipboard", json!({ "connectionId": self.connection_id, "sessionId": self.session_id, "text": text }))
        {
            tracing::warn!(error = %error.message, "failed to forward remote clipboard text");
        }
    }

    fn on_file_contents_request(&mut self, request: FileContentsRequest) {
        tracing::debug!(?request, "file clipboard is not supported");
    }

    fn on_file_contents_response(&mut self, response: FileContentsResponse<'_>) {
        tracing::debug!(?response, "file clipboard is not supported");
    }

    fn on_lock(&mut self, _data_id: LockDataId) {}

    fn on_unlock(&mut self, _data_id: LockDataId) {}
}

pub fn truncate_chars(value: &str, max: usize) -> String {
    if value.chars().count() <= max {
        return value.to_string();
    }
    value.chars().take(max).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn truncates_long_text() {
        assert_eq!(truncate_chars("abcdef", 3), "abc");
        assert_eq!(truncate_chars("abc", 3), "abc");
    }
}
