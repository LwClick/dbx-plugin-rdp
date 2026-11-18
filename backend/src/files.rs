//! Clipboard file staging.
//!
//! Both directions go through the CLIPRDR file stream protocol. Local files
//! are staged under the plugin data directory and served on demand; files
//! copied on the remote side are downloaded into a temporary directory and
//! then either saved as-is or packed into a ZIP archive.

use std::collections::HashMap;
use std::collections::hash_map::DefaultHasher;
use std::fs::{self, File, OpenOptions};
use std::hash::{Hash, Hasher};
use std::io::{self, Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use ironrdp::cliprdr::pdu::{ClipboardFileAttributes, ClipboardFormatId, FileDescriptor};
use tokio::sync::oneshot;

pub const MAX_FILES: usize = 500;
pub const MAX_FILE_BYTES: u64 = 2 * 1024 * 1024 * 1024;
pub const MAX_TOTAL_BYTES: u64 = 4 * 1024 * 1024 * 1024;
pub const UPLOAD_CHUNK_BYTES: usize = 512 * 1024;
pub const DOWNLOAD_CHUNK_BYTES: u32 = 1024 * 1024;
/// Upper bound for a single FileContentsResponse we produce.
pub const MAX_RESPONSE_BYTES: u64 = 4 * 1024 * 1024;
/// Staged data older than this is removed when a new session starts.
const STALE_AGE: Duration = Duration::from_secs(60 * 60);
/// Leave room for the null terminator and the relative path prefix.
const MAX_WIRE_NAME_CHARS: usize = 259;

pub type FileChunkResult = Result<Vec<u8>, String>;

#[derive(Debug, Clone)]
pub struct StagedFile {
    pub name: String,
    pub relative_path: Option<String>,
    pub size: u64,
    pub path: PathBuf,
    pub written: u64,
    pub complete: bool,
}

#[derive(Debug)]
pub struct UploadState {
    pub token: String,
    pub dir: PathBuf,
    pub files: Vec<StagedFile>,
    pub total: u64,
}

#[derive(Debug, Clone)]
pub struct RemoteFileList {
    pub files: Vec<FileDescriptor>,
    pub clip_data_id: Option<u32>,
}

/// PNG received from the remote clipboard, staged on disk for the save banner.
#[derive(Debug, Clone)]
pub struct RemoteImage {
    pub token: String,
    pub path: PathBuf,
    pub size: u64,
    pub width: u32,
    pub height: u32,
}

#[derive(Debug, Clone)]
pub struct DownloadState {
    pub token: String,
    pub dir: PathBuf,
    pub files: Vec<FileDescriptor>,
    pub clip_data_id: Option<u32>,
    pub name: String,
    pub archive: PathBuf,
    pub size: u64,
}

#[derive(Default)]
pub struct ClipboardFiles {
    /// True once the remote acknowledged our initial FormatList. Sending paste
    /// or file requests before that errors out in the CLIPRDR client and would
    /// surface as a session failure.
    pub ready: bool,
    /// True when the server negotiated streamed file clipboard support.
    pub file_clip: bool,
    /// Remote copy that arrived during initialization; requested on ready.
    pub pending_remote_format: Option<ClipboardFormatId>,
    /// Text copied locally and advertised over CLIPRDR. Never persisted or logged.
    pub local_text: Option<String>,
    local_text_hash: u64,
    /// True while a paste waits for the remote to acknowledge our FormatList.
    pub pending_paste: bool,
    /// True once the remote accepted our latest text FormatList.
    pub advertised: bool,
    /// Paste calls blocked on the next FormatList response.
    pub paste_waiters: Vec<std::sync::mpsc::Sender<bool>>,
    pub upload: Option<UploadState>,
    pub download: Option<DownloadState>,
    pub remote_files: Option<RemoteFileList>,
    /// DIB payloads served to the remote when it requests an image format.
    /// Kept in memory only; image data never touches the staging directories.
    pub local_image_dib: Option<Vec<u8>>,
    pub local_image_dibv5: Option<Vec<u8>>,
    /// Image format the remote advertised last, requested instead of text.
    pub pending_image_request: Option<ClipboardFormatId>,
    pub remote_image: Option<RemoteImage>,
    /// Pending outgoing FileContentsRequest, keyed by stream id.
    pub pending: HashMap<u32, oneshot::Sender<FileChunkResult>>,
    next_stream_id: u32,
}

impl core::fmt::Debug for ClipboardFiles {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.debug_struct("ClipboardFiles")
            .field("upload", &self.upload)
            .field("download", &self.download)
            .field("remote_files", &self.remote_files)
            .field("pending", &self.pending.len())
            .field("local_text_len", &self.local_text.as_ref().map(String::len))
            .finish_non_exhaustive()
    }
}

