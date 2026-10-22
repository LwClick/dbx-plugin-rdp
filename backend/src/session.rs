use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, Weak};
use std::time::Duration;

use dbx_plugin_sdk::{PluginEmitter, PluginError};
use ironrdp::client::config::{ClipboardType, ConfigBuilder, Destination};
use ironrdp::client::output_channel::output_channel;
use ironrdp::client::rdp::{
    AutoReconnectDecision, DesktopUpdate, RdpClient, RdpInputEvent, RdpInputSender, RdpOutputEvent,
};
use ironrdp::connector::{ConnectorError, ConnectorErrorKind};
use ironrdp::input::{
    Database, MouseButton, MousePosition, Operation, Scancode, WheelRotations, synchronize_event,
};
use ironrdp::pdu::rdp::capability_sets::MajorPlatformType;
use ironrdp::pdu::rdp::client_info::PerformanceFlags;
use ironrdp_tls::CertificateValidation;
use serde_json::json;
use smallvec::SmallVec;
use tokio::runtime::Handle;

use crate::cert::{CertPrompt, CertificateInfo, PinStore};
use crate::clipboard::RdpClipboardFactory;
use crate::credentials::{PerformanceProfile, RdpConnectOptions, SecurityMode};
use crate::protocol::{
    FRAME_CHANNEL, FRAME_TYPE_BITMAP, FRAME_TYPE_RESIZE, FrameHeader, InputMessage, InputPacket,
    MAX_DESKTOP_DIM, MIN_DESKTOP_DIM, Rect, chunk_rect,
};

const INITIAL_WIDTH: u16 = 1280;
const INITIAL_HEIGHT: u16 = 800;
const FRAME_INTERVAL: Duration = Duration::from_millis(33);

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
    emit_events: bool,
    state: Mutex<SessionState>,
    cert_info: Arc<Mutex<Option<CertificateInfo>>>,
    input: Mutex<Option<RdpInputSender>>,
    input_db: Mutex<Database>,
    frame: Mutex<FrameState>,
    clipboard_text: Arc<Mutex<Option<String>>>,
    viewers: AtomicUsize,
    emitter: PluginEmitter,
    runtime: Handle,
    closed: AtomicBool,
}

struct FrameState {
    width: u16,
    height: u16,
    rgba: Vec<u8>,
    dirty: Option<Rect>,
    full: bool,
    seq: u32,
    resize_pending: Option<(u16, u16)>,
}

impl FrameState {
    fn new(width: u16, height: u16) -> Self {
        Self {
            width,
            height,
            rgba: vec![0; usize::from(width) * usize::from(height) * 4],
            dirty: None,
            full: true,
            seq: 0,
            resize_pending: Some((width, height)),
        }
    }

    fn resize(&mut self, width: u16, height: u16) {
        if self.width == width && self.height == height {
            return;
        }
        self.width = width;
        self.height = height;
        self.rgba = vec![0; usize::from(width) * usize::from(height) * 4];
        self.dirty = None;
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
        self.dirty = Some(match self.dirty {
            Some(current) => current.union(rect),
            None => rect,
        });
        let dirty_area = self.dirty.map(Rect::area).unwrap_or(0);
        let desktop_area = u32::from(self.width) * u32::from(self.height);
        if desktop_area > 0 && dirty_area.saturating_mul(100) >= desktop_area.saturating_mul(60) {
            self.full = true;
        }
    }
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

        let mut builder = ConfigBuilder::new()
            .with_destination(Destination::from_parts(options.host.clone(), options.port))
            .with_username(options.username.clone())
            .with_password(options.password.to_string())
            .with_client_name("DBX-RDP")
            .with_client_build(19_041)
            .with_client_dir("C:\\Windows\\System32")
            .with_platform(MajorPlatformType::WINDOWS)
            .with_desktop_width(INITIAL_WIDTH)
            .with_desktop_height(INITIAL_HEIGHT)
            .with_desktop_scale_factor(100)
            .with_credssp(options.security_mode == SecurityMode::Nla)
            .with_tls(options.security_mode == SecurityMode::Tls)
            .with_autologon(true)
            .with_pointer_software_rendering(true)
            .with_certificate_validation(CertificateValidation::Strict)
            .with_certificate_validation_callback(cert_prompt.callback())
            .with_clipboard(if options.clipboard_sync {
                ClipboardType::Enable
            } else {
                ClipboardType::Disable
            })
            .with_performance_flags(performance_flags(options.performance_profile));
        if let Some(domain) = options
            .domain
            .as_deref()
            .filter(|domain| !domain.is_empty())
        {
            builder = builder.with_domain(domain.to_string());
        }
        let config = builder.build().map_err(|error| {
            PluginError::new(-32011, format!("Invalid RDP configuration: {error}"))
        })?;

        let clipboard_text = Arc::new(Mutex::new(None));
        let session = Arc::new(Self {
            connection_id: options.connection_id.clone(),
            session_id,
            name: options.name.clone(),
            host: options.connection_host.clone(),
            port: options.connection_port,
            security_mode: options.security_mode,
            emit_events,
            state: Mutex::new(SessionState::Connecting),
            cert_info,
            input: Mutex::new(None),
            input_db: Mutex::new(Database::new()),
            frame: Mutex::new(FrameState::new(INITIAL_WIDTH, INITIAL_HEIGHT)),
            clipboard_text: Arc::clone(&clipboard_text),
            viewers: AtomicUsize::new(0),
            emitter: emitter.clone(),
            runtime: runtime.clone(),
            closed: AtomicBool::new(false),
        });

