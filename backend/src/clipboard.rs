use std::sync::{Arc, Mutex};

use dbx_plugin_sdk::PluginEmitter;
use ironrdp::client::rdp::RdpInputSender;
use ironrdp::cliprdr::backend::{ClipboardMessage, CliprdrBackend, CliprdrBackendFactory};
use ironrdp::cliprdr::pdu::{
    ClipboardFormat, ClipboardFormatId, ClipboardFormatName, ClipboardGeneralCapabilityFlags,
    FileContentsFlags, FileContentsRequest, FileContentsResponse, FileDescriptor,
    FormatDataRequest, FormatDataResponse, LockDataId,
};
use ironrdp::core::{IntoOwned as _, impl_as_any};
use ironrdp_cliprdr_format::bitmap::{dib_to_png, dibv5_to_png};
use serde_json::json;

use crate::files::ClipboardFiles;

pub(crate) const MAX_CLIPBOARD_CHARS: usize = 1024 * 1024;

pub struct RdpClipboardFactory {
    input: RdpInputSender,
    files: Arc<Mutex<ClipboardFiles>>,
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
        files: Arc<Mutex<ClipboardFiles>>,
        emitter: PluginEmitter,
        connection_id: String,
        session_id: u32,
        temp_dir: String,
    ) -> Self {
        Self {
            input,
            files,
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
            files: Arc::clone(&self.files),
            emitter: self.emitter.clone(),
            connection_id: self.connection_id.clone(),
            session_id: self.session_id,
            temp_dir: self.temp_dir.clone(),
        })
    }
}

struct RdpClipboardBackend {
    input: RdpInputSender,
    files: Arc<Mutex<ClipboardFiles>>,
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
    fn emit_files(&self) {
        let files = self
            .files
            .lock()
            .ok()
            .and_then(|files| files.remote_files.clone());
        let (list, clip_data_id) = match files {
            Some(list) => (list.files, list.clip_data_id),
            None => (Vec::new(), None),
        };
        let payload: Vec<_> = list
            .iter()
            .map(|file| {
                json!({
                    "name": file.name,
                    "relativePath": file.relative_path,
                    "size": file.file_size.unwrap_or(0),
                })
            })
            .collect();
        if let Err(error) = self.emitter.event(
            "rdp/files",
            json!({
                "connectionId": self.connection_id,
                "sessionId": self.session_id,
                "files": payload,
                "clipDataId": clip_data_id,
            }),
        ) {
            tracing::warn!(error = %error.message, "failed to forward remote file list");
        }
    }

    /// Converts a remote DIB/DIBV5 payload to PNG and publishes the save banner.
    fn forward_remote_image(&self, format: ClipboardFormatId, data: &[u8]) {
        let converted = if format == ClipboardFormatId::CF_DIBV5 {
            dibv5_to_png(data)
        } else {
            dib_to_png(data)
        };
        let png = match converted {
            Ok(png) => png,
            Err(error) => {
                tracing::warn!(%error, "cannot convert remote clipboard image");
                return;
            }
        };
        let image =
            self.files
                .lock()
                .ok()
                .and_then(|mut files| match files.store_remote_image(&png) {
                    Ok(image) => Some(image),
                    Err(error) => {
                        tracing::warn!(%error, "cannot stage remote clipboard image");
                        None
                    }
                });
        let Some(image) = image else {
            return;
        };
        if let Err(error) = self.emitter.event(
            "rdp/clipboard/image",
            json!({
                "connectionId": self.connection_id,
                "sessionId": self.session_id,
                "token": image.token,
                "name": "remote-clipboard.png",
                "size": image.size,
                "width": image.width,
                "height": image.height,
            }),
        ) {
            tracing::warn!(error = %error.message, "failed to forward remote clipboard image");
        }
    }
}

impl CliprdrBackend for RdpClipboardBackend {
    fn temporary_directory(&self) -> &str {
        &self.temp_dir
    }

    fn client_capabilities(&self) -> ClipboardGeneralCapabilityFlags {
        // Advertise streamed file copy so Windows Explorer can paste files we
        // staged locally, and so remote file copies arrive as a file list.
        ClipboardGeneralCapabilityFlags::STREAM_FILECLIP_ENABLED
            | ClipboardGeneralCapabilityFlags::CAN_LOCK_CLIPDATA
            | ClipboardGeneralCapabilityFlags::HUGE_FILE_SUPPORT_ENABLED
    }