impl Drop for ClipboardFiles {
    fn drop(&mut self) {
        // Staged files can contain user data; drop them with the session.
        if let Some(upload) = self.upload.take() {
            let _ = fs::remove_dir_all(&upload.dir);
        }
        if let Some(download) = self.download.take() {
            let _ = fs::remove_dir_all(&download.dir);
        }
        if let Some(image) = self.remote_image.take() {
            let _ = fs::remove_file(&image.path);
        }
    }
}

impl ClipboardFiles {
    /// Stores the text we will serve over CLIPRDR. Returns `true` when the
    /// content differs from the previously advertised text.
    pub fn set_local_text(&mut self, text: String) -> bool {
        let hash = text_hash(&text);
        let changed = self.local_text_hash != hash || self.local_text.is_none();
        self.local_text = Some(text);
        self.local_text_hash = hash;
        changed
    }

    pub fn has_local_text(&self) -> bool {
        self.local_text.is_some()
    }

    pub fn clear_local_text(&mut self) {
        self.local_text = None;
        self.local_text_hash = 0;
    }

    pub fn set_local_image(&mut self, dib: Vec<u8>, dibv5: Vec<u8>) {
        self.local_image_dib = Some(dib);
        self.local_image_dibv5 = Some(dibv5);
    }

    /// Returns the best available DIB payload for the requested format.
    pub fn local_image_data(&self, format: ClipboardFormatId) -> Option<&[u8]> {
        if format == ClipboardFormatId::CF_DIBV5 {
            self.local_image_dibv5
                .as_deref()
                .or(self.local_image_dib.as_deref())
        } else if format == ClipboardFormatId::CF_DIB {
            self.local_image_dib
                .as_deref()
                .or(self.local_image_dibv5.as_deref())
        } else {
            None
        }
    }

    /// Finishes a pending text paste once the remote answers our FormatList.
    ///
    /// File advertisements also produce `FormatListResponse`; only wake paste
    /// waiters when a paste is actually pending to avoid mistaking one for the
    /// other.
    pub fn acknowledge_format_list(&mut self, ok: bool) {
        self.advertised = ok;
        if !self.pending_paste {
            return;
        }
        self.pending_paste = false;
        for waiter in self.paste_waiters.drain(..) {
            let _ = waiter.send(ok);
        }
    }

    pub fn next_stream_id(&mut self) -> u32 {
        for _ in 0..u32::MAX {
            self.next_stream_id = self.next_stream_id.wrapping_add(1).max(1);
            if !self.pending.contains_key(&self.next_stream_id) {
                return self.next_stream_id;
            }
        }
        // Unreachable unless more than 4 billion requests are pending.
        self.next_stream_id = 1;
        1
    }

