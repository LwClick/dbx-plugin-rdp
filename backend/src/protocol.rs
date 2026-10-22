//! Binary protocols shared by the sidecar and the sandboxed workbench UI.
//!
//! Frames are sent on `rdp-frame-v1`, input on `rdp-input-v1`. Both messages
//! carry an explicit session id because a single sidecar process serves every
//! saved RDP connection of the plugin.

pub const FRAME_CHANNEL: &str = "rdp-frame-v1";
pub const INPUT_CHANNEL: &str = "rdp-input-v1";

pub const FRAME_MAGIC: u8 = 0xD1;
pub const INPUT_MAGIC: u8 = 0xD2;
pub const PROTOCOL_VERSION: u8 = 1;

pub const FRAME_HEADER_LEN: usize = 32;
pub const INPUT_HEADER_LEN: usize = 8;

/// The host bridge rejects a single binary message above 8 MiB. Keep every
/// chunk well below that, and row-aligned so the UI can blit it directly.
pub const MAX_CHUNK_BYTES: usize = 4 * 1024 * 1024;

pub const MIN_DESKTOP_DIM: u16 = 200;
pub const MAX_DESKTOP_DIM: u16 = 8192;

pub const FRAME_TYPE_BITMAP: u8 = 1;
pub const FRAME_TYPE_RESIZE: u8 = 2;

pub const INPUT_TYPE_KEY: u8 = 1;
pub const INPUT_TYPE_MOUSE_MOVE: u8 = 2;
pub const INPUT_TYPE_MOUSE_BUTTON: u8 = 3;
pub const INPUT_TYPE_WHEEL: u8 = 4;
pub const INPUT_TYPE_UNICODE: u8 = 5;
pub const INPUT_TYPE_RELEASE_ALL: u8 = 6;
pub const INPUT_TYPE_FOCUS_SYNC: u8 = 7;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Rect {
    pub x: u16,
    pub y: u16,
    pub width: u16,
    pub height: u16,
}

impl Rect {
    pub const fn new(x: u16, y: u16, width: u16, height: u16) -> Self {
        Self {
            x,
            y,
            width,
            height,
        }
    }

    pub const fn is_empty(self) -> bool {
        self.width == 0 || self.height == 0
    }

    pub fn area(self) -> u32 {
        u32::from(self.width) * u32::from(self.height)
    }

    pub fn right(self) -> u32 {
        u32::from(self.x) + u32::from(self.width)
    }

    pub fn bottom(self) -> u32 {
        u32::from(self.y) + u32::from(self.height)
    }

    pub fn union(self, other: Self) -> Self {
        if self.is_empty() {
            return other;
        }
        if other.is_empty() {
            return self;
        }
        let left = u32::from(self.x).min(u32::from(other.x));
        let top = u32::from(self.y).min(u32::from(other.y));
        let right = self.right().max(other.right());
        let bottom = self.bottom().max(other.bottom());
        Self {
            x: clamp_u16(left),
            y: clamp_u16(top),
            width: clamp_u16(right.saturating_sub(left)),
            height: clamp_u16(bottom.saturating_sub(top)),
        }
    }
}

