use std::collections::HashMap;
use std::fs;
use std::io::Write as _;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, Weak};
use std::time::{Duration, Instant};

use base64::Engine as _;
use base64::engine::general_purpose::STANDARD as BASE64;
use dbx_plugin_sdk::{PluginEmitter, PluginError};
use ironrdp::client::config::{ClipboardType, ConfigBuilder, Destination};
use ironrdp::client::output_channel::output_channel;
use ironrdp::client::rdp::{
    AutoReconnectDecision, DesktopUpdate, RdpClient, RdpInputEvent, RdpInputSender, RdpOutputEvent,
};
use ironrdp::cliprdr::backend::ClipboardMessage;
use ironrdp::cliprdr::pdu::{
    ClipboardFormat, ClipboardFormatId, FileContentsFlags, FileContentsRequest,
};
use ironrdp::connector::{ConnectorError, ConnectorErrorKind};
use ironrdp::input::{
    Database, MouseButton, MousePosition, Operation, Scancode, WheelRotations, synchronize_event,
};
use ironrdp::pdu::rdp::capability_sets::MajorPlatformType;
use ironrdp::pdu::rdp::client_info::PerformanceFlags;
use ironrdp_cliprdr_format::bitmap::{png_to_cf_dib, png_to_cf_dibv5};
use ironrdp_tls::CertificateValidation;
use serde_json::json;
use smallvec::SmallVec;
use tokio::runtime::Handle;

use crate::cert::{CertPrompt, CertificateInfo, PinStore};
use crate::clipboard::{MAX_CLIPBOARD_CHARS, RdpClipboardFactory};
use crate::credentials::{PerformanceProfile, RdpConnectOptions, SecurityMode};
use crate::files::{
    ClipboardFiles, DOWNLOAD_CHUNK_BYTES, DownloadState, FileChunkResult, MAX_FILE_BYTES,
    create_download_file, prune_stale_directories,
};
use crate::protocol::{
    CURSOR_CHANNEL, CURSOR_KIND_BITMAP, CURSOR_KIND_DEFAULT, CURSOR_KIND_HIDDEN, ClipboardChunk,
    CursorHeader, FRAME_CHANNEL, FRAME_TYPE_BITMAP, FRAME_TYPE_RESIZE, FrameHeader, InputMessage,
    InputPacket, MAX_DESKTOP_DIM, MIN_DESKTOP_DIM, Rect, chunk_rect,
};

const FRAME_INTERVAL: Duration = Duration::from_millis(33);
/// Full-desktop updates are expensive over the host binary bridge. Cap them at
/// ~10 fps while keeping dirty-rect updates at the normal frame interval.
const FULL_FRAME_MIN_INTERVAL: Duration = Duration::from_millis(100);
/// Upper bound on independent dirty rectangles coalesced per frame.
const MAX_DIRTY_RECTS: usize = 16;
/// Promote to a full frame once the merged dirty area reaches this percentage.
const FULL_FRAME_DIRTY_PERCENT: u64 = 85;
/// Maximum bitmap bytes emitted in one 33 ms tick. Remaining rectangles stay
/// dirty and are sent on the next tick.
const FRAME_TICK_BUDGET_BYTES: usize = 8 * 1024 * 1024;
/// Merge rectangles separated by at most this many pixels.
const DIRTY_MERGE_GAP: u16 = 2;
const MAX_TYPE_CHARS: usize = 32_768;
const TYPE_CHUNK_CHARS: usize = 128;
/// How long a paste waits for the remote to acknowledge our FormatList
/// before falling back to synthetic keystrokes.
const PASTE_ACK_TIMEOUT: Duration = Duration::from_millis(1200);
/// Abandoned image uploads and unconsumed completed images expire here.
const IMAGE_UPLOAD_TTL: Duration = Duration::from_secs(120);
/// Keep at most this many image transfers buffered in memory (24 MiB each).
const MAX_PENDING_IMAGES: usize = 4;

#[derive(Debug, Clone)]
pub enum SessionState {
    Connecting,
    Connected,
    Reconnecting,
    Disconnected,
    Failed { code: i32, message: String },
}

impl SessionState {
    fn label(&self) -> &'static str {
        match self {
            Self::Connecting => "connecting",
            Self::Connected => "connected",
            Self::Reconnecting => "reconnecting",
            Self::Disconnected => "disconnected",
            Self::Failed { .. } => "failed",
        }
    }
}

pub struct RdpSession {
    pub connection_id: String,
    pub session_id: u32,
    pub name: String,
    pub host: String,
    pub port: u16,
    pub security_mode: SecurityMode,
    desktop_width: u16,
    desktop_height: u16,
    auto_resize: bool,
    clipboard_sync: bool,
    /// Effective RDPDR shared directory, reported to the workbench.
    shared_folder: Mutex<Option<String>>,
    /// Audio redirection was requested for this session.
    audio_playback: bool,
    /// macOS Command shortcuts are mapped to Windows Ctrl combinations.
    remap_shortcuts: bool,
    emit_events: bool,
    state: Mutex<SessionState>,
    disconnect_info: Mutex<Option<DisconnectInfo>>,
    cert_info: Arc<Mutex<Option<CertificateInfo>>>,
    input: Mutex<Option<RdpInputSender>>,
    input_db: Mutex<Database>,
    /// Serializes pasted-text injection so two concurrent pastes cannot interleave.
    typing: tokio::sync::Mutex<()>,
    /// Serializes the synchronous paste handshake (advertise + ACK + Ctrl+V).
    paste_lock: Mutex<()>,
    frame: Mutex<FrameState>,
    /// Last cursor message, replayed when a workbench attaches. Servers usually
    /// send the shape once, long before the plugin tab is opened.
    cursor: Mutex<Option<Vec<u8>>>,
    clipboard_files: Arc<Mutex<ClipboardFiles>>,
    /// PNG chunks uploaded from the workbench, keyed by the UI token.
    image_uploads: Mutex<HashMap<u32, ImageUpload>>,
    /// Fully assembled PNGs waiting for the paste RPC.
    pending_images: Mutex<HashMap<u32, PendingImage>>,
    viewers: AtomicUsize,
    emitter: PluginEmitter,
    runtime: Handle,
    log: Arc<Mutex<Option<fs::File>>>,
    last_frame_log: Mutex<Instant>,
    closed: AtomicBool,
}

#[derive(Debug, Clone)]
struct DisconnectInfo {
    reason: String,
    detail: String,
}

struct ImageUpload {
    total: usize,
    chunks: Vec<Option<Vec<u8>>>,
    received: usize,
    updated: Instant,
}

struct PendingImage {
    png: Vec<u8>,
    updated: Instant,
}

struct FrameState {
    width: u16,
    height: u16,
    rgba: Vec<u8>,
    /// Last full frame that contained visible pixels. Servers blank the
    /// session before applying a display resize; this keeps the previous
    /// desktop available for the transitional frame.
    last_good: Vec<u8>,
    /// Servers often send several full black frames while re-activating the
    /// surface after a resize. Skip a few so the scaled picture stays visible
    /// until real content arrives.
    skip_black_full: u8,
    dirty: Vec<Rect>,
    dirty_area: u64,
    full: bool,
    seq: u32,
    resize_pending: Option<(u16, u16)>,
    last_full_emit: Option<Instant>,
    frames_sent: u64,
    full_frames_sent: u64,
    rects_sent: u64,
    bytes_sent: u64,
}

impl FrameState {
    fn new(width: u16, height: u16) -> Self {
        Self {
            width,
            height,
            rgba: vec![0; usize::from(width) * usize::from(height) * 4],
            last_good: Vec::new(),
            skip_black_full: 0,
            dirty: Vec::new(),
            dirty_area: 0,
            full: true,
            seq: 0,
            resize_pending: Some((width, height)),
            last_full_emit: None,
            frames_sent: 0,
            full_frames_sent: 0,
            rects_sent: 0,
            bytes_sent: 0,
        }
    }

    fn resize(&mut self, width: u16, height: u16) {
        if self.width == width && self.height == height {
            return;
        }
        let previous_width = self.width;
        let previous_height = self.height;
        let expected = usize::from(previous_width) * usize::from(previous_height) * 4;
        let previous = if self.last_good.len() == expected && expected > 0 {
            std::mem::take(&mut self.last_good)
        } else {
            std::mem::take(&mut self.rgba)
        };
        self.width = width;
        self.height = height;
        self.rgba = if previous_width > 0 && previous_height > 0 && !previous.is_empty() {
            // Keep the old desktop visible (scaled) until the server repaints.
            // Zeroing here made a dropped resize/full frame look like a black
            // screen on servers that do not send a full redraw immediately.
            scale_rgba_nearest(&previous, previous_width, previous_height, width, height)
        } else {
            vec![0; usize::from(width) * usize::from(height) * 4]
        };
        self.last_good = self.rgba.clone();
        self.skip_black_full = 3;
        self.dirty.clear();
        self.dirty_area = 0;
        self.full = true;
        self.resize_pending = Some((width, height));
    }

    fn apply_pixels(&mut self, pixels: &[u32], rect: Rect) {
        if rect.is_empty()
            || rect.right() > u32::from(self.width)
            || rect.bottom() > u32::from(self.height)
        {
            return;
        }
        let full_rect =
            rect.x == 0 && rect.y == 0 && rect.width == self.width && rect.height == self.height;
        let has_visible = pixels
            .iter()
            .take(usize::from(rect.width) * usize::from(rect.height))
            .any(|pixel| pixel & 0x00FF_FFFF != 0);
        if self.skip_black_full > 0 {
            if full_rect && !has_visible {
                self.skip_black_full -= 1;
                return;
            }
            self.skip_black_full = 0;
        }
        let stride = usize::from(self.width);
        let row_pixels = usize::from(rect.width);
        // `DesktopUpdate` buffers are packed to the dirty region, while `Image`
        // buffers cover the whole framebuffer. Only the destination uses the
        // full-desktop stride; the source is always rect-local.
        if pixels.len() < row_pixels * usize::from(rect.height) {
            return;
        }
        for row in 0..usize::from(rect.height) {
            let src_start = row * row_pixels;
            let src_end = src_start + row_pixels;
            let dst_start = ((usize::from(rect.y) + row) * stride + usize::from(rect.x)) * 4;
            let dst_end = dst_start + row_pixels * 4;
            for (pixel, dst) in pixels[src_start..src_end]
                .iter()
                .zip(self.rgba[dst_start..dst_end].chunks_exact_mut(4))
            {
                dst[0] = ((pixel >> 16) & 0xFF) as u8;
                dst[1] = ((pixel >> 8) & 0xFF) as u8;
                dst[2] = (pixel & 0xFF) as u8;
                dst[3] = 0xFF;
            }
        }
        self.add_dirty(rect);
        let desktop_area = u64::from(self.width) * u64::from(self.height);
        if desktop_area > 0
            && self.dirty_area.saturating_mul(100)
                >= desktop_area.saturating_mul(FULL_FRAME_DIRTY_PERCENT)
        {
            self.full = true;
            self.dirty.clear();
            self.dirty_area = 0;
        }
        // The server may paint the desktop through many partial updates, so a
        // single full-rect update cannot be relied on for the snapshot. Keep
        // the latest composited picture whenever it is about to be sent as a
        // full frame.
        if self.full && has_visible {
            self.last_good.clear();
            self.last_good.extend_from_slice(&self.rgba);
        }
    }