        let (output_tx, output_rx) = output_channel(32);
        let mut client = RdpClient::new(config, output_tx);
        let input_sender = client.input_sender();
        if let Ok(mut slot) = session.input.lock() {
            *slot = Some(input_sender.clone());
        }
        if options.clipboard_sync {
            let factory = RdpClipboardFactory::new(
                input_sender,
                Arc::clone(&clipboard_text),
                emitter.clone(),
                session.connection_id.clone(),
                session.session_id,
                clipboard_temp_dir(),
            );
            client = client.with_cliprdr_backend_factory(Box::new(factory));
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
        if !self.emit_events {
            return;
        }
        let state = self.state();
        let mut payload = json!({
            "connectionId": self.connection_id,
            "sessionId": self.session_id,
            "state": state.label(),
        });
        if let SessionState::Failed { code, message } = &state {
            payload["errorCode"] = json!(code);
            payload["error"] = json!(message);
        }
        if let Err(error) = self.emitter.event("rdp/state", payload) {
            tracing::warn!(error = %error.message, "failed to emit RDP state");
        }
    }

    pub fn attach(&self) -> serde_json::Value {
        self.viewers.fetch_add(1, Ordering::SeqCst);
        if let Ok(mut frame) = self.frame.lock() {
            frame.full = true;
        }
        let (width, height) = self
            .frame
            .lock()
            .map(|frame| (frame.width, frame.height))
            .unwrap_or((INITIAL_WIDTH, INITIAL_HEIGHT));
        let certificate = self.cert_info.lock().ok().and_then(|info| info.clone());
        json!({
            "sessionId": self.session_id,
            "width": width,
            "height": height,
            "name": self.name,
            "host": self.host,
            "port": self.port,
            "certificate": certificate,
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

    pub fn set_clipboard(&self, text: String) {
        if let Ok(mut clipboard) = self.clipboard_text.lock() {
            *clipboard = Some(text);
        }
        self.advertise_clipboard();
    }

    pub fn paste(self: &Arc<Self>, text: String) {
        self.set_clipboard(text);
        let Some(input) = self.input.lock().ok().and_then(|input| input.clone()) else {
            return;
        };
        let session = Arc::clone(self);
        self.runtime.spawn(async move {
            // The clipboard advertisement and the fast-path paste are carried
            // on different RDP channels; give the server a moment to process
            // the format list before the keystroke arrives.
            tokio::time::sleep(Duration::from_millis(120)).await;
            let Ok(mut database) = session.input_db.lock() else {
                return;
            };
            // Drop modifiers the user may still be holding from the intercepted
            // Ctrl+V, then send one clean chord.
            let mut events = database.release_all();
            events.extend(database.apply([
                Operation::KeyPressed(Scancode::from_u8(false, 0x1D)),
                Operation::KeyPressed(Scancode::from_u8(false, 0x2F)),
                Operation::KeyReleased(Scancode::from_u8(false, 0x2F)),
                Operation::KeyReleased(Scancode::from_u8(false, 0x1D)),
            ]));
            let _ = input.try_send(RdpInputEvent::FastPath(events));
        });
    }

    fn advertise_clipboard(&self) {
        let Some(input) = self.input.lock().ok().and_then(|input| input.clone()) else {
            return;
        };
        let formats = vec![ironrdp::cliprdr::pdu::ClipboardFormat::new(
            ironrdp::cliprdr::pdu::ClipboardFormatId::CF_UNICODETEXT,
        )];
        if let Err(error) = input
            .send_clipboard(ironrdp::cliprdr::backend::ClipboardMessage::SendInitiateCopy(formats))
        {
            tracing::debug!(%error, "cannot advertise local clipboard");
        }
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
        if let Err(error) = input.try_send(RdpInputEvent::FastPath(events)) {
            tracing::debug!(%error, "dropping RDP input event (queue full)");
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
            RdpOutputEvent::DisplayResizeFallback(reason) => {
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
                frame.dirty = None;
                frame.full = false;
            }
            return;
        }
        let prepared = {
            let Ok(mut frame) = self.frame.lock() else {
                return;
            };
            let resize = frame.resize_pending.take();
            let rect = if frame.full {
                Rect::new(0, 0, frame.width, frame.height)
            } else {
                match frame.dirty.take() {
                    Some(rect) if !rect.is_empty() => rect,
                    Some(_) => return,
                    None => return,
                }
            };
            frame.full = false;
            frame.seq = frame.seq.wrapping_add(1);
            let seq = frame.seq;
            let payload = copy_rect(&frame.rgba, frame.width, rect);
            (frame.width, frame.height, rect, seq, payload, resize)
        };
        let (desktop_width, desktop_height, rect, seq, payload, resize) = prepared;
        if let Some((width, height)) = resize {
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
            let mut message = Vec::with_capacity(crate::protocol::FRAME_HEADER_LEN + end - start);
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

fn performance_flags(profile: PerformanceProfile) -> PerformanceFlags {
    match profile {
        PerformanceProfile::Quality => PerformanceFlags::empty(),
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
}