    pub fn begin_upload(
        &mut self,
        token: &str,
        name: &str,
        relative_path: Option<&str>,
        size: u64,
    ) -> Result<usize, String> {
        validate_token(token)?;
        if size > MAX_FILE_BYTES {
            return Err(format!(
                "File is too large: {} bytes (limit {} bytes)",
                size, MAX_FILE_BYTES
            ));
        }
        let name = sanitize_name(name, true).ok_or_else(|| "Invalid file name".to_string())?;
        let relative_path = match relative_path {
            Some(path) if !path.trim().is_empty() => Some(
                sanitize_relative_path(path, true)
                    .ok_or_else(|| "Invalid folder path".to_string())?,
            ),
            _ => None,
        };
        let wire_len = relative_path
            .as_ref()
            .map(|path| path.chars().count() + 1)
            .unwrap_or(0)
            + name.chars().count();
        if wire_len > MAX_WIRE_NAME_CHARS {
            return Err("File name is too long".to_string());
        }

        if self
            .upload
            .as_ref()
            .is_none_or(|upload| upload.token != token)
        {
            self.clear_upload();
            let dir = prepare_dir(&uploads_root().join(token))?;
            self.upload = Some(UploadState {
                token: token.to_string(),
                dir,
                files: Vec::new(),
                total: 0,
            });
        }
        let upload = self
            .upload
            .as_mut()
            .ok_or_else(|| "Upload state is unavailable".to_string())?;
        if upload.files.len() >= MAX_FILES {
            return Err(format!("Too many files (limit {MAX_FILES})"));
        }
        let new_total = upload.total.saturating_add(size);
        if new_total > MAX_TOTAL_BYTES {
            return Err(format!(
                "Upload exceeds the total size limit ({} bytes)",
                MAX_TOTAL_BYTES
            ));
        }
        let index = upload.files.len();
        let path = upload.dir.join(format!("{index}.bin"));
        let file = OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&path)
            .map_err(|error| format!("Cannot create staging file: {error}"))?;
        set_file_permissions(&file);
        drop(file);
        upload.files.push(StagedFile {
            name,
            relative_path,
            size,
            path,
            written: 0,
            complete: false,
        });
        upload.total = new_total;
        Ok(index)
    }

    pub fn write_upload_chunk(
        &mut self,
        token: &str,
        index: usize,
        offset: u64,
        data: &[u8],
    ) -> Result<u64, String> {
        let upload = self
            .upload
            .as_mut()
            .filter(|upload| upload.token == token)
            .ok_or_else(|| "Unknown upload".to_string())?;
        let staged = upload
            .files
            .get_mut(index)
            .ok_or_else(|| "Unknown file index".to_string())?;
        if offset != staged.written {
            return Err(format!(
                "Unexpected chunk offset {offset}, expected {}",
                staged.written
            ));
        }
        let end = staged
            .written
            .checked_add(data.len() as u64)
            .ok_or_else(|| "File is too large".to_string())?;
        if end > staged.size {
            return Err("Chunk exceeds the declared file size".to_string());
        }
        let mut file = OpenOptions::new()
            .write(true)
            .open(&staged.path)
            .map_err(|error| format!("Cannot open staging file: {error}"))?;
        file.seek(SeekFrom::Start(offset))
            .map_err(|error| format!("Cannot seek staging file: {error}"))?;
        file.write_all(data)
            .map_err(|error| format!("Cannot write staging file: {error}"))?;
        staged.written = end;
        Ok(end)
    }

    pub fn finish_upload(&mut self, token: &str, index: usize) -> Result<(), String> {
        let upload = self
            .upload
            .as_mut()
            .filter(|upload| upload.token == token)
            .ok_or_else(|| "Unknown upload".to_string())?;
        let staged = upload
            .files
            .get_mut(index)
            .ok_or_else(|| "Unknown file index".to_string())?;
        if staged.written != staged.size {
            return Err(format!(
                "Upload incomplete: {}/{} bytes",
                staged.written, staged.size
            ));
        }
        staged.complete = true;
        Ok(())
    }

    pub fn cancel_upload(&mut self, token: &str) {
        if self
            .upload
            .as_ref()
            .is_some_and(|upload| upload.token == token)
        {
            self.clear_upload();
        }
    }

    pub fn clear_upload(&mut self) {
        if let Some(upload) = self.upload.take() {
            let _ = fs::remove_dir_all(&upload.dir);
        }
    }

    /// Builds the descriptors advertised on the remote clipboard.
    pub fn advertised_files(&self, token: &str) -> Result<Vec<FileDescriptor>, String> {
        let upload = self
            .upload
            .as_ref()
            .filter(|upload| upload.token == token)
            .ok_or_else(|| "Unknown upload".to_string())?;
        if upload.files.is_empty() {
            return Err("No files to send".to_string());
        }
        if upload.files.iter().any(|file| !file.complete) {
            return Err("Upload has not finished".to_string());
        }
        Ok(upload
            .files
            .iter()
            .map(|file| {
                let mut descriptor = FileDescriptor::new(file.name.clone())
                    .with_file_size(file.size)
                    .with_attributes(ClipboardFileAttributes::ARCHIVE);
                if let Some(path) = &file.relative_path {
                    descriptor = descriptor.with_relative_path(path.replace('/', "\\"));
                }
                descriptor
            })
            .collect())
    }

    pub fn read_upload_range(
        &self,
        index: usize,
        position: u64,
        length: u32,
    ) -> Result<Vec<u8>, String> {
        let upload = self
            .upload
            .as_ref()
            .ok_or_else(|| "No local files".to_string())?;
        let staged = upload
            .files
            .get(index)
            .ok_or_else(|| "Unknown file index".to_string())?;
        if !staged.complete {
            return Err("File upload is incomplete".to_string());
        }
        let length = u64::from(length).min(MAX_RESPONSE_BYTES);
        let remaining = staged.size.saturating_sub(position);
        if position > staged.size {
            return Err("File offset is out of range".to_string());
        }
        let length = length.min(remaining);
        let mut file = File::open(&staged.path)
            .map_err(|error| format!("Cannot open staged file: {error}"))?;
        file.seek(SeekFrom::Start(position))
            .map_err(|error| format!("Cannot seek staged file: {error}"))?;
        let mut data = vec![0u8; length as usize];
        file.read_exact(&mut data)
            .map_err(|error| format!("Cannot read staged file: {error}"))?;
        Ok(data)
    }

    pub fn upload_file_size(&self, index: usize) -> Option<u64> {
        self.upload
            .as_ref()
            .and_then(|upload| upload.files.get(index))
            .map(|file| file.size)
    }

    pub fn begin_download(
        &mut self,
        files: Vec<FileDescriptor>,
        clip_data_id: Option<u32>,
    ) -> Result<DownloadState, String> {
        if files.is_empty() {
            return Err("No remote files to download".to_string());
        }
        if files.len() > MAX_FILES {
            return Err(format!("Too many files (limit {MAX_FILES})"));
        }
        let total: u64 = files.iter().filter_map(|file| file.file_size).sum();
        if total > MAX_TOTAL_BYTES {
            return Err(format!(
                "Remote files exceed the total size limit ({} bytes)",
                MAX_TOTAL_BYTES
            ));
        }
        let token = new_token();
        let dir = prepare_dir(&downloads_root().join(&token))?;
        let state = DownloadState {
            token,
            dir,
            files,
            clip_data_id,
            name: String::new(),
            archive: PathBuf::new(),
            size: 0,
        };
        if let Some(previous) = self.download.replace(state) {
            let _ = fs::remove_dir_all(&previous.dir);
        }
        Ok(self
            .download
            .as_ref()
            .expect("download state was just inserted")
            .clone())
    }

    pub fn clear_download(&mut self, token: &str) {
        if self
            .download
            .as_ref()
            .is_some_and(|state| state.token == token)
            && let Some(state) = self.download.take()
        {
            let _ = fs::remove_dir_all(&state.dir);
        }
    }

    pub fn finish_download(&mut self, token: &str) -> Result<(String, PathBuf, u64), String> {
        let state = self
            .download
            .as_mut()
            .filter(|state| state.token == token)
            .ok_or_else(|| "Unknown download".to_string())?;
        let mut entries: Vec<(String, PathBuf)> = Vec::new();
        let mut used: HashMap<String, usize> = HashMap::new();
        for (index, descriptor) in state.files.iter().enumerate() {
            let name = sanitize_name(&descriptor.name, cfg!(windows))
                .unwrap_or_else(|| format!("file-{index}"));
            let relative = descriptor
                .relative_path
                .as_deref()
                .and_then(|path| sanitize_relative_path(path, cfg!(windows)));
            let entry = match relative {
                Some(path) => format!("{path}/{name}"),
                None => name,
            };
            // Two remote folders can contain the same relative name; keep both
            // by suffixing the duplicate instead of overwriting an entry.
            let entry = if used.contains_key(&entry) {
                match entry.rsplit_once('.') {
                    Some((stem, extension)) if !stem.is_empty() => {
                        format!("{stem}-{index}.{extension}")
                    }
                    _ => format!("{entry}-{index}"),
                }
            } else {
                entry
            };
            used.insert(entry.clone(), index);
            entries.push((entry, state.dir.join(format!("{index}.bin"))));
        }

        let (name, archive) = if entries.len() == 1 {
            let (entry, source) = &entries[0];
            let name = entry.rsplit('/').next().unwrap_or(entry).to_string();
            let destination = state.dir.join(&name);
            if source != &destination {
                fs::rename(source, &destination)
                    .or_else(|_| fs::copy(source, &destination).map(|_| ()))
                    .map_err(|error| format!("Cannot finalize downloaded file: {error}"))?;
            }
            (name, destination)
        } else {
            let name = format!("rdp-files-{}.zip", timestamp());
            let destination = state.dir.join(&name);
            write_zip(&entries, &destination)
                .map_err(|error| format!("Cannot create archive: {error}"))?;
            (name, destination)
        };
        let size = fs::metadata(&archive)
            .map(|metadata| metadata.len())
            .map_err(|error| format!("Cannot inspect downloaded file: {error}"))?;
        state.name = name.clone();
        state.archive = archive.clone();
        state.size = size;
        Ok((name, archive, size))
    }

    /// Stages a PNG received from the remote clipboard. The previous image is
    /// removed so only the current clipboard image stays on disk.
    pub fn store_remote_image(&mut self, png: &[u8]) -> Result<RemoteImage, String> {
        if png.len() as u64 > crate::protocol::MAX_CLIPBOARD_IMAGE_BYTES as u64 {
            return Err("Remote clipboard image is too large".to_string());
        }
        let (width, height) = png_dimensions(png).ok_or("Invalid clipboard image")?;
        let dir = prepare_dir(&images_root())?;
        let token = new_token();
        let path = dir.join(format!("{token}.png"));
        let mut file = create_download_file(&path)
            .map_err(|error| format!("Cannot create clipboard image: {error}"))?;
        file.write_all(png)
            .map_err(|error| format!("Cannot write clipboard image: {error}"))?;
        let image = RemoteImage {
            token,
            path: path.clone(),
            size: png.len() as u64,
            width,
            height,
        };
        if let Some(previous) = self.remote_image.replace(image.clone()) {
            let _ = fs::remove_file(previous.path);
        }
        Ok(image)
    }

    pub fn read_remote_image(
        &self,
        token: &str,
        offset: u64,
        length: u64,
    ) -> Result<(Vec<u8>, bool), String> {
        let image = self
            .remote_image
            .as_ref()
            .filter(|image| image.token == token)
            .ok_or_else(|| "Unknown clipboard image".to_string())?;
        if offset > image.size {
            return Err("Image offset is out of range".to_string());
        }
        let length = length
            .min(u64::from(DOWNLOAD_CHUNK_BYTES))
            .min(MAX_RESPONSE_BYTES)
            .min(image.size - offset);
        let mut file = File::open(&image.path)
            .map_err(|error| format!("Cannot open clipboard image: {error}"))?;
        file.seek(SeekFrom::Start(offset))
            .map_err(|error| format!("Cannot seek clipboard image: {error}"))?;
        let mut data = vec![0u8; length as usize];
        file.read_exact(&mut data)
            .map_err(|error| format!("Cannot read clipboard image: {error}"))?;
        Ok((data, offset + length >= image.size))
    }

    pub fn clear_remote_image(&mut self, token: &str) {
        if self
            .remote_image
            .as_ref()
            .is_some_and(|image| image.token == token)
            && let Some(image) = self.remote_image.take()
        {
            let _ = fs::remove_file(&image.path);
        }
    }
}