    /// Records a repainted rectangle, merging overlaps and keeping the list
    /// bounded so two far-apart updates do not inflate into a full frame.
    fn add_dirty(&mut self, mut rect: Rect) {
        if rect.is_empty() {
            return;
        }
        let mut index = 0;
        while index < self.dirty.len() {
            if rects_touch(self.dirty[index], rect, DIRTY_MERGE_GAP) {
                rect = rect.union(self.dirty.remove(index));
                // The merged rectangle may now touch earlier entries.
                index = 0;
            } else {
                index += 1;
            }
        }
        self.dirty.push(rect);
        while self.dirty.len() > MAX_DIRTY_RECTS {
            self.merge_closest_pair();
        }
        self.dirty_area = self.dirty.iter().map(|rect| u64::from(rect.area())).sum();
    }

    fn merge_closest_pair(&mut self) {
        if self.dirty.len() < 2 {
            return;
        }
        let mut best = (0, 1);
        let mut best_cost = u64::MAX;
        for first in 0..self.dirty.len() {
            for second in (first + 1)..self.dirty.len() {
                let a = self.dirty[first];
                let b = self.dirty[second];
                let cost = u64::from(a.union(b).area())
                    .saturating_sub(u64::from(a.area()) + u64::from(b.area()));
                if cost < best_cost {
                    best_cost = cost;
                    best = (first, second);
                }
            }
        }
        let second = self.dirty.remove(best.1);
        let first = self.dirty.remove(best.0);
        self.dirty.push(first.union(second));
    }
}

/// Checks whether two rectangles overlap or are within `gap` pixels of each
/// other on both axes.
fn rects_touch(a: Rect, b: Rect, gap: u16) -> bool {
    let gap = u32::from(gap);
    u32::from(a.x) <= b.right().saturating_add(gap)
        && u32::from(b.x) <= a.right().saturating_add(gap)
        && u32::from(a.y) <= b.bottom().saturating_add(gap)
        && u32::from(b.y) <= a.bottom().saturating_add(gap)
}

impl RdpSession {
    pub fn connect(
        runtime: Handle,
        mut options: RdpConnectOptions,
        session_id: u32,
        emitter: PluginEmitter,
        emit_events: bool,
    ) -> Result<Arc<Self>, PluginError> {
        options.ensure_password()?;
        let cert_info: Arc<Mutex<Option<CertificateInfo>>> = Arc::new(Mutex::new(None));
        let pin_store = PinStore::from_env();
        let cert_prompt = CertPrompt::new(
            options.connection_host.clone(),
            options.connection_port,
            options.certificate_policy,
            pin_store,
            Arc::clone(&cert_info),
        );

        let desktop_width = options.desktop_width;
        let desktop_height = options.desktop_height;
        let auto_resize = options.auto_resize;

        let mut builder = ConfigBuilder::new()
            .with_destination(Destination::from_parts(options.host.clone(), options.port))
            .with_username(options.username.clone())
            .with_password(options.password.to_string())
            .with_client_name("DBX-RDP")
            .with_client_build(19_041)
            .with_client_dir("C:\\Windows\\System32")
            .with_platform(MajorPlatformType::WINDOWS)
            .with_desktop_width(desktop_width)
            .with_desktop_height(desktop_height)
            .with_desktop_scale_factor(100)
            .with_color_depth(32)
            .with_lossy_compression(!options.lossless)
            .with_credssp(options.security_mode == SecurityMode::Nla)
            .with_tls(options.security_mode == SecurityMode::Tls)
            .with_autologon(true)
            .with_certificate_validation(CertificateValidation::Strict)
            .with_certificate_validation_callback(cert_prompt.callback())
            .with_clipboard(if options.clipboard_sync {
                ClipboardType::Enable
            } else {
                ClipboardType::Disable
            })
            .with_rdpdr(options.share_folder)
            .with_performance_flags(performance_flags(options.performance_profile));
        // cpal-backed playback only exists on macOS/Windows; the Linux build
        // omits the feature to avoid an ALSA build dependency.
        #[cfg(not(target_os = "linux"))]
        {
            builder = builder.with_audio_mode(if options.audio_playback {
                ironrdp_cfg::AudioMode::RedirectToClient
            } else {
                ironrdp_cfg::AudioMode::Disabled
            });
        }
        #[cfg(target_os = "linux")]
        if options.audio_playback {
            tracing::info!("audio redirection is unavailable on this build");
        }
        if let Some(domain) = options
            .domain
            .as_deref()
            .filter(|domain| !domain.is_empty())
        {
            builder = builder.with_domain(domain.to_string());
        }
        if let Some(layout) = options.keyboard_layout {
            builder = builder.with_keyboard_layout(layout);
        }
        let config = builder.build().map_err(|error| {
            PluginError::new(-32011, format!("Invalid RDP configuration: {error}"))
        })?;

        let clipboard_files = Arc::new(Mutex::new(ClipboardFiles::default()));
        prune_stale_directories();
        let log = Arc::new(Mutex::new(open_diagnostics_log()));
        let session = Arc::new(Self {
            connection_id: options.connection_id.clone(),
            session_id,
            name: options.name.clone(),
            host: options.connection_host.clone(),
            port: options.connection_port,
            security_mode: options.security_mode,
            desktop_width,
            desktop_height,
            auto_resize,
            clipboard_sync: options.clipboard_sync,
            shared_folder: Mutex::new(None),
            audio_playback: options.audio_playback,
            remap_shortcuts: options.remap_shortcuts,
            emit_events,
            state: Mutex::new(SessionState::Connecting),
            disconnect_info: Mutex::new(None),
            cert_info,
            input: Mutex::new(None),
            input_db: Mutex::new(Database::new()),
            typing: tokio::sync::Mutex::new(()),
            paste_lock: Mutex::new(()),
            frame: Mutex::new(FrameState::new(desktop_width, desktop_height)),
            cursor: Mutex::new(None),
            clipboard_files: Arc::clone(&clipboard_files),
            image_uploads: Mutex::new(HashMap::new()),
            pending_images: Mutex::new(HashMap::new()),
            viewers: AtomicUsize::new(0),
            emitter: emitter.clone(),
            runtime: runtime.clone(),
            log: Arc::clone(&log),
            last_frame_log: Mutex::new(Instant::now() - Duration::from_secs(2)),
            closed: AtomicBool::new(false),
        });
        session.log_session(format!(
            "connect session={} security={:?} timeout={}s",
            session.session_id,
            session.security_mode,
            options.connect_timeout.as_secs()
        ));

        let (output_tx, output_rx) = output_channel(32);
        let mut client = RdpClient::new(config, output_tx);
        let input_sender = client.input_sender();
        if let Ok(mut slot) = session.input.lock() {
            *slot = Some(input_sender.clone());
        }
        if options.clipboard_sync {
            let factory = RdpClipboardFactory::new(
                input_sender,
                Arc::clone(&clipboard_files),
                emitter.clone(),
                session.connection_id.clone(),
                session.session_id,
                clipboard_temp_dir(),
            );
            client = client.with_cliprdr_backend_factory(Box::new(factory));
        }
        if options.share_folder {
            match crate::drives::shared_folder_root(options.shared_folder.as_deref())
                .and_then(|root| crate::drives::build_factory(&root).map(|factory| (root, factory)))
            {
                Ok((root, factory)) => {
                    session.log_session(format!(
                        "rdpdr session={} root={}",
                        session.session_id,
                        root.display()
                    ));
                    if let Ok(mut slot) = session.shared_folder.lock() {
                        *slot = Some(root.to_string_lossy().into_owned());
                    }
                    client = client.with_rdpdr_backend_factory(factory);
                }
                Err(error) => {
                    // A missing or unreadable folder must not break the
                    // desktop connection; RDPDR is simply skipped.
                    session.log_session(format!(
                        "rdpdr disabled session={} error={error}",
                        session.session_id
                    ));
                    tracing::warn!(%error, "RDPDR shared folder is unavailable");
                }
            }
        }
        client = client.with_desktop_updates().with_auto_reconnect(3);

        let _client_thread = std::thread::Builder::new()
            .name(format!("rdp-client-{}", session.session_id))
            .spawn(move || {
                let Ok(runtime) = tokio::runtime::Builder::new_current_thread()
                    .enable_all()
                    .build()
                else {
                    tracing::error!("cannot create RDP client runtime");
                    return;
                };
                runtime.block_on(client.run());
            })
            .map_err(|error| {
                PluginError::new(-32000, format!("Cannot start RDP client: {error}"))
            })?;

        let (connect_tx, connect_rx) = std::sync::mpsc::channel::<Result<(), PluginError>>();
        let weak = Arc::downgrade(&session);
        runtime.spawn(async move {
            consume_output(output_rx, weak, connect_tx).await;
        });

        let weak = Arc::downgrade(&session);
        std::thread::Builder::new()
            .name(format!("rdp-frame-{}", session.session_id))
            .spawn(move || frame_pump(weak))
            .map_err(|error| {
                PluginError::new(-32000, format!("Cannot start frame pump: {error}"))
            })?;

        match connect_rx.recv_timeout(options.connect_timeout + Duration::from_secs(2)) {
            Ok(Ok(())) => Ok(session),
            Ok(Err(error)) => {
                session.disconnect();
                Err(error)
            }
            Err(_) => {
                session.disconnect();
                Err(PluginError::new(-32014, "RDP connection timed out"))
            }
        }
    }

    pub fn state(&self) -> SessionState {
        self.state
            .lock()
            .map(|state| state.clone())
            .unwrap_or(SessionState::Disconnected)
    }