fn clamp_u16(value: u32) -> u16 {
    u16::try_from(value).unwrap_or(u16::MAX)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FrameHeader {
    pub frame_type: u8,
    pub session_id: u32,
    pub seq: u32,
    pub desktop_width: u16,
    pub desktop_height: u16,
    pub rect: Rect,
    pub row_start: u16,
    pub chunk_index: u16,
    pub chunk_count: u16,
}

impl FrameHeader {
    pub fn encode(self) -> [u8; FRAME_HEADER_LEN] {
        let mut out = [0u8; FRAME_HEADER_LEN];
        out[0] = FRAME_MAGIC;
        out[1] = PROTOCOL_VERSION;
        out[2] = self.frame_type;
        out[3] = 0;
        out[4..8].copy_from_slice(&self.session_id.to_be_bytes());
        out[8..12].copy_from_slice(&self.seq.to_be_bytes());
        out[12..14].copy_from_slice(&self.desktop_width.to_be_bytes());
        out[14..16].copy_from_slice(&self.desktop_height.to_be_bytes());
        out[16..18].copy_from_slice(&self.rect.x.to_be_bytes());
        out[18..20].copy_from_slice(&self.rect.y.to_be_bytes());
        out[20..22].copy_from_slice(&self.rect.width.to_be_bytes());
        out[22..24].copy_from_slice(&self.rect.height.to_be_bytes());
        out[24..26].copy_from_slice(&self.row_start.to_be_bytes());
        out[26..28].copy_from_slice(&self.chunk_index.to_be_bytes());
        out[28..30].copy_from_slice(&self.chunk_count.to_be_bytes());
        out[30..32].copy_from_slice(&0u16.to_be_bytes());
        out
    }

    #[cfg(test)]
    pub fn decode(bytes: &[u8]) -> Result<Self, ProtocolError> {
        if bytes.len() != FRAME_HEADER_LEN {
            return Err(ProtocolError::Truncated);
        }
        if bytes[0] != FRAME_MAGIC {
            return Err(ProtocolError::BadMagic);
        }
        if bytes[1] != PROTOCOL_VERSION {
            return Err(ProtocolError::UnsupportedVersion(bytes[1]));
        }
        let frame_type = bytes[2];
        if !matches!(frame_type, FRAME_TYPE_BITMAP | FRAME_TYPE_RESIZE) {
            return Err(ProtocolError::UnknownType(frame_type));
        }
        Ok(Self {
            frame_type,
            session_id: read_u32(&bytes[4..8]),
            seq: read_u32(&bytes[8..12]),
            desktop_width: read_u16(&bytes[12..14]),
            desktop_height: read_u16(&bytes[14..16]),
            rect: Rect::new(
                read_u16(&bytes[16..18]),
                read_u16(&bytes[18..20]),
                read_u16(&bytes[20..22]),
                read_u16(&bytes[22..24]),
            ),
            row_start: read_u16(&bytes[24..26]),
            chunk_index: read_u16(&bytes[26..28]),
            chunk_count: read_u16(&bytes[28..30]),
        })
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum InputMessage {
    Key {
        code: u8,
        extended: bool,
        pressed: bool,
    },
    MouseMove {
        x: u16,
        y: u16,
    },
    MouseButton {
        button: u8,
        pressed: bool,
        x: u16,
        y: u16,
    },
    Wheel {
        vertical: bool,
        delta: i16,
        x: u16,
        y: u16,
    },
    Unicode {
        codepoint: u32,
        pressed: bool,
    },
    ReleaseAll,
    FocusSync {
        scroll_lock: bool,
        num_lock: bool,
        caps_lock: bool,
        kana_lock: bool,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct InputPacket {
    pub session_id: u32,
    pub message: InputMessage,
}

pub fn parse_input(bytes: &[u8]) -> Result<InputPacket, ProtocolError> {
    if bytes.len() < INPUT_HEADER_LEN {
        return Err(ProtocolError::Truncated);
    }
    if bytes[0] != INPUT_MAGIC {
        return Err(ProtocolError::BadMagic);
    }
    if bytes[1] != PROTOCOL_VERSION {
        return Err(ProtocolError::UnsupportedVersion(bytes[1]));
    }
    let packet_type = bytes[2];
    let session_id = read_u32(&bytes[4..8]);
    let payload = &bytes[INPUT_HEADER_LEN..];
    let message = match packet_type {
        INPUT_TYPE_KEY => {
            if payload.len() != 3 {
                return Err(ProtocolError::InvalidPayload);
            }
            InputMessage::Key {
                code: payload[0],
                extended: payload[1] != 0,
                pressed: payload[2] != 0,
            }
        }
        INPUT_TYPE_MOUSE_MOVE => {
            if payload.len() != 4 {
                return Err(ProtocolError::InvalidPayload);
            }
            InputMessage::MouseMove {
                x: read_u16(&payload[0..2]),
                y: read_u16(&payload[2..4]),
            }
        }
        INPUT_TYPE_MOUSE_BUTTON => {
            if payload.len() != 6 {
                return Err(ProtocolError::InvalidPayload);
            }
            InputMessage::MouseButton {
                button: payload[0],
                pressed: payload[1] != 0,
                x: read_u16(&payload[2..4]),
                y: read_u16(&payload[4..6]),
            }
        }
        INPUT_TYPE_WHEEL => {
            if payload.len() != 7 {
                return Err(ProtocolError::InvalidPayload);
            }
            InputMessage::Wheel {
                vertical: payload[0] != 0,
                delta: read_i16(&payload[1..3]),
                x: read_u16(&payload[3..5]),
                y: read_u16(&payload[5..7]),
            }
        }
        INPUT_TYPE_UNICODE => {
            if payload.len() != 5 {
                return Err(ProtocolError::InvalidPayload);
            }
            InputMessage::Unicode {
                codepoint: read_u32(&payload[0..4]),
                pressed: payload[4] != 0,
            }
        }
        INPUT_TYPE_RELEASE_ALL => {
            if !payload.is_empty() {
                return Err(ProtocolError::InvalidPayload);
            }
            InputMessage::ReleaseAll
        }
        INPUT_TYPE_FOCUS_SYNC => {
            if payload.len() != 4 {
                return Err(ProtocolError::InvalidPayload);
            }
            InputMessage::FocusSync {
                scroll_lock: payload[0] != 0,
                num_lock: payload[1] != 0,
                caps_lock: payload[2] != 0,
                kana_lock: payload[3] != 0,
            }
        }
        other => return Err(ProtocolError::UnknownType(other)),
    };
    Ok(InputPacket {
        session_id,
        message,
    })
}

/// Row-aligned chunks for one changed rectangle.
pub fn chunk_rect(rect: Rect) -> Chunks {
    Chunks::new(rect)
}

pub struct Chunks {
    rect: Rect,
    rows_per_chunk: usize,
    next_row: u16,
    total_chunks: u16,
    index: u16,
}

impl Chunks {
    fn new(rect: Rect) -> Self {
        let row_bytes = usize::from(rect.width).saturating_mul(4);
        let rows_per_chunk = MAX_CHUNK_BYTES.checked_div(row_bytes).unwrap_or(0).max(1);
        let rows = usize::from(rect.height);
        let total_chunks = if rows == 0 {
            0
        } else {
            rows.div_ceil(rows_per_chunk)
        };
        Self {
            rect,
            rows_per_chunk,
            next_row: 0,
            total_chunks: u16::try_from(total_chunks).unwrap_or(u16::MAX),
            index: 0,
        }
    }

    pub fn total(&self) -> u16 {
        self.total_chunks
    }
}

impl Iterator for Chunks {
    type Item = (u16, u16, u16); // row_start, rows, chunk_index

    fn next(&mut self) -> Option<Self::Item> {
        if self.next_row >= self.rect.height || self.rect.width == 0 {
            return None;
        }
        let remaining = u32::from(self.rect.height - self.next_row);
        let rows = u32::try_from(self.rows_per_chunk)
            .unwrap_or(u32::MAX)
            .min(remaining);
        #[expect(clippy::cast_possible_truncation)]
        let rows = rows as u16;
        let item = (self.next_row, rows, self.index);
        self.next_row = self.next_row.saturating_add(rows);
        self.index = self.index.saturating_add(1);
        Some(item)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProtocolError {
    Truncated,
    BadMagic,
    UnsupportedVersion(u8),
    UnknownType(u8),
    InvalidPayload,
}

impl core::fmt::Display for ProtocolError {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            Self::Truncated => write!(f, "message is truncated"),
            Self::BadMagic => write!(f, "bad magic byte"),
            Self::UnsupportedVersion(version) => {
                write!(f, "unsupported protocol version {version}")
            }
            Self::UnknownType(kind) => write!(f, "unknown message type {kind}"),
            Self::InvalidPayload => write!(f, "invalid payload length"),
        }
    }
}

impl core::error::Error for ProtocolError {}

fn read_u16(bytes: &[u8]) -> u16 {
    u16::from_be_bytes([bytes[0], bytes[1]])
}

fn read_i16(bytes: &[u8]) -> i16 {
    i16::from_be_bytes([bytes[0], bytes[1]])
}

fn read_u32(bytes: &[u8]) -> u32 {
    u32::from_be_bytes([bytes[0], bytes[1], bytes[2], bytes[3]])
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn frame_header_round_trip() {
        let header = FrameHeader {
            frame_type: FRAME_TYPE_BITMAP,
            session_id: 7,
            seq: 42,
            desktop_width: 1920,
            desktop_height: 1080,
            rect: Rect::new(10, 20, 300, 200),
            row_start: 64,
            chunk_index: 1,
            chunk_count: 3,
        };
        let decoded = FrameHeader::decode(&header.encode()).expect("decode");
        assert_eq!(decoded, header);
    }

    #[test]
    fn rejects_bad_magic_and_version() {
        let header = FrameHeader {
            frame_type: FRAME_TYPE_BITMAP,
            session_id: 1,
            seq: 1,
            desktop_width: 800,
            desktop_height: 600,
            rect: Rect::new(0, 0, 1, 1),
            row_start: 0,
            chunk_index: 0,
            chunk_count: 1,
        };
        let mut bytes = header.encode();
        bytes[0] = 0;
        assert_eq!(FrameHeader::decode(&bytes), Err(ProtocolError::BadMagic));
        let mut bytes = header.encode();
        bytes[1] = 9;
        assert_eq!(
            FrameHeader::decode(&bytes),
            Err(ProtocolError::UnsupportedVersion(9))
        );
    }

    #[test]
    fn chunks_are_row_aligned_and_bounded() {
        let rect = Rect::new(0, 0, 1920, 1080);
        let chunks: Vec<_> = chunk_rect(rect).collect();
        assert_eq!(chunks.len(), 2);
        let total_rows: u16 = chunks.iter().map(|(_, rows, _)| *rows).sum();
        assert_eq!(total_rows, 1080);
        for (expected_index, (start, rows, index)) in chunks.into_iter().enumerate() {
            let bytes = usize::from(rows) * 1920 * 4;
            assert!(bytes <= MAX_CHUNK_BYTES);
            assert_eq!(usize::from(index), expected_index);
            assert!(start < 1080);
        }
    }

    #[test]
    fn rect_union_covers_both() {
        let a = Rect::new(10, 10, 20, 20);
        let b = Rect::new(50, 5, 10, 60);
        let union = a.union(b);
        assert_eq!(union, Rect::new(10, 5, 50, 60));
    }

    #[test]
    fn input_round_trip_key() {
        let mut bytes = vec![INPUT_MAGIC, PROTOCOL_VERSION, INPUT_TYPE_KEY, 0];
        bytes.extend_from_slice(&9u32.to_be_bytes());
        bytes.extend_from_slice(&[0x1D, 1, 1]);
        let packet = parse_input(&bytes).expect("parse");
        assert_eq!(packet.session_id, 9);
        assert_eq!(
            packet.message,
            InputMessage::Key {
                code: 0x1D,
                extended: true,
                pressed: true
            }
        );
    }

    #[test]
    fn input_rejects_truncated_payload() {
        let mut bytes = vec![INPUT_MAGIC, PROTOCOL_VERSION, INPUT_TYPE_WHEEL, 0];
        bytes.extend_from_slice(&1u32.to_be_bytes());
        bytes.extend_from_slice(&[1, 0]);
        assert_eq!(parse_input(&bytes), Err(ProtocolError::InvalidPayload));
    }
}