fn validate_token(token: &str) -> Result<(), String> {
    if (8..=64).contains(&token.len())
        && token.chars().all(|character| {
            character.is_ascii_alphanumeric() || character == '-' || character == '_'
        })
    {
        Ok(())
    } else {
        Err("Invalid upload token".to_string())
    }
}

pub fn new_token() -> String {
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_nanos())
        .unwrap_or_default();
    format!("{nanos:x}{:x}", std::process::id())
}

fn text_hash(value: &str) -> u64 {
    let mut hasher = DefaultHasher::new();
    value.hash(&mut hasher);
    hasher.finish()
}

fn timestamp() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_secs())
        .unwrap_or_default()
}

fn data_root() -> PathBuf {
    std::env::var_os("DBX_PLUGIN_DATA_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(std::env::temp_dir)
        .join("clipboard")
}

pub fn uploads_root() -> PathBuf {
    data_root().join("uploads")
}

pub fn downloads_root() -> PathBuf {
    data_root().join("downloads")
}

pub fn images_root() -> PathBuf {
    data_root().join("images")
}

/// Removes staging directories left behind by a crashed or killed process.
pub fn prune_stale_directories() {
    for root in [uploads_root(), downloads_root(), images_root()] {
        let Ok(entries) = fs::read_dir(&root) else {
            continue;
        };
        for entry in entries.flatten() {
            let Ok(metadata) = entry.metadata() else {
                continue;
            };
            let Ok(modified) = metadata.modified() else {
                continue;
            };
            if modified
                .elapsed()
                .map(|age| age > STALE_AGE)
                .unwrap_or(false)
            {
                if metadata.is_dir() {
                    let _ = fs::remove_dir_all(entry.path());
                } else {
                    let _ = fs::remove_file(entry.path());
                }
            }
        }
    }
}

fn prepare_dir(path: &Path) -> Result<PathBuf, String> {
    fs::create_dir_all(path)
        .map_err(|error| format!("Cannot create staging directory: {error}"))?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        let _ = fs::set_permissions(path, fs::Permissions::from_mode(0o700));
    }
    Ok(path.to_path_buf())
}