    fn set_state(&self, state: SessionState) {
        if let Ok(mut current) = self.state.lock() {
            *current = state;
        }
    }

    fn emit_state(&self) {
        let state = self.state();
        self.log_session(format!(
            "state session={} {}",
            self.session_id,
            state.label()
        ));
        if !self.emit_events {
            return;
        }
        let mut payload = json!({
            "connectionId": self.connection_id,
            "sessionId": self.session_id,
            "state": state.label(),
        });
        if let SessionState::Failed { code, message } = &state {
            payload["errorCode"] = json!(code);
            payload["error"] = json!(message);
        }
        if matches!(state, SessionState::Disconnected)
            && let Some(info) = self
                .disconnect_info
                .lock()
                .ok()
                .and_then(|info| info.clone())
        {
            payload["reason"] = json!(info.reason);
            payload["detail"] = json!(info.detail);
        }
        if let Err(error) = self.emitter.event("rdp/state", payload) {
            tracing::warn!(error = %error.message, "failed to emit RDP state");
        }
    }

    pub fn attach(&self, viewport: Option<(u16, u16)>) -> serde_json::Value {
        self.viewers.fetch_add(1, Ordering::SeqCst);
        self.log_session(format!(
            "attach session={} viewport={}",
            self.session_id,
            viewport
                .map(|(width, height)| format!("{width}x{height}"))
                .unwrap_or_else(|| "none".to_string())
        ));
        if let Ok(mut frame) = self.frame.lock() {
            frame.full = true;
        }
        if let Some(message) = self.cursor.lock().ok().and_then(|cursor| cursor.clone())
            && self.emit_events
            && let Err(error) = self.emitter.binary(CURSOR_CHANNEL, &message)
        {
            tracing::warn!(error = %error.message, "failed to replay RDP cursor");
        }
        let (width, height) = self
            .frame
            .lock()
            .map(|frame| (frame.width, frame.height))
            .unwrap_or((self.desktop_width, self.desktop_height));
        let certificate = self.cert_info.lock().ok().and_then(|info| info.clone());
        let shared_folder = self.shared_folder.lock().ok().and_then(|slot| slot.clone());
        json!({
            "sessionId": self.session_id,
            "width": width,
            "height": height,
            "autoResize": self.auto_resize,
            "name": self.name,
            "host": self.host,
            "port": self.port,
            "certificate": certificate,
            "sharedFolder": shared_folder,
            "audioSupported": cfg!(not(target_os = "linux")),
            "audioEnabled": self.audio_playback && cfg!(not(target_os = "linux")),
            "remapShortcuts": self.remap_shortcuts,
        })
    }

    pub fn detach(&self) {
        let _ = self
            .viewers
            .fetch_update(Ordering::SeqCst, Ordering::SeqCst, |viewers| {
                viewers.checked_sub(1)
            });
    }

    pub fn resize(&self, width: u16, height: u16) -> Result<serde_json::Value, PluginError> {
        if !self.auto_resize {
            let (width, height) = self
                .frame
                .lock()
                .map(|frame| (frame.width, frame.height))
                .unwrap_or((self.desktop_width, self.desktop_height));
            return Ok(json!({
                "accepted": false,
                "width": width,
                "height": height,
                "reason": "fixed resolution",
            }));
        }
        let width = width.clamp(MIN_DESKTOP_DIM, MAX_DESKTOP_DIM);
        let width = width & !1;
        let height = height.clamp(MIN_DESKTOP_DIM, MAX_DESKTOP_DIM);
        let Some(input) = self.input.lock().ok().and_then(|input| input.clone()) else {
            return Err(PluginError::new(-32010, "RDP session is not connected"));
        };
        match input.try_send(RdpInputEvent::Resize {
            width,
            height,
            scale_factor: 100,
            physical_size: None,
        }) {
            Ok(()) => {
                self.log_session(format!(
                    "resize request session={} {}x{}",
                    self.session_id, width, height
                ));
                if let Ok(mut frame) = self.frame.lock() {
                    frame.full = true;
                }
                Ok(json!({ "accepted": true, "width": width, "height": height }))
            }
            Err(error) => Err(PluginError::new(
                -32000,
                format!("Cannot resize RDP desktop: {error}"),
            )),
        }
    }

    pub fn request_full_frame(&self) {
        if let Ok(mut frame) = self.frame.lock() {
            frame.full = true;
        }
    }

    pub fn paste(self: &Arc<Self>, text: String) -> serde_json::Value {
        let _guard = self.paste_lock.lock().ok();
        let total = text.chars().count();
        if self.clipboard_sync && self.clipboard_ready_for_paste() {
            let clipped: String = text.chars().take(MAX_CLIPBOARD_CHARS).collect();
            if !clipped.is_empty() && self.paste_via_clipboard(clipped) {
                return json!({
                    "typed": 0,
                    "truncated": total > MAX_CLIPBOARD_CHARS,
                    "mode": "clipboard",
                });
            }
        }
        self.type_text(text)
    }

    fn clipboard_ready_for_paste(&self) -> bool {
        self.clipboard_files
            .lock()
            .map(|files| files.ready)
            .unwrap_or(false)
    }

    /// Advertises `text` on CLIPRDR, waits for the remote to accept the format
    /// list, then sends Ctrl+V. Returns `false` when the caller should fall
    /// back to synthetic typing.
    fn paste_via_clipboard(&self, text: String) -> bool {
        let rx = {
            let Ok(mut files) = self.clipboard_files.lock() else {
                return false;
            };
            let changed = files.set_local_text(text);
            if !changed && files.advertised {
                None
            } else {
                let (tx, rx) = std::sync::mpsc::channel();
                files.pending_paste = true;
                files.paste_waiters.push(tx);
                Some(rx)
            }
        };
        let Some(rx) = rx else {
            // The remote already accepted this exact text; paste immediately.
            return self.send_ctrl_v();
        };
        let advertise = self.send_clipboard_message(ClipboardMessage::SendInitiateCopy(vec![
            ClipboardFormat::new(ClipboardFormatId::CF_UNICODETEXT),
        ]));
        if advertise.is_err() {
            self.cancel_paste_waiter();
            return false;
        }
        let acknowledged = rx.recv_timeout(PASTE_ACK_TIMEOUT).unwrap_or(false);
        if !acknowledged {
            self.cancel_paste_waiter();
            return false;
        }
        self.send_ctrl_v()
    }

    fn cancel_paste_waiter(&self) {
        // `paste_lock` guarantees at most one waiter, so clearing is safe.
        if let Ok(mut files) = self.clipboard_files.lock() {
            files.paste_waiters.clear();
            files.pending_paste = false;
        }
    }

    /// Advertises local text without waiting for an acknowledgement. Remote
    /// apps querying the clipboard through their own menu (Edit → Paste) then
    /// see the latest local text even when the user never pressed Ctrl+V.
    pub fn advertise_local_text(&self, text: String) -> Result<serde_json::Value, PluginError> {
        if !self.clipboard_sync || !self.clipboard_ready() {
            return Ok(json!({ "ok": true, "advertised": false }));
        }
        let text: String = text.chars().take(MAX_CLIPBOARD_CHARS).collect();
        if text.is_empty() {
            return Ok(json!({ "ok": true, "advertised": false }));
        }
        let changed = self
            .clipboard_files
            .lock()
            .map(|mut files| {
                if !files.set_local_text(text) {
                    return false;
                }
                // Force a new FormatList even when the previous advertisement
                // was acknowledged before the clipboard changed.
                files.advertised = false;
                true
            })
            .unwrap_or(false);
        if !changed {
            return Ok(json!({ "ok": true, "advertised": false }));
        }
        self.send_clipboard_message(ClipboardMessage::SendInitiateCopy(vec![
            ClipboardFormat::new(ClipboardFormatId::CF_UNICODETEXT),
        ]))
        .map_err(|error| PluginError::new(-32012, error))?;
        Ok(json!({ "ok": true, "advertised": true }))
    }

    /// Collects one PNG chunk from the workbench. Runs on the binary reader
    /// thread, so it only buffers data and never waits for the remote.
    pub fn handle_clipboard_chunk(&self, chunk: ClipboardChunk<'_>) -> Result<(), PluginError> {
        if !self.clipboard_sync {
            return Ok(());
        }
        let total = usize::try_from(chunk.total_len)
            .map_err(|_| PluginError::new(-32602, "Invalid image size"))?;
        let index = usize::from(chunk.chunk_index);
        let count = usize::from(chunk.chunk_count);
        let token = chunk.token;
        let now = Instant::now();
        let complete = {
            let Ok(mut uploads) = self.image_uploads.lock() else {
                return Ok(());
            };
            uploads.retain(|_, upload| now.duration_since(upload.updated) < IMAGE_UPLOAD_TTL);
            if !uploads.contains_key(&token)
                && uploads.len() >= MAX_PENDING_IMAGES
                && let Some(oldest) = uploads
                    .iter()
                    .min_by_key(|(_, upload)| upload.updated)
                    .map(|(token, _)| *token)
            {
                uploads.remove(&oldest);
            }
            let upload = uploads.entry(token).or_insert_with(|| ImageUpload {
                total,
                chunks: vec![None; count],
                received: 0,
                updated: now,
            });
            if upload.total != total || upload.chunks.len() != count {
                // The UI reused a token for a different image; start over.
                *upload = ImageUpload {
                    total,
                    chunks: vec![None; count],
                    received: 0,
                    updated: now,
                };
            }
            upload.updated = now;
            if upload.chunks[index].is_none() {
                upload.chunks[index] = Some(chunk.payload.to_vec());
                upload.received += 1;
            }
            upload.received == upload.chunks.len()
        };
        if !complete {
            return Ok(());
        }
        let upload = self
            .image_uploads
            .lock()
            .ok()
            .and_then(|mut uploads| uploads.remove(&token));
        let Some(upload) = upload else {
            return Ok(());
        };
        let mut png = Vec::with_capacity(upload.total);
        for part in upload.chunks {
            let Some(part) = part else {
                return Ok(());
            };
            png.extend_from_slice(&part);
        }
        if png.len() != upload.total {
            return Ok(());
        }
        if let Ok(mut images) = self.pending_images.lock() {
            images.retain(|_, image| now.duration_since(image.updated) < IMAGE_UPLOAD_TTL);
            if images.len() >= MAX_PENDING_IMAGES
                && let Some(oldest) = images
                    .iter()
                    .min_by_key(|(_, image)| image.updated)
                    .map(|(token, _)| *token)
            {
                images.remove(&oldest);
            }
            images.insert(token, PendingImage { png, updated: now });
        }
        Ok(())
    }