    fn on_ready(&mut self) {
        let (pending, has_local_text) = self
            .files
            .lock()
            .map(|mut files| {
                files.ready = true;
                (files.pending_remote_format.take(), files.has_local_text())
            })
            .unwrap_or((None, false));
        if let Some(format) = pending {
            let _ = self
                .input
                .send_clipboard(ClipboardMessage::SendInitiatePaste(format));
        }
        // Advertise the last local text once the channel can accept requests;
        // a paste issued during initialization is replayed here.
        if has_local_text {
            let _ = self
                .input
                .send_clipboard(ClipboardMessage::SendInitiateCopy(vec![
                    ClipboardFormat::new(ClipboardFormatId::CF_UNICODETEXT),
                ]));
        }
    }

    fn on_format_list_response(&mut self, ok: bool) {
        // Only text advertisements that are waiting for an ACK wake a paste;
        // file advertisements also end up here.
        if let Ok(mut files) = self.files.lock() {
            files.acknowledge_format_list(ok);
        }
    }

    fn on_request_format_list(&mut self) {
        // The CLIPRDR channel only becomes ready after the remote answers one
        // of our FormatList PDUs. Advertise an empty text clipboard: local text
        // is injected with keyboard events, but an empty text entry keeps the
        // handshake compatible with servers that reject zero-format lists.
        if let Ok(mut files) = self.files.lock() {
            files.ready = false;
            files.advertised = false;
            files.pending_remote_format = None;
        }
        let formats = vec![ClipboardFormat::new(ClipboardFormatId::CF_UNICODETEXT)];
        let _ = self
            .input
            .send_clipboard(ClipboardMessage::SendInitiateCopy(formats));
    }

    fn on_process_negotiated_capabilities(
        &mut self,
        capabilities: ClipboardGeneralCapabilityFlags,
    ) {
        tracing::debug!(?capabilities, "clipboard capabilities negotiated");
        if let Ok(mut files) = self.files.lock() {
            files.file_clip =
                capabilities.contains(ClipboardGeneralCapabilityFlags::STREAM_FILECLIP_ENABLED);
        }
    }

    fn on_remote_copy(&mut self, available_formats: &[ClipboardFormat]) {
        let had_files = self
            .files
            .lock()
            .ok()
            .map(|mut files| {
                let had = files.remote_files.is_some();
                files.remote_files = None;
                had
            })
            .unwrap_or(false);
        if had_files {
            self.emit_files();
        }

        let file_list = available_formats.iter().find(|format| {
            format
                .name()
                .is_some_and(|name| name.value() == ClipboardFormatName::FILE_LIST.value())
        });
        // Office and drawing apps put a bitmap preview next to text; prefer the
        // text and only fetch the image when no text format is offered.
        let image_format = available_formats
            .iter()
            .find(|format| format.id() == ClipboardFormatId::CF_DIBV5)
            .or_else(|| {
                available_formats
                    .iter()
                    .find(|format| format.id() == ClipboardFormatId::CF_DIB)
            });
        let target = if let Some(format) = file_list {
            // Explorer also advertises CF_UNICODETEXT (the display name, often
            // without an extension) next to the file list. Requesting the file
            // list first is what stops file copies from being pasted as text.
            format.id()
        } else if available_formats
            .iter()
            .any(|format| format.id() == ClipboardFormatId::CF_UNICODETEXT)
        {
            ClipboardFormatId::CF_UNICODETEXT
        } else if let Some(format) = image_format {
            format.id()
        } else {
            return;
        };

        let ready = self
            .files
            .lock()
            .map(|mut files| {
                files.pending_image_request = is_image_format(target).then_some(target);
                if files.ready {
                    true
                } else {
                    // The channel handshake is still in flight; asking now would
                    // fail the whole session. Replay this request from on_ready.
                    files.pending_remote_format = Some(target);
                    false
                }
            })
            .unwrap_or(false);
        if ready {
            let _ = self
                .input
                .send_clipboard(ClipboardMessage::SendInitiatePaste(target));
        }
    }