fn set_file_permissions(file: &File) {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        let _ = file.set_permissions(fs::Permissions::from_mode(0o600));
    }
}

pub fn create_download_file(path: &Path) -> io::Result<File> {
    let file = OpenOptions::new().write(true).create_new(true).open(path)?;
    set_file_permissions(&file);
    Ok(file)
}

/// Reads the IHDR dimensions of a PNG without decoding pixel data.
pub fn png_dimensions(data: &[u8]) -> Option<(u32, u32)> {
    const SIGNATURE: [u8; 8] = [0x89, b'P', b'N', b'G', 0x0D, 0x0A, 0x1A, 0x0A];
    if data.len() < 24 || data[0..8] != SIGNATURE || &data[12..16] != b"IHDR" {
        return None;
    }
    let width = u32::from_be_bytes([data[16], data[17], data[18], data[19]]);
    let height = u32::from_be_bytes([data[20], data[21], data[22], data[23]]);
    (width > 0 && height > 0).then_some((width, height))
}

pub fn sanitize_name(raw: &str, windows: bool) -> Option<String> {
    let base = raw.rsplit(['/', '\\']).next().unwrap_or(raw).trim();
    if base.is_empty() || base == "." || base == ".." {
        return None;
    }
    let mut cleaned = String::with_capacity(base.len());
    for character in base.chars() {
        // Replace path separators, control characters and Windows-invalid
        // characters; a file name must never smuggle a directory component.
        if character.is_control()
            || matches!(character, '/' | '\\')
            || (windows && matches!(character, '<' | '>' | ':' | '"' | '|' | '?' | '*'))
        {
            cleaned.push('_');
        } else {
            cleaned.push(character);
        }
    }
    let cleaned = cleaned.trim_end_matches([' ', '.']).to_string();
    if cleaned.is_empty() {
        return None;
    }
    if windows && is_reserved_windows_name(&cleaned) {
        return Some(format!("{cleaned}_"));
    }
    Some(cleaned)
}