    /// Converts the uploaded PNG and pastes it together with optional text.
    pub fn paste_image(
        self: &Arc<Self>,
        token: u32,
        text: Option<String>,
    ) -> Result<serde_json::Value, PluginError> {
        if !self.clipboard_sync {
            return Err(PluginError::new(-32012, "Clipboard sync is disabled"));
        }
        if !self.clipboard_ready_for_paste() {
            return Err(PluginError::new(
                -32012,
                "Clipboard is still initializing; try again in a moment",
            ));
        }
        let png = self
            .pending_images
            .lock()
            .ok()
            .and_then(|mut images| images.remove(&token).map(|image| image.png))
            .ok_or_else(|| PluginError::new(-32602, "Unknown clipboard image"))?;
        let dibv5 = png_to_cf_dibv5(&png).map_err(|error| {
            PluginError::new(-32602, format!("Invalid clipboard image: {error}"))
        })?;
        let dib = png_to_cf_dib(&png).map_err(|error| {
            PluginError::new(-32602, format!("Invalid clipboard image: {error}"))
        })?;

        let _guard = self.paste_lock.lock().ok();
        let (rx, formats) = {
            let Ok(mut files) = self.clipboard_files.lock() else {
                return Err(PluginError::new(-32012, "Clipboard is unavailable"));
            };
            files.set_local_image(dib, dibv5);
            let mut formats = vec![
                ClipboardFormat::new(ClipboardFormatId::CF_DIBV5),
                ClipboardFormat::new(ClipboardFormatId::CF_DIB),
            ];
            match text.filter(|value| !value.is_empty()) {
                Some(text) => {
                    files.set_local_text(text.chars().take(MAX_CLIPBOARD_CHARS).collect());
                    formats.insert(0, ClipboardFormat::new(ClipboardFormatId::CF_UNICODETEXT));
                }
                None => files.clear_local_text(),
            }
            files.advertised = false;
            let (tx, rx) = std::sync::mpsc::channel();
            files.pending_paste = true;
            files.paste_waiters.push(tx);
            (rx, formats)
        };
        if let Err(error) = self.send_clipboard_message(ClipboardMessage::SendInitiateCopy(formats))
        {
            self.cancel_paste_waiter();
            return Err(PluginError::new(-32012, error));
        }
        let acknowledged = rx.recv_timeout(PASTE_ACK_TIMEOUT).unwrap_or(false);
        if !acknowledged {
            self.cancel_paste_waiter();
            return Err(PluginError::new(
                -32013,
                "The remote clipboard did not accept the image",
            ));
        }
        if !self.send_ctrl_v() {
            return Err(PluginError::new(-32013, "Cannot send the paste shortcut"));
        }
        Ok(json!({ "ok": true, "mode": "clipboard" }))
    }

    pub fn read_clipboard_image(
        &self,
        token: &str,
        offset: u64,
        length: u64,
    ) -> Result<serde_json::Value, PluginError> {
        let (data, eof) = self
            .clipboard_files
            .lock()
            .map_err(|_| PluginError::new(-32012, "Clipboard is unavailable"))?
            .read_remote_image(token, offset, length)
            .map_err(|error| PluginError::new(-32020, error))?;
        Ok(json!({
            "dataBase64": BASE64.encode(&data),
            "length": data.len(),
            "eof": eof,
        }))
    }

    pub fn release_clipboard_image(&self, token: &str) {
        if let Ok(mut files) = self.clipboard_files.lock() {
            files.clear_remote_image(token);
        }
    }

    fn send_ctrl_v(&self) -> bool {
        let Some(input) = self.input.lock().ok().and_then(|input| input.clone()) else {
            return false;
        };
        let Ok(mut database) = self.input_db.lock() else {
            return false;
        };
        let mut events = database.release_all();
        events.extend(database.apply([
            Operation::KeyPressed(Scancode::from_u8(false, 0x1D)),
            Operation::KeyPressed(Scancode::from_u8(false, 0x2F)),
            Operation::KeyReleased(Scancode::from_u8(false, 0x2F)),
            Operation::KeyReleased(Scancode::from_u8(false, 0x1D)),
        ]));
        if events.is_empty() {
            return false;
        }
        input.try_send(RdpInputEvent::FastPath(events)).is_ok()
    }

    /// Injects text as Unicode input events instead of relying on clipboard
    /// redirection. Typed chunks keep the fast-path input queue responsive.
    pub fn type_text(self: &Arc<Self>, text: String) -> serde_json::Value {
        let total = text.chars().count();
        let truncated = total > MAX_TYPE_CHARS;
        let chars: Vec<char> = text.chars().take(MAX_TYPE_CHARS).collect();
        let typed = chars.len();
        if typed == 0 {
            return json!({ "typed": 0, "truncated": false, "mode": "typed" });
        }
        let session = Arc::clone(self);
        self.runtime.spawn(async move {
            session.inject_text(chars).await;
        });
        json!({ "typed": typed, "truncated": truncated, "mode": "typed" })
    }