    fn on_format_data_request(&mut self, request: FormatDataRequest) {
        // Replying before the channel is Ready would fail the whole session.
        let response = {
            let Ok(files) = self.files.lock() else {
                return;
            };
            if !files.ready {
                return;
            }
            if request.format == ClipboardFormatId::CF_UNICODETEXT {
                // An empty string keeps the pending request from hanging when
                // no text has been advertised yet.
                FormatDataResponse::new_unicode_string(files.local_text.as_deref().unwrap_or(""))
            } else if let Some(data) = files.local_image_data(request.format) {
                FormatDataResponse::new_data(data.to_vec())
            } else {
                FormatDataResponse::new_error()
            }
        };
        let _ = self
            .input
            .send_clipboard(ClipboardMessage::SendFormatData(response.into_owned()));
    }

    fn on_format_data_response(&mut self, response: FormatDataResponse<'_>) {
        if response.is_error() {
            return;
        }
        let image_format = self
            .files
            .lock()
            .ok()
            .and_then(|mut files| files.pending_image_request.take());
        if let Some(format) = image_format {
            self.forward_remote_image(format, response.data());
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
        if let Err(error) = self.emitter.event(
            "rdp/clipboard",
            json!({
                "connectionId": self.connection_id,
                "sessionId": self.session_id,
                "text": text,
            }),
        ) {
            tracing::warn!(error = %error.message, "failed to forward remote clipboard text");
        }
    }

    fn on_remote_file_list(&mut self, files: &[FileDescriptor], clip_data_id: Option<u32>) {
        tracing::debug!(
            count = files.len(),
            ?clip_data_id,
            "remote clipboard file list"
        );
        if let Ok(mut state) = self.files.lock() {
            state.remote_files = Some(crate::files::RemoteFileList {
                files: files.to_vec(),
                clip_data_id,
            });
            state.pending_image_request = None;
            // The remote now owns the clipboard; our previously advertised
            // text must be re-advertised before the next local paste.
            state.advertised = false;
        }
        self.emit_files();
    }

    fn on_file_contents_request(&mut self, request: FileContentsRequest) {
        let response = if request.flags.contains(FileContentsFlags::SIZE) {
            let size = self
                .files
                .lock()
                .ok()
                .and_then(|files| files.upload_file_size(usize::try_from(request.index).ok()?));
            match size {
                Some(size) => FileContentsResponse::new_size_response(request.stream_id, size),
                None => FileContentsResponse::new_error(request.stream_id),
            }
        } else if request.flags.contains(FileContentsFlags::RANGE) {
            let index = usize::try_from(request.index).unwrap_or(usize::MAX);
            let data = self.files.lock().ok().and_then(|files| {
                files
                    .read_upload_range(index, request.position, request.requested_size)
                    .ok()
            });
            match data {
                Some(data) => FileContentsResponse::new_data_response(request.stream_id, data),
                None => FileContentsResponse::new_error(request.stream_id),
            }
        } else {
            FileContentsResponse::new_error(request.stream_id)
        };
        let _ = self
            .input
            .send_clipboard(ClipboardMessage::SendFileContentsResponse(
                response.into_owned(),
            ));
    }

    fn on_file_contents_response(&mut self, response: FileContentsResponse<'_>) {
        let stream_id = response.stream_id();
        let result = if response.is_error() {
            Err("Remote side refused the file request".to_string())
        } else {
            Ok(response.data().to_vec())
        };
        let sender = self
            .files
            .lock()
            .ok()
            .and_then(|mut files| files.pending.remove(&stream_id));
        if let Some(sender) = sender {
            let _ = sender.send(result);
        }
    }

    fn on_lock(&mut self, data_id: LockDataId) {
        tracing::debug!(clip_data_id = data_id.0, "remote locked clipboard data");
    }

    fn on_unlock(&mut self, data_id: LockDataId) {
        tracing::debug!(clip_data_id = data_id.0, "remote unlocked clipboard data");
    }
}

pub fn truncate_chars(value: &str, max: usize) -> String {
    if value.chars().count() <= max {
        return value.to_string();
    }
    value.chars().take(max).collect()
}

fn is_image_format(format: ClipboardFormatId) -> bool {
    format == ClipboardFormatId::CF_DIB || format == ClipboardFormatId::CF_DIBV5
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