pub fn sanitize_relative_path(raw: &str, windows: bool) -> Option<String> {
    let normalized = raw.replace('\\', "/");
    let mut parts = Vec::new();
    for part in normalized.split('/') {
        if part.is_empty() || part == "." {
            continue;
        }
        // Never allow the remote side to escape the staging directory.
        if part == ".." {
            return None;
        }
        let part = sanitize_name(part, windows)?;
        parts.push(part);
    }
    if parts.is_empty() {
        None
    } else {
        Some(parts.join("/"))
    }
}

fn is_reserved_windows_name(name: &str) -> bool {
    let stem = name.split('.').next().unwrap_or(name).trim_end();
    let upper = stem.to_ascii_uppercase();
    matches!(upper.as_str(), "CON" | "PRN" | "AUX" | "NUL")
        || (upper.len() == 4
            && (upper.starts_with("COM") || upper.starts_with("LPT"))
            && upper.as_bytes()[3].is_ascii_digit())
}

/// Writes a store-only (uncompressed) ZIP archive.
pub fn write_zip(entries: &[(String, PathBuf)], destination: &Path) -> io::Result<u64> {
    let mut output = File::create(destination)?;
    set_file_permissions(&output);
    let mut central: Vec<CentralEntry> = Vec::with_capacity(entries.len());
    let mut offset: u64 = 0;

    for (name, source) in entries {
        let metadata = fs::metadata(source)?;
        let size = metadata.len();
        let name_bytes = name.as_bytes();
        let header_offset = offset;
        let mut header = Vec::with_capacity(30 + name_bytes.len());
        write_u32(&mut header, 0x0403_4b50);
        write_u16(&mut header, 20);
        write_u16(&mut header, 0x0800); // UTF-8 names
        write_u16(&mut header, 0); // stored
        write_u16(&mut header, 0); // time
        write_u16(&mut header, 0); // date
        write_u32(&mut header, 0); // crc, patched below
        write_u32(&mut header, u32::try_from(size).unwrap_or(u32::MAX));
        write_u32(&mut header, u32::try_from(size).unwrap_or(u32::MAX));
        write_u16(
            &mut header,
            u16::try_from(name_bytes.len()).unwrap_or(u16::MAX),
        );
        write_u16(&mut header, 0);
        header.extend_from_slice(name_bytes);
        output.write_all(&header)?;

        let mut input = File::open(source)?;
        let mut hasher = crc32fast::Hasher::new();
        let mut buffer = vec![0u8; 256 * 1024];
        let mut copied = 0u64;
        loop {
            let read = input.read(&mut buffer)?;
            if read == 0 {
                break;
            }
            hasher.update(&buffer[..read]);
            output.write_all(&buffer[..read])?;
            copied += read as u64;
        }
        if copied != size {
            return Err(io::Error::new(
                io::ErrorKind::UnexpectedEof,
                "file changed while packing the archive",
            ));
        }
        let crc = hasher.finalize();
        let end = output.seek(SeekFrom::End(0))?;
        output.seek(SeekFrom::Start(header_offset + 14))?;
        output.write_all(&crc.to_le_bytes())?;
        output.seek(SeekFrom::Start(end))?;
        offset = end;
        central.push(CentralEntry {
            name: name_bytes.to_vec(),
            crc,
            size,
            header_offset,
        });
    }

    let directory_offset = offset;
    let mut directory = Vec::new();
    for entry in &central {
        write_u32(&mut directory, 0x0201_4b50);
        write_u16(&mut directory, 20); // made by
        write_u16(&mut directory, 20); // needed
        write_u16(&mut directory, 0x0800);
        write_u16(&mut directory, 0);
        write_u16(&mut directory, 0);
        write_u16(&mut directory, 0);
        write_u32(&mut directory, entry.crc);
        write_u32(
            &mut directory,
            u32::try_from(entry.size).unwrap_or(u32::MAX),
        );
        write_u32(
            &mut directory,
            u32::try_from(entry.size).unwrap_or(u32::MAX),
        );
        write_u16(
            &mut directory,
            u16::try_from(entry.name.len()).unwrap_or(u16::MAX),
        );
        write_u16(&mut directory, 0);
        write_u16(&mut directory, 0);
        write_u16(&mut directory, 0);
        write_u16(&mut directory, 0);
        write_u32(&mut directory, 0);
        write_u32(
            &mut directory,
            u32::try_from(entry.header_offset).unwrap_or(u32::MAX),
        );
        directory.extend_from_slice(&entry.name);
        offset += 46 + entry.name.len() as u64;
    }
    let directory_size = offset - directory_offset;
    output.write_all(&directory)?;
    let mut end_record = Vec::with_capacity(22);
    write_u32(&mut end_record, 0x0605_4b50);
    write_u16(&mut end_record, 0);
    write_u16(&mut end_record, 0);
    write_u16(
        &mut end_record,
        u16::try_from(central.len()).unwrap_or(u16::MAX),
    );
    write_u16(
        &mut end_record,
        u16::try_from(central.len()).unwrap_or(u16::MAX),
    );
    write_u32(
        &mut end_record,
        u32::try_from(directory_size).unwrap_or(u32::MAX),
    );
    write_u32(
        &mut end_record,
        u32::try_from(directory_offset).unwrap_or(u32::MAX),
    );
    write_u16(&mut end_record, 0);
    output.write_all(&end_record)?;
    output.flush()?;
    output.seek(SeekFrom::End(0))
}