    async fn inject_text(self: Arc<Self>, chars: Vec<char>) {
        let _guard = self.typing.lock().await;
        let Some(input) = self.input.lock().ok().and_then(|input| input.clone()) else {
            return;
        };
        let mut first = true;
        for chunk in chars.chunks(TYPE_CHUNK_CHARS) {
            let events = {
                let Ok(mut database) = self.input_db.lock() else {
                    return;
                };
                let mut events = if first {
                    database.release_all()
                } else {
                    SmallVec::new()
                };
                first = false;
                for character in chunk {
                    events.extend(database.apply(text_operations(*character)));
                }
                events
            };
            if events.is_empty() {
                continue;
            }
            let mut pending = RdpInputEvent::FastPath(events);
            for _ in 0..100 {
                match input.try_send(pending) {
                    Ok(()) => break,
                    Err(tokio::sync::mpsc::error::TrySendError::Full(event)) => {
                        pending = event;
                        tokio::time::sleep(Duration::from_millis(10)).await;
                    }
                    Err(_) => return,
                }
            }
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
        self.log_session(format!(
            "type session={} chars={}",
            self.session_id,
            chars.len()
        ));
    }

    pub fn security(&self) -> serde_json::Value {
        let certificate = self.cert_info.lock().ok().and_then(|info| info.clone());
        json!({
            "nla": self.security_mode == SecurityMode::Nla,
            "name": self.name,
            "host": self.host,
            "port": self.port,
            "certificate": certificate,
        })
    }

    pub fn begin_upload(
        &self,
        token: &str,
        name: &str,
        relative_path: Option<&str>,
        size: u64,
    ) -> Result<usize, PluginError> {
        let mut files = self
            .clipboard_files
            .lock()
            .map_err(|_| PluginError::new(-32000, "Clipboard state is unavailable"))?;
        files
            .begin_upload(token, name, relative_path, size)
            .map_err(|error| PluginError::new(-32020, error))
    }

    pub fn write_upload_chunk(
        &self,
        token: &str,
        index: usize,
        offset: u64,
        data: &[u8],
    ) -> Result<u64, PluginError> {
        let mut files = self
            .clipboard_files
            .lock()
            .map_err(|_| PluginError::new(-32000, "Clipboard state is unavailable"))?;
        files
            .write_upload_chunk(token, index, offset, data)
            .map_err(|error| PluginError::new(-32020, error))
    }

    pub fn finish_upload(&self, token: &str, index: usize) -> Result<(), PluginError> {
        let mut files = self
            .clipboard_files
            .lock()
            .map_err(|_| PluginError::new(-32000, "Clipboard state is unavailable"))?;
        files
            .finish_upload(token, index)
            .map_err(|error| PluginError::new(-32020, error))
    }

    pub fn cancel_upload(&self, token: &str) {
        if let Ok(mut files) = self.clipboard_files.lock() {
            files.cancel_upload(token);
        }
    }

    pub fn advertise_files(&self, token: &str) -> Result<serde_json::Value, PluginError> {
        if !self.clipboard_sync {
            return Err(PluginError::new(
                -32012,
                "Clipboard sync is disabled for this connection",
            ));
        }
        self.require_file_clipboard()?;
        let descriptors = self
            .clipboard_files
            .lock()
            .map_err(|_| PluginError::new(-32000, "Clipboard state is unavailable"))?
            .advertised_files(token)
            .map_err(|error| PluginError::new(-32020, error))?;
        let count = descriptors.len();
        self.send_clipboard_message(ClipboardMessage::SendInitiateFileCopy(descriptors))
            .map_err(|error| PluginError::new(-32012, error))?;
        self.log_session(format!(
            "advertise files session={} count={}",
            self.session_id, count
        ));
        Ok(json!({ "count": count }))
    }

    pub fn start_download(self: &Arc<Self>) -> Result<serde_json::Value, PluginError> {
        if !self.clipboard_sync {
            return Err(PluginError::new(
                -32012,
                "Clipboard sync is disabled for this connection",
            ));
        }
        self.require_file_clipboard()?;
        let remote = self
            .clipboard_files
            .lock()
            .map_err(|_| PluginError::new(-32000, "Clipboard state is unavailable"))?
            .remote_files
            .clone()
            .ok_or_else(|| PluginError::new(-32020, "No remote files on the clipboard"))?;
        let state = self
            .clipboard_files
            .lock()
            .map_err(|_| PluginError::new(-32000, "Clipboard state is unavailable"))?
            .begin_download(remote.files, remote.clip_data_id)
            .map_err(|error| PluginError::new(-32020, error))?;
        let token = state.token.clone();
        let session = Arc::clone(self);
        self.runtime
            .spawn(async move { session.run_download(state).await });
        Ok(json!({ "token": token }))
    }

    pub fn read_download(
        &self,
        token: &str,
        offset: u64,
        length: u64,
    ) -> Result<serde_json::Value, PluginError> {
        let (path, size) = {
            let files = self
                .clipboard_files
                .lock()
                .map_err(|_| PluginError::new(-32000, "Clipboard state is unavailable"))?;
            let state = files
                .download
                .as_ref()
                .filter(|state| state.token == token)
                .ok_or_else(|| PluginError::new(-32020, "Unknown download"))?;
            (state.archive.clone(), state.size)
        };
        if path.as_os_str().is_empty() {
            return Err(PluginError::new(-32020, "Download is not ready"));
        }
        if offset >= size {
            return Ok(json!({ "dataBase64": "", "length": 0, "eof": true }));
        }
        let length = length.min(u64::from(DOWNLOAD_CHUNK_BYTES));
        let length = length.min(size - offset) as usize;
        let mut file = std::fs::File::open(&path).map_err(|error| {
            PluginError::new(-32020, format!("Cannot open downloaded file: {error}"))
        })?;
        std::io::Seek::seek(&mut file, std::io::SeekFrom::Start(offset)).map_err(|error| {
            PluginError::new(-32020, format!("Cannot seek downloaded file: {error}"))
        })?;
        let mut data = vec![0u8; length];
        std::io::Read::read_exact(&mut file, &mut data).map_err(|error| {
            PluginError::new(-32020, format!("Cannot read downloaded file: {error}"))
        })?;
        let eof = offset + data.len() as u64 >= size;
        Ok(json!({
            "dataBase64": BASE64.encode(&data),
            "length": data.len(),
            "eof": eof,
        }))
    }

    pub fn release_download(&self, token: &str) {
        if let Ok(mut files) = self.clipboard_files.lock() {
            files.clear_download(token);
        }
    }

    fn send_clipboard_message(&self, message: ClipboardMessage) -> Result<(), String> {
        let input = self
            .input
            .lock()
            .ok()
            .and_then(|input| input.clone())
            .ok_or_else(|| "RDP session is not connected".to_string())?;
        input
            .send_clipboard(message)
            .map_err(|error| format!("Cannot send clipboard message: {error}"))
    }

    fn clipboard_ready(&self) -> bool {
        self.clipboard_files
            .lock()
            .map(|files| files.ready)
            .unwrap_or(false)
    }

    fn clipboard_supports_files(&self) -> bool {
        self.clipboard_files
            .lock()
            .map(|files| files.file_clip)
            .unwrap_or(false)
    }

    fn require_file_clipboard(&self) -> Result<(), PluginError> {
        if !self.clipboard_ready() {
            return Err(PluginError::new(
                -32012,
                "Clipboard is still initializing; try again in a moment",
            ));
        }
        if !self.clipboard_supports_files() {
            return Err(PluginError::new(
                -32012,
                "The remote server does not support clipboard file transfer",
            ));
        }
        Ok(())
    }

    async fn run_download(self: Arc<Self>, state: DownloadState) {
        let total: u64 = state.files.iter().filter_map(|file| file.file_size).sum();
        let mut received = 0u64;
        let mut last_emit = Instant::now();
        for (index, descriptor) in state.files.iter().enumerate() {
            let size = match descriptor.file_size {
                Some(size) => size,
                None => match self.request_file_size(index, state.clip_data_id).await {
                    Ok(size) => size,
                    Err(error) => {
                        self.fail_download(&state.token, error);
                        return;
                    }
                },
            };
            if size > MAX_FILE_BYTES {
                self.fail_download(&state.token, "Remote file is too large".to_string());
                return;
            }
            let path = state.dir.join(format!("{index}.bin"));
            let mut file = match create_download_file(&path) {
                Ok(file) => file,
                Err(error) => {
                    self.fail_download(
                        &state.token,
                        format!("Cannot create downloaded file: {error}"),
                    );
                    return;
                }
            };
            let mut position = 0u64;
            while position < size {
                let want = (size - position).min(u64::from(DOWNLOAD_CHUNK_BYTES)) as u32;
                match self
                    .request_file_chunk(index, position, want, state.clip_data_id)
                    .await
                {
                    Ok(data) => {
                        if data.is_empty() {
                            self.fail_download(
                                &state.token,
                                "Remote returned an empty file chunk".to_string(),
                            );
                            return;
                        }
                        if let Err(error) = std::io::Write::write_all(&mut file, &data) {
                            self.fail_download(
                                &state.token,
                                format!("Cannot write downloaded file: {error}"),
                            );
                            return;
                        }
                        position += data.len() as u64;
                        received += data.len() as u64;
                    }
                    Err(error) => {
                        self.fail_download(&state.token, error);
                        return;
                    }
                }
                if last_emit.elapsed() >= Duration::from_millis(400) {
                    last_emit = Instant::now();
                    self.emit_file_event(
                        "rdp/files/progress",
                        &state.token,
                        json!({ "received": received, "total": total.max(received) }),
                    );
                }
            }
            let _ = std::io::Write::flush(&mut file);
        }
        let result = self
            .clipboard_files
            .lock()
            .ok()
            .and_then(|mut files| files.finish_download(&state.token).ok());
        match result {
            Some((name, _path, size)) => {
                self.log_session(format!(
                    "download finished session={} files={} bytes={}",
                    self.session_id,
                    state.files.len(),
                    size
                ));
                self.emit_file_event(
                    "rdp/files/ready",
                    &state.token,
                    json!({ "name": name, "size": size }),
                );
            }
            None => {
                self.fail_download(&state.token, "Cannot finalize downloaded files".to_string())
            }
        }
    }

    async fn request_file_size(&self, index: usize, data_id: Option<u32>) -> Result<u64, String> {
        let data = self
            .request_file_contents(index, FileContentsFlags::SIZE, 0, 8, data_id)
            .await?;
        let bytes: [u8; 8] = data
            .as_slice()
            .try_into()
            .map_err(|_| "Invalid size response from the remote".to_string())?;
        Ok(u64::from_le_bytes(bytes))
    }

    async fn request_file_chunk(
        &self,
        index: usize,
        position: u64,
        length: u32,
        data_id: Option<u32>,
    ) -> Result<Vec<u8>, String> {
        self.request_file_contents(index, FileContentsFlags::RANGE, position, length, data_id)
            .await
    }

    async fn request_file_contents(
        &self,
        index: usize,
        flags: FileContentsFlags,
        position: u64,
        length: u32,
        data_id: Option<u32>,
    ) -> Result<Vec<u8>, String> {
        let (stream_id, receiver) = {
            let mut files = self
                .clipboard_files
                .lock()
                .map_err(|_| "Clipboard state is unavailable".to_string())?;
            let stream_id = files.next_stream_id();
            let (sender, receiver) = tokio::sync::oneshot::channel::<FileChunkResult>();
            files.pending.insert(stream_id, sender);
            (stream_id, receiver)
        };
        let request = FileContentsRequest {
            stream_id,
            index: i32::try_from(index).map_err(|_| "File index is out of range".to_string())?,
            flags,
            position,
            requested_size: length,
            data_id,
        };
        if let Err(error) =
            self.send_clipboard_message(ClipboardMessage::SendFileContentsRequest(request))
        {
            if let Ok(mut files) = self.clipboard_files.lock() {
                files.pending.remove(&stream_id);
            }
            return Err(error);
        }
        match tokio::time::timeout(Duration::from_secs(30), receiver).await {
            Ok(Ok(result)) => result,
            Ok(Err(_)) => Err("Clipboard channel closed".to_string()),
            Err(_) => {
                if let Ok(mut files) = self.clipboard_files.lock() {
                    files.pending.remove(&stream_id);
                }
                Err("Timed out waiting for the remote file".to_string())
            }
        }
    }

    fn emit_file_event(&self, method: &str, token: &str, extra: serde_json::Value) {
        if !self.emit_events {
            return;
        }
        let mut payload = json!({
            "connectionId": self.connection_id,
            "sessionId": self.session_id,
            "token": token,
        });
        if let (Some(target), Some(extra)) = (payload.as_object_mut(), extra.as_object()) {
            for (key, value) in extra {
                target.insert(key.clone(), value.clone());
            }
        }
        let _ = self.emitter.event(method, payload);
    }

    fn fail_download(&self, token: &str, message: String) {
        self.log_session(format!(
            "download failed session={} error={}",
            self.session_id, message
        ));
        self.emit_file_event("rdp/files/error", token, json!({ "message": message }));
    }

    pub fn handle_input(&self, packet: InputPacket) -> Result<(), PluginError> {
        if packet.session_id != self.session_id {
            return Err(PluginError::new(
                -32011,
                "Input targets a different RDP session",
            ));
        }
        let Some(input) = self.input.lock().ok().and_then(|input| input.clone()) else {
            return Err(PluginError::new(-32010, "RDP session is not connected"));
        };
        let mut database = self
            .input_db
            .lock()
            .map_err(|_| PluginError::new(-32000, "RDP input state is poisoned"))?;
        let droppable = matches!(packet.message, InputMessage::MouseMove { .. });
        let events: SmallVec<[ironrdp::pdu::input::fast_path::FastPathInputEvent; 2]> =
            match packet.message {
                InputMessage::ReleaseAll => database.release_all(),
                InputMessage::FocusSync {
                    scroll_lock,
                    num_lock,
                    caps_lock,
                    kana_lock,
                } => {
                    let mut events = SmallVec::new();
                    events.push(synchronize_event(
                        scroll_lock,
                        num_lock,
                        caps_lock,
                        kana_lock,
                    ));
                    events
                }
                InputMessage::Key {
                    code,
                    extended,
                    pressed,
                } => {
                    let scancode = Scancode::from_u8(extended, code);
                    let operation = if pressed {
                        Operation::KeyPressed(scancode)
                    } else {
                        Operation::KeyReleased(scancode)
                    };
                    database.apply([operation])
                }
                InputMessage::MouseMove { x, y } => {
                    database.apply([Operation::MouseMove(MousePosition { x, y })])
                }
                InputMessage::MouseButton {
                    button,
                    pressed,
                    x,
                    y,
                } => {
                    let Some(button) = MouseButton::from_web_button(button) else {
                        return Ok(());
                    };
                    let operation = if pressed {
                        Operation::MouseButtonPressed(button)
                    } else {
                        Operation::MouseButtonReleased(button)
                    };
                    database.apply([Operation::MouseMove(MousePosition { x, y }), operation])
                }
                InputMessage::Wheel {
                    vertical,
                    delta,
                    x,
                    y,
                } => database.apply([
                    Operation::MouseMove(MousePosition { x, y }),
                    Operation::WheelRotations(WheelRotations {
                        is_vertical: vertical,
                        rotation_units: delta,
                    }),
                ]),
                InputMessage::Unicode { codepoint, pressed } => {
                    let Some(character) = char::from_u32(codepoint) else {
                        return Ok(());
                    };
                    let operation = if pressed {
                        Operation::UnicodeKeyPressed(character)
                    } else {
                        Operation::UnicodeKeyReleased(character)
                    };
                    database.apply([operation])
                }
            };
        if events.is_empty() {
            return Ok(());
        }
        // `database` must not stay locked while we wait for queue capacity:
        // paste injection and clipboard callbacks also need it.
        drop(database);
        let attempts = if droppable { 1 } else { 10 };
        let mut pending = RdpInputEvent::FastPath(events);
        for attempt in 0..attempts {
            match input.try_send(pending) {
                Ok(()) => return Ok(()),
                Err(tokio::sync::mpsc::error::TrySendError::Full(event)) => {
                    pending = event;
                    if attempt + 1 == attempts {
                        break;
                    }
                    std::thread::sleep(Duration::from_millis(2));
                }
                Err(_) => return Ok(()),
            }
        }
        if !droppable {
            tracing::warn!("dropping RDP input event after retrying a full queue");
        }
        Ok(())
    }

    pub fn disconnect(&self) {
        if self.closed.swap(true, Ordering::SeqCst) {
            return;
        }
        if let Ok(input) = self.input.lock()
            && let Some(input) = input.as_ref()
        {
            input.request_close();
        }
    }

    fn apply_desktop_update(&self, update: DesktopUpdate) {
        let (pixels, width, height, region) = update.into_parts();
        let width = width.get();
        let height = height.get();
        let rect = Rect::new(
            region.left,
            region.top,
            region.right.saturating_sub(region.left).saturating_add(1),
            region.bottom.saturating_sub(region.top).saturating_add(1),
        );
        if let Ok(mut frame) = self.frame.lock() {
            if frame.width != width || frame.height != height {
                self.log_session(format!(
                    "server resize session={} {}x{} -> {}x{}",
                    self.session_id, frame.width, frame.height, width, height
                ));
            }
            frame.resize(width, height);
            frame.apply_pixels(&pixels, rect);
        }
    }

    fn apply_image(&self, pixels: &[u32], width: u16, height: u16) {
        if let Ok(mut frame) = self.frame.lock() {
            frame.resize(width, height);
            frame.apply_pixels(pixels, Rect::new(0, 0, width, height));
        }
    }
}

impl Drop for RdpSession {
    fn drop(&mut self) {
        if let Ok(input) = self.input.lock()
            && let Some(input) = input.as_ref()
        {
            input.request_close();
        }
    }
}

impl RdpSession {
    fn emit_cursor_default(&self) {
        self.log_session(format!("cursor session={} kind=default", self.session_id));
        self.publish_cursor(cursor_message(
            self.session_id,
            CURSOR_KIND_DEFAULT,
            0,
            0,
            0,
            0,
            &[],
        ));
    }

