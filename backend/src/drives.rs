//! RDPDR shared-folder redirect.
//!
//! The remote side sees the configured directory as `\\tsclient\DBX RDP`.
//! The default root lives under the plugin data directory so enabling the
//! feature never exposes the user's home directory by accident.

use std::path::{Path, PathBuf};

use ironrdp_rdpdr::RdpdrBackendFactory;

pub const DRIVE_NAME: &str = "DBX RDP";
const DRIVE_DEVICE_ID: u32 = 1;

/// Resolves the directory that will be shared with the remote session.
///
/// An explicit path must be absolute, must not contain `..`, and must already
/// exist as a directory. Empty or missing values fall back to
/// `DBX_PLUGIN_DATA_DIR/shared`.
pub fn shared_folder_root(configured: Option<&str>) -> Result<PathBuf, String> {
    if let Some(raw) = configured.map(str::trim).filter(|value| !value.is_empty()) {
        if raw.split(['/', '\\']).any(|part| part == "..") {
            return Err("Shared folder path must not contain '..'".to_string());
        }
        let path = PathBuf::from(raw);
        if !path.is_absolute() {
            return Err("Shared folder path must be absolute".to_string());
        }
        if !path.is_dir() {
            return Err("Shared folder path is not an existing directory".to_string());
        }
        return Ok(path);
    }

    let root = std::env::var_os("DBX_PLUGIN_DATA_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(std::env::temp_dir)
        .join("shared");
    std::fs::create_dir_all(&root)
        .map_err(|error| format!("Cannot create the shared folder: {error}"))?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        let _ = std::fs::set_permissions(&root, std::fs::Permissions::from_mode(0o700));
    }
    Ok(root)
}

/// Builds the platform-specific RDPDR backend factory for one shared folder.
pub fn build_factory(root: &Path) -> Result<Box<dyn RdpdrBackendFactory + Send>, String> {
    #[cfg(any(target_os = "macos", target_os = "linux"))]
    {
        use ironrdp_rdpdr_native::backend::NixRdpdrBackendFactory;
        Ok(Box::new(
            NixRdpdrBackendFactory::new(root.to_string_lossy().into_owned())
                .with_drive(DRIVE_DEVICE_ID, DRIVE_NAME.to_string()),
        ))
    }

    #[cfg(windows)]
    {
        use ironrdp_rdpdr_native::{RedirectedDrive, WindowsRdpdrBackendFactory};
        let drive = RedirectedDrive::new(DRIVE_DEVICE_ID, DRIVE_NAME, root, false)
            .map_err(|error| format!("Cannot build the shared drive: {error}"))?;
        Ok(Box::new(WindowsRdpdrBackendFactory::new(drive)))
    }

    #[cfg(not(any(target_os = "macos", target_os = "linux", windows)))]
    {
        let _ = root;
        Err("Drive redirection is not supported on this platform".to_string())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rejects_relative_and_parent_paths() {
        assert!(shared_folder_root(Some("relative/path")).is_err());
        assert!(shared_folder_root(Some("/tmp/../etc")).is_err());
    }

    #[test]
    fn rejects_missing_directories() {
        let missing = std::env::temp_dir().join("dbx-rdp-missing-share");
        let _ = std::fs::remove_dir_all(&missing);
        assert!(shared_folder_root(Some(&missing.to_string_lossy())).is_err());
    }

    #[test]
    fn accepts_an_existing_absolute_directory() {
        let directory = tempfile::tempdir().unwrap();
        let root = shared_folder_root(Some(&directory.path().to_string_lossy())).unwrap();
        assert_eq!(root, directory.path());
        assert!(build_factory(&root).is_ok());
    }
}