struct CentralEntry {
    name: Vec<u8>,
    crc: u32,
    size: u64,
    header_offset: u64,
}

fn write_u16(target: &mut Vec<u8>, value: u16) {
    target.extend_from_slice(&value.to_le_bytes());
}

fn write_u32(target: &mut Vec<u8>, value: u32) {
    target.extend_from_slice(&value.to_le_bytes());
}

#[cfg(test)]
mod tests {
    use std::sync::Mutex;

    use super::*;

    static ENV_LOCK: Mutex<()> = Mutex::new(());

    fn with_data_dir<T>(run: impl FnOnce(&Path) -> T) -> T {
        let _guard = ENV_LOCK.lock().unwrap_or_else(|error| error.into_inner());
        let dir = tempfile::tempdir().unwrap();
        // SAFETY: serialized by ENV_LOCK; tests never run concurrently here.
        unsafe { std::env::set_var("DBX_PLUGIN_DATA_DIR", dir.path()) };
        run(dir.path())
    }

    #[test]
    fn rejects_traversal_and_sanitizes_windows_characters() {
        assert!(sanitize_relative_path("../../etc", true).is_none());
        assert!(sanitize_relative_path("/abs/path", true).is_some());
        let cleaned = sanitize_name("a:b?c.txt", true).unwrap();
        assert_eq!(cleaned, "a_b_c.txt");
        assert_eq!(sanitize_name("CON", true).unwrap(), "CON_");
        assert_eq!(sanitize_name(".", true), None);
    }

    #[test]
    fn keeps_folder_structure_in_relative_paths() {
        assert_eq!(
            sanitize_relative_path("folder/nested/a.csv", true).unwrap(),
            "folder/nested/a.csv"
        );
        assert_eq!(
            sanitize_relative_path("folder\\nested\\a.csv", true).unwrap(),
            "folder/nested/a.csv"
        );
    }

    #[test]
    fn zip_contains_local_headers_central_directory_and_eocd() {
        let dir = tempfile::tempdir().unwrap();
        let source = dir.path().join("hello.txt");
        fs::write(&source, b"hello").unwrap();
        let destination = dir.path().join("archive.zip");
        let size = write_zip(&[("nested/hello.txt".to_string(), source)], &destination).unwrap();
        assert_eq!(size, fs::metadata(&destination).unwrap().len());
        let data = fs::read(&destination).unwrap();
        assert_eq!(&data[0..4], &0x0403_4b50u32.to_le_bytes());
        assert!(
            data.windows(4)
                .any(|window| window == 0x0201_4b50u32.to_le_bytes())
        );
        assert!(
            data.windows(4)
                .any(|window| window == 0x0605_4b50u32.to_le_bytes())
        );
    }