    fn emit_cursor_hidden(&self) {
        self.log_session(format!("cursor session={} kind=hidden", self.session_id));
        self.publish_cursor(cursor_message(
            self.session_id,
            CURSOR_KIND_HIDDEN,
            0,
            0,
            0,
            0,
            &[],
        ));
    }

    fn emit_cursor_bitmap(
        &self,
        width: u16,
        height: u16,
        hotspot_x: u16,
        hotspot_y: u16,
        bitmap: &[u8],
    ) {
        let expected = usize::from(width) * usize::from(height) * 4;
        if expected == 0 || bitmap.len() < expected {
            return;
        }
        self.log_session(format!(
            "cursor session={} kind=bitmap {}x{}",
            self.session_id, width, height
        ));
        self.publish_cursor(cursor_message(
            self.session_id,
            CURSOR_KIND_BITMAP,
            hotspot_x,
            hotspot_y,
            width,
            height,
            &bitmap[..expected],
        ));
    }

    fn publish_cursor(&self, message: Vec<u8>) {
        if self.emit_events
            && self.viewers.load(Ordering::SeqCst) > 0
            && let Err(error) = self.emitter.binary(CURSOR_CHANNEL, &message)
        {
            tracing::warn!(error = %error.message, "failed to emit RDP cursor");
        }
        if let Ok(mut cursor) = self.cursor.lock() {
            *cursor = Some(message);
        }
    }
}

fn cursor_message(
    session_id: u32,
    kind: u8,
    hotspot_x: u16,
    hotspot_y: u16,
    width: u16,
    height: u16,
    bitmap: &[u8],
) -> Vec<u8> {
    let header = CursorHeader {
        kind,
        session_id,
        hotspot_x,
        hotspot_y,
        width,
        height,
    }
    .encode();
    let mut message = Vec::with_capacity(header.len() + bitmap.len());
    message.extend_from_slice(&header);
    message.extend_from_slice(bitmap);
    message
}

async fn consume_output(
    mut output: ironrdp::client::output_channel::OutputEventReceiver,
    weak: Weak<RdpSession>,
    connect_tx: std::sync::mpsc::Sender<Result<(), PluginError>>,
) {
    let mut connect_tx = Some(connect_tx);
    while let Some(event) = output.recv().await {
        let Some(session) = weak.upgrade() else {
            return;
        };
        match event {
            RdpOutputEvent::Connected => {
                if let Ok(mut info) = session.disconnect_info.lock() {
                    *info = None;
                }
                session.set_state(SessionState::Connected);
                session.emit_state();
                if let Some(tx) = connect_tx.take() {
                    let _ = tx.send(Ok(()));
                }
            }
            RdpOutputEvent::DesktopUpdate(update) => session.apply_desktop_update(update),
            RdpOutputEvent::Image {
                buffer,
                width,
                height,
            } => {
                session.apply_image(&buffer, width.get(), height.get());
            }
            RdpOutputEvent::ConnectionFailure(error) => {
                let mapped = map_connector_error(&error);
                session.log_session(format!(
                    "connect failure session={} code={} message={}",
                    session.session_id, mapped.code, mapped.message
                ));
                session.set_state(SessionState::Failed {
                    code: mapped.code,
                    message: mapped.message.clone(),
                });
                session.emit_state();
                if let Some(tx) = connect_tx.take() {
                    let _ = tx.send(Err(mapped));
                }
            }
            RdpOutputEvent::AutoReconnecting {
                response,
                attempt,
                maximum_attempts,
                ..
            } => {
                session.log_session(format!(
                    "auto reconnect session={} attempt={}/{}",
                    session.session_id, attempt, maximum_attempts
                ));
                let _ = response.send(AutoReconnectDecision::Continue);
                session.set_state(SessionState::Reconnecting);
                session.emit_state();
                if session.emit_events {
                    let _ = session.emitter.event(
                        "rdp/reconnecting",
                        json!({
                            "connectionId": session.connection_id,
                            "sessionId": session.session_id,
                            "attempt": attempt,
                            "maximumAttempts": maximum_attempts,
                        }),
                    );
                }
            }
            RdpOutputEvent::AutoReconnected => {
                session.set_state(SessionState::Connected);
                session.emit_state();
            }
            RdpOutputEvent::PointerDefault => session.emit_cursor_default(),
            RdpOutputEvent::PointerHidden => session.emit_cursor_hidden(),
            RdpOutputEvent::PointerBitmap(pointer) => session.emit_cursor_bitmap(
                pointer.width,
                pointer.height,
                pointer.hotspot_x,
                pointer.hotspot_y,
                &pointer.bitmap_data,
            ),
            // The CSS cursor follows the local mouse; server-side coordinates
            // are only needed by clients that draw an overlay cursor themselves.
            RdpOutputEvent::PointerPosition { .. } => {}
            RdpOutputEvent::DisplayResizeFallback(reason) => {
                session.log_session(format!(
                    "display resize fallback session={} reason={reason:?}",
                    session.session_id
                ));
                if session.emit_events {
                    let _ = session.emitter.event(
                        "rdp/resizeFallback",
                        json!({
                            "connectionId": session.connection_id,
                            "sessionId": session.session_id,
                            "reason": format!("{reason:?}"),
                        }),
                    );
                }
            }
            RdpOutputEvent::Terminated(result) => {
                let (reason, detail) = match &result {
                    Ok(reason) => {
                        let detail = reason.to_string();
                        (classify_disconnect(&detail).to_string(), detail)
                    }
                    Err(error) => {
                        let detail = error.to_string();
                        ("terminated".to_string(), detail)
                    }
                };
                session.log_session(format!(
                    "terminated session={} reason={} detail={}",
                    session.session_id, reason, detail
                ));
                if let Ok(mut info) = session.disconnect_info.lock() {
                    *info = Some(DisconnectInfo {
                        reason: reason.clone(),
                        detail: detail.clone(),
                    });
                }
                if let Some(tx) = connect_tx.take() {
                    let message = match result {
                        Ok(_) => "RDP connection closed before activation".to_string(),
                        Err(error) => format!("RDP session terminated: {error}"),
                    };
                    let _ = tx.send(Err(PluginError::new(-32014, message)));
                }
                session.set_state(SessionState::Disconnected);
                session.emit_state();
                return;
            }
            _ => {}
        }
    }
}

fn frame_pump(weak: Weak<RdpSession>) {
    loop {
        std::thread::sleep(FRAME_INTERVAL);
        let Some(session) = weak.upgrade() else {
            return;
        };
        if session.closed.load(Ordering::SeqCst) {
            return;
        }
        session.pump_frame(&session.emitter);
    }
}

impl RdpSession {
    fn pump_frame(&self, emitter: &PluginEmitter) {
        if self.viewers.load(Ordering::SeqCst) == 0 {
            if let Ok(mut frame) = self.frame.lock() {
                frame.dirty.clear();
                frame.dirty_area = 0;
                frame.full = false;
            }
            return;
        }
        let now = Instant::now();
        let prepared = {
            let Ok(mut frame) = self.frame.lock() else {
                return;
            };
            let has_resize = frame.resize_pending.is_some();
            if frame.full && !should_emit_full(frame.last_full_emit, now, has_resize) {
                // Keep `full` set so the next tick sends the latest pixels.
                return;
            }
            if !frame.full && frame.dirty.is_empty() && !has_resize {
                return;
            }
            let resize = frame.resize_pending.take();
            let resize_seq = resize.map(|_| {
                frame.seq = frame.seq.wrapping_add(1);
                frame.seq
            });
            let is_full = frame.full;
            let mut rects: Vec<(Rect, u32, Vec<u8>)> = Vec::new();
            if is_full {
                let rect = Rect::new(0, 0, frame.width, frame.height);
                frame.seq = frame.seq.wrapping_add(1);
                let seq = frame.seq;
                let payload = copy_rect(&frame.rgba, frame.width, rect);
                frame.frames_sent += 1;
                frame.full_frames_sent += 1;
                frame.rects_sent += 1;
                frame.bytes_sent += payload.len() as u64;
                frame.last_full_emit = Some(now);
                frame.dirty.clear();
                frame.dirty_area = 0;
                rects.push((rect, seq, payload));
            } else {
                let mut tick_bytes = 0usize;
                while let Some(rect) = frame.dirty.pop() {
                    let size = usize::from(rect.width) * usize::from(rect.height) * 4;
                    if tick_bytes > 0 && tick_bytes + size > FRAME_TICK_BUDGET_BYTES {
                        // Send the remaining rectangles on the next tick.
                        frame.dirty.push(rect);
                        break;
                    }
                    frame.seq = frame.seq.wrapping_add(1);
                    let seq = frame.seq;
                    let payload = copy_rect(&frame.rgba, frame.width, rect);
                    tick_bytes += payload.len();
                    frame.frames_sent += 1;
                    frame.rects_sent += 1;
                    frame.bytes_sent += payload.len() as u64;
                    rects.push((rect, seq, payload));
                }
                frame.dirty_area = frame.dirty.iter().map(|rect| u64::from(rect.area())).sum();
            }
            frame.full = false;
            (frame.width, frame.height, rects, resize, resize_seq)
        };
        let (desktop_width, desktop_height, rects, resize, resize_seq) = prepared;
        if let Some((width, height)) = resize {
            let Some(seq) = resize_seq else {
                return;
            };
            self.log_session(format!(
                "emit resize session={} {}x{}",
                self.session_id, width, height
            ));
            let header = FrameHeader {
                frame_type: FRAME_TYPE_RESIZE,
                session_id: self.session_id,
                seq,
                desktop_width: width,
                desktop_height: height,
                rect: Rect::new(0, 0, width, height),
                row_start: 0,
                chunk_index: 0,
                chunk_count: 0,
            };
            if let Err(error) = emitter.binary(FRAME_CHANNEL, &header.encode()) {
                tracing::warn!(error = %error.message, "failed to emit RDP resize frame");
                return;
            }
        }
        for (rect, seq, payload) in rects {
            let mut chunks = chunk_rect(rect);
            let total = chunks.total();
            for (row_start, rows, chunk_index) in chunks.by_ref() {
                let row_bytes = usize::from(rect.width) * 4;
                let start = usize::from(row_start) * row_bytes;
                let end = start + usize::from(rows) * row_bytes;
                if end > payload.len() {
                    break;
                }
                let header = FrameHeader {
                    frame_type: FRAME_TYPE_BITMAP,
                    session_id: self.session_id,
                    seq,
                    desktop_width,
                    desktop_height,
                    rect,
                    row_start,
                    chunk_index,
                    chunk_count: total,
                };
                let mut message =
                    Vec::with_capacity(crate::protocol::FRAME_HEADER_LEN + end - start);
                message.extend_from_slice(&header.encode());
                message.extend_from_slice(&payload[start..end]);
                if let Err(error) = emitter.binary(FRAME_CHANNEL, &message) {
                    tracing::warn!(error = %error.message, "failed to emit RDP frame");
                    if let Ok(mut frame) = self.frame.lock() {
                        frame.full = true;
                    }
                    return;
                }
            }
        }
        if self.should_log_frame() {
            let stats = self.frame.lock().ok().map(|mut frame| {
                let stats = (
                    frame.frames_sent,
                    frame.full_frames_sent,
                    frame.rects_sent,
                    frame.bytes_sent,
                );
                frame.frames_sent = 0;
                frame.full_frames_sent = 0;
                frame.rects_sent = 0;
                frame.bytes_sent = 0;
                stats
            });
            if let Some((frames, full, rects, bytes)) = stats {
                self.log_session(format!(
                    "frames={frames} full={full} rects={rects} bytes={bytes}"
                ));
                if let Err(error) = self.emitter.event(
                    "rdp/stats",
                    json!({
                        "connectionId": self.connection_id,
                        "sessionId": self.session_id,
                        "fps": frames,
                        "fullFps": full,
                        "rects": rects,
                        "kbps": bytes.saturating_mul(8) / 1000,
                        "width": desktop_width,
                        "height": desktop_height,
                    }),
                ) {
                    tracing::debug!(error = %error.message, "failed to emit RDP stats");
                }
            }
        }
    }
}

fn should_emit_full(last: Option<Instant>, now: Instant, has_resize: bool) -> bool {
    if has_resize {
        return true;
    }
    last.is_none_or(|last| now.saturating_duration_since(last) >= FULL_FRAME_MIN_INTERVAL)
}

/// Maps IronRDP's human-readable disconnect description to a stable reason id
/// the UI can localize. The descriptions come from MS-RDPBCGR server error
/// info codes (see `ProtocolIndependentCode::description`).
fn classify_disconnect(detail: &str) -> &'static str {
    let lower = detail.to_ascii_lowercase();
    if lower.contains("another user connected") || lower.contains("disconnected_by_otherconnection")
    {
        "another-connection"
    } else if lower.contains("forced logoff")
        || lower.contains("logoff initiated by an administrative")
        || lower.contains("administrative tool")
    {
        "admin-disconnect"
    } else if lower.contains("idle session limit") {
        "idle-timeout"
    } else if lower.contains("active session limit") || lower.contains("logon timeout") {
        "session-timeout"
    } else if lower.contains("busy shutting down") || lower.contains("shutting down") {
        "server-shutdown"
    } else if lower.contains("busy rebooting") || lower.contains("rebooting") {
        "server-reboot"
    } else if lower.contains("out of available memory") {
        "server-memory"
    } else if lower.contains("denied the connection")
        || lower.contains("insufficient access privileges")
    {
        "server-denied"
    } else if lower.contains("logging off") || lower.contains("logoff") {
        "logoff"
    } else if lower.contains("server ended the session")
        || lower.contains("server initiated disconnect")
    {
        "server-initiated"
    } else {
        "other"
    }
}

impl RdpSession {
    fn log_session(&self, message: impl AsRef<str>) {
        if let Ok(mut slot) = self.log.lock()
            && let Some(file) = slot.as_mut()
        {
            let _ = writeln!(file, "{} {}", log_timestamp(), message.as_ref());
        }
    }

    fn should_log_frame(&self) -> bool {
        if let Ok(mut last) = self.last_frame_log.lock()
            && last.elapsed() >= Duration::from_secs(1)
        {
            *last = Instant::now();
            return true;
        }
        false
    }
}

fn diagnostics_log_path() -> Option<PathBuf> {
    let directory = std::env::var_os("DBX_PLUGIN_DATA_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(std::env::temp_dir);
    Some(directory.join("rdp.log"))
}

fn open_diagnostics_log() -> Option<fs::File> {
    let path = diagnostics_log_path()?;
    if let Some(parent) = path.parent() {
        let _ = fs::create_dir_all(parent);
    }
    let oversized = fs::metadata(&path)
        .map(|metadata| metadata.len() > 1024 * 1024)
        .unwrap_or(false);
    let file = if oversized {
        fs::File::create(&path)
    } else {
        fs::OpenOptions::new().create(true).append(true).open(&path)
    };
    file.ok()
}

fn log_timestamp() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|duration| duration.as_secs())
        .unwrap_or_default()
}

fn scale_rgba_nearest(
    source: &[u8],
    source_width: u16,
    source_height: u16,
    target_width: u16,
    target_height: u16,
) -> Vec<u8> {
    let source_width = usize::from(source_width);
    let source_height = usize::from(source_height);
    let target_width = usize::from(target_width);
    let target_height = usize::from(target_height);
    let mut target = vec![0u8; target_width * target_height * 4];
    if source_width == 0
        || source_height == 0
        || source.len() < source_width * source_height * 4
        || target_width == 0
        || target_height == 0
    {
        return target;
    }
    for y in 0..target_height {
        let source_y = (y * source_height / target_height).min(source_height - 1);
        for x in 0..target_width {
            let source_x = (x * source_width / target_width).min(source_width - 1);
            let source_offset = (source_y * source_width + source_x) * 4;
            let target_offset = (y * target_width + x) * 4;
            target[target_offset..target_offset + 4]
                .copy_from_slice(&source[source_offset..source_offset + 4]);
            target[target_offset + 3] = 0xFF;
        }
    }
    target
}

fn copy_rect(rgba: &[u8], desktop_width: u16, rect: Rect) -> Vec<u8> {
    let row_bytes = usize::from(rect.width) * 4;
    let mut out = Vec::with_capacity(row_bytes * usize::from(rect.height));
    for row in 0..usize::from(rect.height) {
        let start =
            ((usize::from(rect.y) + row) * usize::from(desktop_width) + usize::from(rect.x)) * 4;
        let end = start + row_bytes;
        if end > rgba.len() {
            break;
        }
        out.extend_from_slice(&rgba[start..end]);
    }
    out
}

fn text_operations(character: char) -> [Operation; 2] {
    match character {
        '\n' | '\r' => [
            Operation::KeyPressed(Scancode::from_u8(false, 0x1C)),
            Operation::KeyReleased(Scancode::from_u8(false, 0x1C)),
        ],
        '\t' => [
            Operation::KeyPressed(Scancode::from_u8(false, 0x0F)),
            Operation::KeyReleased(Scancode::from_u8(false, 0x0F)),
        ],
        _ => [
            Operation::UnicodeKeyPressed(character),
            Operation::UnicodeKeyReleased(character),
        ],
    }
}

fn performance_flags(profile: PerformanceProfile) -> PerformanceFlags {
    match profile {
        // Keep font smoothing and desktop composition on for maximum clarity.
        // `PerformanceFlags::empty()` would silently disable ClearType.
        PerformanceProfile::Quality => {
            PerformanceFlags::ENABLE_FONT_SMOOTHING | PerformanceFlags::ENABLE_DESKTOP_COMPOSITION
        }
        PerformanceProfile::Balanced => PerformanceFlags::default(),
        PerformanceProfile::Speed => {
            PerformanceFlags::DISABLE_WALLPAPER
                | PerformanceFlags::DISABLE_FULLWINDOWDRAG
                | PerformanceFlags::DISABLE_MENUANIMATIONS
                | PerformanceFlags::DISABLE_THEMING
                | PerformanceFlags::DISABLE_CURSOR_SHADOW
                | PerformanceFlags::DISABLE_CURSORSETTINGS
        }
    }
}

fn clipboard_temp_dir() -> String {
    let base = std::env::var_os("DBX_PLUGIN_DATA_DIR")
        .map(std::path::PathBuf::from)
        .unwrap_or_else(std::env::temp_dir);
    let dir = base.join("clipboard");
    let _ = std::fs::create_dir_all(&dir);
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        let _ = std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o700));
    }
    dir.to_string_lossy().into_owned()
}