    #[test]
    fn upload_roundtrip_serves_requested_ranges() {
        with_data_dir(|_| {
            let mut files = ClipboardFiles::default();
            let index = files
                .begin_upload("token-12345678", "hello.txt", None, 5)
                .unwrap();
            assert_eq!(
                files.write_upload_chunk("token-12345678", index, 0, b"hello"),
                Ok(5)
            );
            files.finish_upload("token-12345678", index).unwrap();
            assert_eq!(
                files.read_upload_range(index, 1, 3).unwrap(),
                b"ell".to_vec()
            );
            assert_eq!(files.advertised_files("token-12345678").unwrap().len(), 1);
            files.clear_upload();
        });
    }

    #[test]
    fn single_download_keeps_the_original_name() {
        with_data_dir(|_| {
            let mut files = ClipboardFiles::default();
            let state = files
                .begin_download(
                    vec![
                        FileDescriptor::new("报告.txt")
                            .with_file_size(6)
                            .with_relative_path("docs"),
                    ],
                    None,
                )
                .unwrap();
            fs::write(state.dir.join("0.bin"), b"hello\n").unwrap();
            let (name, path, size) = files.finish_download(&state.token).unwrap();
            assert_eq!(name, "报告.txt");
            assert_eq!(size, 6);
            assert_eq!(fs::read(path).unwrap(), b"hello\n");
        });
    }

    #[test]
    fn local_text_dedupes_and_wakes_paste_waiters() {
        let mut files = ClipboardFiles::default();
        assert!(files.set_local_text("hello".to_string()));
        assert!(!files.set_local_text("hello".to_string()));
        assert!(files.set_local_text("world".to_string()));

        let (tx, rx) = std::sync::mpsc::channel();
        files.paste_waiters.push(tx);
        files.pending_paste = true;
        files.acknowledge_format_list(true);

        assert!(rx.recv_timeout(Duration::from_millis(50)).unwrap());
        assert!(!files.pending_paste);
        assert!(files.advertised);
    }

    #[test]
    fn rejects_invalid_png_header() {
        assert_eq!(png_dimensions(b"not a png"), None);
    }

    fn fake_png(width: u32, height: u32) -> Vec<u8> {
        let mut png = vec![0x89, b'P', b'N', b'G', 0x0D, 0x0A, 0x1A, 0x0A];
        png.extend_from_slice(&13u32.to_be_bytes());
        png.extend_from_slice(b"IHDR");
        png.extend_from_slice(&width.to_be_bytes());
        png.extend_from_slice(&height.to_be_bytes());
        png.extend_from_slice(&[8, 6, 0, 0, 0]);
        png
    }

    #[test]
    fn stages_and_reads_remote_image() {
        with_data_dir(|_| {
            let mut files = ClipboardFiles::default();
            let png = fake_png(2, 3);
            let image = files.store_remote_image(&png).unwrap();
            assert_eq!((image.width, image.height), (2, 3));
            assert_eq!(image.size, png.len() as u64);
            let (data, eof) = files.read_remote_image(&image.token, 0, 1024).unwrap();
            assert_eq!(data, png);
            assert!(eof);
            files.clear_remote_image(&image.token);
            assert!(files.remote_image.is_none());
        });
    }

    #[test]
    fn file_format_list_response_does_not_wake_paste_waiters() {
        let mut files = ClipboardFiles::default();
        let (tx, rx) = std::sync::mpsc::channel();
        files.paste_waiters.push(tx);
        files.acknowledge_format_list(true);

        assert!(rx.recv_timeout(Duration::from_millis(50)).is_err());
        assert_eq!(files.paste_waiters.len(), 1);
    }
    #[test]
    fn multiple_downloads_are_packed_into_a_zip() {
        with_data_dir(|_| {
            let mut files = ClipboardFiles::default();
            let state = files
                .begin_download(
                    vec![
                        FileDescriptor::new("a.txt")
                            .with_file_size(2)
                            .with_relative_path("folder"),
                        FileDescriptor::new("b.txt").with_file_size(0),
                    ],
                    Some(7),
                )
                .unwrap();
            fs::write(state.dir.join("0.bin"), b"hi").unwrap();
            fs::write(state.dir.join("1.bin"), b"").unwrap();
            let (name, path, size) = files.finish_download(&state.token).unwrap();
            assert!(name.starts_with("rdp-files-"));
            assert!(name.ends_with(".zip"));
            assert!(size > 0);
            let data = fs::read(path).unwrap();
            let text = String::from_utf8_lossy(&data);
            assert!(text.contains("folder/a.txt"));
            assert!(text.contains("b.txt"));
            assert!(
                data.windows(4)
                    .any(|window| window == 0x0201_4b50u32.to_le_bytes())
            );
        });
    }
}