fn map_connector_error(error: &ConnectorError) -> PluginError {
    let source = std::error::Error::source(error)
        .map(|source| source.to_string())
        .unwrap_or_default();
    let message = match error.kind() {
        ConnectorErrorKind::Credssp(_) => {
            if source.is_empty() {
                "Authentication failed (CredSSP/NLA)".to_string()
            } else {
                format!("Authentication failed: {source}")
            }
        }
        ConnectorErrorKind::AccessDenied => "Access denied by the remote host".to_string(),
        ConnectorErrorKind::Negotiation(_) => "RDP security negotiation failed".to_string(),
        _ => {
            let text = error.to_string();
            if text.to_ascii_lowercase().contains("cert") {
                format!("Server certificate was rejected: {text}")
            } else {
                text
            }
        }
    };
    let lower = message.to_ascii_lowercase();
    let code = if lower.contains("cert") {
        -32012
    } else if lower.contains("credssp")
        || lower.contains("authentication")
        || lower.contains("access denied")
        || lower.contains("logon")
    {
        -32013
    } else if lower.contains("timeout") || lower.contains("timed out") {
        -32014
    } else {
        -32000
    };
    PluginError::new(code, message)
}

// The input channel name is part of the public protocol; keep it referenced
// here so a future refactor cannot silently rename only one side.
#[allow(dead_code)]
const INPUT_CHANNEL_NAME: &str = crate::protocol::INPUT_CHANNEL;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rgba_conversion_matches_channel_order() {
        let mut frame = FrameState::new(2, 1);
        frame.apply_pixels(&[0x0011_2233, 0x00AA_BBCC], Rect::new(0, 0, 2, 1));
        assert_eq!(&frame.rgba[0..4], &[0x11, 0x22, 0x33, 0xFF]);
        assert_eq!(&frame.rgba[4..8], &[0xAA, 0xBB, 0xCC, 0xFF]);
    }

    #[test]
    fn resize_keeps_previous_picture_scaled() {
        let mut frame = FrameState::new(2, 2);
        frame.full = false;
        frame.apply_pixels(
            &[0x00FF_0000, 0x0000_FF00, 0x0000_00FF, 0x00FF_FFFF],
            Rect::new(0, 0, 2, 2),
        );
        frame.resize(4, 4);
        assert!(frame.full);
        assert_eq!(frame.resize_pending, Some((4, 4)));
        let pixel = |x: usize, y: usize| {
            let start = (y * 4 + x) * 4;
            [
                frame.rgba[start],
                frame.rgba[start + 1],
                frame.rgba[start + 2],
                frame.rgba[start + 3],
            ]
        };
        assert_eq!(pixel(0, 0), [0xFF, 0x00, 0x00, 0xFF]);
        assert_eq!(pixel(2, 2), [0xFF, 0xFF, 0xFF, 0xFF]);
        assert_eq!(pixel(3, 3), [0xFF, 0xFF, 0xFF, 0xFF]);
        assert!(
            frame
                .rgba
                .chunks_exact(4)
                .any(|pixel| pixel[0] | pixel[1] | pixel[2] != 0)
        );
    }

    #[test]
    fn first_full_black_after_resize_is_skipped() {
        let mut frame = FrameState::new(2, 2);
        frame.apply_pixels(
            &[0x00FF_0000, 0x0000_FF00, 0x0000_00FF, 0x00FF_FFFF],
            Rect::new(0, 0, 2, 2),
        );
        frame.resize(4, 4);
        // The server often sends several all-black blanks while reactivating.
        for _ in 0..3 {
            frame.apply_pixels(&[0; 16], Rect::new(0, 0, 4, 4));
        }
        assert!(
            frame
                .rgba
                .chunks_exact(4)
                .any(|pixel| pixel[0] | pixel[1] | pixel[2] != 0)
        );
        // Later black frames are legitimate content and must be applied.
        frame.apply_pixels(&[0; 16], Rect::new(0, 0, 4, 4));
        assert!(
            frame
                .rgba
                .chunks_exact(4)
                .all(|pixel| pixel[0] | pixel[1] | pixel[2] == 0)
        );
    }

    #[test]
    fn resize_survives_a_full_black_blank() {
        let mut frame = FrameState::new(2, 2);
        frame.apply_pixels(
            &[0x00FF_0000, 0x0000_FF00, 0x0000_00FF, 0x00FF_FFFF],
            Rect::new(0, 0, 2, 2),
        );
        // A server often blanks the session before applying a display resize.
        frame.apply_pixels(&[0, 0, 0, 0], Rect::new(0, 0, 2, 2));
        frame.resize(4, 4);
        assert!(
            frame
                .rgba
                .chunks_exact(4)
                .any(|pixel| pixel[0] | pixel[1] | pixel[2] != 0)
        );
    }

    #[test]
    fn packed_region_is_written_at_its_offset() {
        let mut frame = FrameState::new(4, 3);
        frame.full = false;
        frame.apply_pixels(
            &[0x00FF_0000, 0x0000_FF00, 0x0000_00FF, 0x0011_2233],
            Rect::new(1, 1, 2, 2),
        );
        let pixel = |x: usize, y: usize| {
            let start = (y * 4 + x) * 4;
            [
                frame.rgba[start],
                frame.rgba[start + 1],
                frame.rgba[start + 2],
                frame.rgba[start + 3],
            ]
        };
        assert_eq!(pixel(0, 0), [0, 0, 0, 0]);
        assert_eq!(pixel(1, 1), [0xFF, 0x00, 0x00, 0xFF]);
        assert_eq!(pixel(2, 1), [0x00, 0xFF, 0x00, 0xFF]);
        assert_eq!(pixel(1, 2), [0x00, 0x00, 0xFF, 0xFF]);
        assert_eq!(pixel(2, 2), [0x11, 0x22, 0x33, 0xFF]);
        assert_eq!(pixel(3, 2), [0, 0, 0, 0]);
        assert!(!frame.full);
    }

    #[test]
    fn copy_rect_reads_only_the_region() {
        let mut rgba = vec![0u8; 2 * 2 * 4];
        for (index, byte) in rgba.iter_mut().enumerate() {
            *byte = u8::try_from(index % 251).unwrap();
        }
        let copy = copy_rect(&rgba, 2, Rect::new(1, 1, 1, 1));
        assert_eq!(copy.len(), 4);
        assert_eq!(&copy[..], &rgba[12..16]);
    }

    #[test]
    fn dirty_region_promotes_to_full_frame() {
        let mut frame = FrameState::new(100, 100);
        frame.full = false;
        frame.apply_pixels(&vec![0x00FF_FFFF; 100 * 100], Rect::new(0, 0, 100, 100));
        assert!(frame.full);
    }

    #[test]
    fn far_apart_dirty_regions_stay_separate() {
        let mut frame = FrameState::new(100, 100);
        frame.full = false;
        frame.apply_pixels(&[0x00FF_FFFF; 4], Rect::new(0, 0, 2, 2));
        frame.apply_pixels(&[0x00FF_FFFF; 4], Rect::new(90, 90, 2, 2));
        assert_eq!(frame.dirty.len(), 2);
        assert!(!frame.full);
        assert_eq!(frame.dirty_area, 8);
    }

    #[test]
    fn adjacent_dirty_regions_merge() {
        let mut frame = FrameState::new(100, 100);
        frame.full = false;
        frame.apply_pixels(&[0x00FF_FFFF; 4], Rect::new(0, 0, 2, 2));
        frame.apply_pixels(&[0x00FF_FFFF; 4], Rect::new(2, 0, 2, 2));
        assert_eq!(frame.dirty.len(), 1);
        assert_eq!(frame.dirty[0], Rect::new(0, 0, 4, 2));
    }

    #[test]
    fn dirty_rect_list_is_bounded() {
        let mut frame = FrameState::new(200, 200);
        frame.full = false;
        for index in 0..40u16 {
            let x = (index % 20) * 10;
            let y = (index / 20) * 10;
            frame.apply_pixels(&[0x00FF_FFFF; 4], Rect::new(x, y, 2, 2));
        }
        assert!(frame.dirty.len() <= MAX_DIRTY_RECTS);
    }

    #[test]
    fn quality_profile_keeps_font_smoothing() {
        let flags = performance_flags(PerformanceProfile::Quality);
        assert!(flags.contains(PerformanceFlags::ENABLE_FONT_SMOOTHING));
        assert!(flags.contains(PerformanceFlags::ENABLE_DESKTOP_COMPOSITION));
    }

    #[test]
    fn full_frames_are_throttled() {
        let now = Instant::now();
        assert!(should_emit_full(None, now, false));
        assert!(!should_emit_full(Some(now), now, false));
        assert!(should_emit_full(
            Some(now),
            now + FULL_FRAME_MIN_INTERVAL,
            false
        ));
        assert!(should_emit_full(Some(now), now, true));
    }

    #[test]
    fn text_operations_handle_newlines_tabs_and_unicode() {
        assert!(matches!(
            text_operations('\n')[0],
            Operation::KeyPressed(code) if code == Scancode::from_u8(false, 0x1C)
        ));
        assert!(matches!(
            text_operations('\t')[0],
            Operation::KeyPressed(code) if code == Scancode::from_u8(false, 0x0F)
        ));
        assert!(matches!(
            text_operations('中')[0],
            Operation::UnicodeKeyPressed('中')
        ));
        assert!(matches!(
            text_operations('中')[1],
            Operation::UnicodeKeyReleased('中')
        ));
    }

    #[test]
    fn classifies_known_disconnect_reasons() {
        assert_eq!(
            classify_disconnect(
                "[Protocol independent error] Another user connected to the server, forcing the disconnection of the current connection"
            ),
            "another-connection"
        );
        assert_eq!(
            classify_disconnect(
                "[Protocol independent error] The disconnection was initiated by an administrative tool on the server in another session"
            ),
            "admin-disconnect"
        );
        assert_eq!(
            classify_disconnect(
                "[Protocol independent error] The idle session limit timer on the server has elapsed"
            ),
            "idle-timeout"
        );
        assert_eq!(
            classify_disconnect(
                "[Protocol independent error] The remote server is busy shutting down"
            ),
            "server-shutdown"
        );
        assert_eq!(
            classify_disconnect("[Protocol independent error] The remote server is busy rebooting"),
            "server-reboot"
        );
        assert_eq!(
            classify_disconnect("server initiated disconnect"),
            "server-initiated"
        );
        assert_eq!(classify_disconnect("something unexpected"), "other");
    }
}
