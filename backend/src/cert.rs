use std::collections::BTreeMap;
use std::fs;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use dbx_plugin_sdk::{UserInputOption, UserInputPrompt, host_client};
use ironrdp_tls::CertificateValidationCallback;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use x509_cert::der::Decode;

use crate::credentials::CertificatePolicy;

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct CertificateInfo {
    #[serde(rename = "sha256")]
    pub fingerprint_sha256: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub subject: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub issuer: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub not_after: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PinEntry {
    sha256: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    subject: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    issuer: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    accepted_at: String,
}

#[derive(Debug, Default, Serialize, Deserialize)]
struct PinFile {
    version: u32,
    entries: BTreeMap<String, PinEntry>,
}

pub struct PinStore {
    path: PathBuf,
    lock: Mutex<()>,
}

impl PinStore {
    pub fn new(path: PathBuf) -> Self {
        Self {
            path,
            lock: Mutex::new(()),
        }
    }

    pub fn from_env() -> Arc<Self> {
        let dir = std::env::var_os("DBX_PLUGIN_DATA_DIR")
            .map(PathBuf::from)
            .unwrap_or_else(|| std::env::temp_dir().join("dbx-rdp-plugin"));
        let _ = fs::create_dir_all(&dir);
        Arc::new(Self::new(dir.join("known_hosts.json")))
    }

    pub fn check(&self, key: &str, fingerprint: &str) -> PinCheck {
        let _guard = self.lock.lock().ok();
        let file = self.read();
        match file.entries.get(key) {
            None => PinCheck::Unknown,
            Some(entry) if entry.sha256.eq_ignore_ascii_case(fingerprint) => PinCheck::Match,
            Some(entry) => PinCheck::Mismatch(entry.clone()),
        }
    }

    pub fn remember(&self, key: &str, info: &CertificateInfo) -> Result<(), String> {
        let _guard = self
            .lock
            .lock()
            .map_err(|_| "pin store lock is poisoned".to_string())?;
        let mut file = self.read();
        file.version = 1;
        file.entries.insert(
            key.to_string(),
            PinEntry {
                sha256: info.fingerprint_sha256.clone(),
                subject: info.subject.clone(),
                issuer: info.issuer.clone(),
                accepted_at: unix_timestamp(),
            },
        );
        self.write(&file)
    }

    pub fn forget(&self, key: &str) -> Result<(), String> {
        let _guard = self
            .lock
            .lock()
            .map_err(|_| "pin store lock is poisoned".to_string())?;
        let mut file = self.read();
        if file.entries.remove(key).is_some() {
            self.write(&file)?;
        }
        Ok(())
    }

    fn read(&self) -> PinFile {
        match fs::read_to_string(&self.path) {
            Ok(json) => serde_json::from_str(&json).unwrap_or_else(|error| {
                tracing::warn!(path = %self.path.display(), %error, "ignoring unreadable certificate pin store");
                PinFile::default()
            }),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => PinFile::default(),
            Err(error) => {
                tracing::warn!(path = %self.path.display(), %error, "cannot read certificate pin store");
                PinFile::default()
            }
        }
    }

    fn write(&self, file: &PinFile) -> Result<(), String> {
        if let Some(parent) = self.path.parent() {
            fs::create_dir_all(parent).map_err(|error| format!("create pin directory: {error}"))?;
        }
        let json = serde_json::to_string_pretty(file).map_err(|error| error.to_string())?;
        let temp = self.path.with_extension("json.tmp");
        fs::write(&temp, json).map_err(|error| format!("write pin store: {error}"))?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt as _;
            fs::set_permissions(&temp, fs::Permissions::from_mode(0o600))
                .map_err(|error| format!("chmod pin store: {error}"))?;
        }
        fs::rename(&temp, &self.path).map_err(|error| format!("replace pin store: {error}"))
    }
}

#[derive(Debug, Clone)]
pub enum PinCheck {
    Match,
    Unknown,
    Mismatch(PinEntry),
}

pub struct CertPrompt {
    host: String,
    port: u16,
    policy: CertificatePolicy,
    store: Arc<PinStore>,
    info: Arc<Mutex<Option<CertificateInfo>>>,
}

impl CertPrompt {
    pub fn new(
        host: String,
        port: u16,
        policy: CertificatePolicy,
        store: Arc<PinStore>,
        info: Arc<Mutex<Option<CertificateInfo>>>,
    ) -> Arc<Self> {
        Arc::new(Self {
            host,
            port,
            policy,
            store,
            info,
        })
    }

    pub fn callback(self: &Arc<Self>) -> CertificateValidationCallback {
        let prompt = Arc::clone(self);
        Arc::new(move |der: &[u8], endpoint: &str, error: &str| {
            prompt.accept_invalid(der, endpoint, error)
        })
    }

    fn accept_invalid(&self, der: &[u8], endpoint: &str, error: &str) -> bool {
        let info = inspect_certificate(der);
        if let Ok(mut slot) = self.info.lock() {
            *slot = Some(info.clone());
        }
        let key = crate::credentials::pin_key(&self.host, self.port);
        let fingerprint = info.fingerprint_sha256.clone();

        match (self.policy, self.store.check(&key, &fingerprint)) {
            (CertificatePolicy::Strict, _) => {
                tracing::warn!(host = %self.host, %endpoint, "rejecting untrusted certificate (strict policy)");
                false
            }
            (_, PinCheck::Match) => {
                tracing::info!(host = %self.host, "accepted pinned certificate");
                true
            }
            (_, PinCheck::Mismatch(entry)) => {
                tracing::warn!(
                    host = %self.host,
                    previous = %entry.sha256,
                    current = %fingerprint,
                    "server certificate changed; refusing to connect"
                );
                false
            }
            (CertificatePolicy::Prompt, PinCheck::Unknown) => {
                if !self.ask_user(&info, endpoint, error) {
                    return false;
                }
                if let Err(write_error) = self.store.remember(&key, &info) {
                    tracing::warn!(%write_error, "cannot persist certificate pin");
                    return false;
                }
                true
            }
        }
    }

    fn ask_user(&self, info: &CertificateInfo, endpoint: &str, error: &str) -> bool {
        let Some(client) = host_client() else {
            tracing::warn!("no DBX UI available for the certificate prompt");
            return false;
        };
        let mut prompt = format!(
            "RDP 服务器证书未受信任 / Untrusted RDP server certificate\n\nHost: {}:{}\nSHA-256: {}\nSubject: {}\nIssuer: {}\nValid until: {}\nEndpoint: {}\nReason: {}",
            self.host,
            self.port,
            info.fingerprint_sha256,
            empty_dash(&info.subject),
            empty_dash(&info.issuer),
            empty_dash(&info.not_after),
            endpoint,
            truncate(error, 300)
        );
        if prompt.len() > 1900 {
            prompt.truncate(1900);
            prompt.push('…');
        }
        let question = UserInputPrompt::choice(
            prompt,
            vec![
                UserInputOption {
                    value: "accept".to_string(),
                    label: "信任并继续 / Trust and continue".to_string(),
                },
                UserInputOption {
                    value: "reject".to_string(),
                    label: "取消 / Cancel".to_string(),
                },
            ],
        )
        .with_title("RDP certificate / RDP 证书")
        .with_timeout_secs(300);
        match client.request_user_input(&question) {
            Ok(answer) => answer.submitted() == Some("accept"),
            Err(error) => {
                tracing::warn!(error = %error.message, "certificate prompt failed");
                false
            }
        }
    }
}

pub fn inspect_certificate(der: &[u8]) -> CertificateInfo {
    let fingerprint = format_fingerprint(&Sha256::digest(der));
    let Ok(cert) = x509_cert::Certificate::from_der(der) else {
        return CertificateInfo {
            fingerprint_sha256: fingerprint,
            ..CertificateInfo::default()
        };
    };
    let tbs = cert.tbs_certificate();
    let validity = tbs.validity();
    CertificateInfo {
        fingerprint_sha256: fingerprint,
        subject: tbs.subject().to_string(),
        issuer: tbs.issuer().to_string(),
        not_after: validity.not_after.to_string(),
    }
}

pub fn format_fingerprint(bytes: &[u8]) -> String {
    let mut out = String::with_capacity(bytes.len() * 3);
    for (index, byte) in bytes.iter().enumerate() {
        if index > 0 {
            out.push(':');
        }
        use core::fmt::Write as _;
        let _ = write!(out, "{byte:02X}");
    }
    out
}

fn empty_dash(value: &str) -> &str {
    if value.is_empty() { "-" } else { value }
}

fn truncate(value: &str, max: usize) -> String {
    if value.chars().count() <= max {
        return value.to_string();
    }
    let mut out: String = value.chars().take(max).collect();
    out.push('…');
    out
}

fn unix_timestamp() -> String {
    let seconds = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|duration| duration.as_secs())
        .unwrap_or_default();
    seconds.to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fingerprint_is_colon_separated_uppercase() {
        assert_eq!(format_fingerprint(&[0x00, 0xAB, 0xFF]), "00:AB:FF");
    }

    #[test]
    fn pin_store_round_trip_and_mismatch() {
        let dir = tempfile::tempdir().unwrap();
        let store = PinStore::new(dir.path().join("known_hosts.json"));
        let info = CertificateInfo {
            fingerprint_sha256: "AA:BB".to_string(),
            subject: "CN=rdp".to_string(),
            issuer: "CN=rdp".to_string(),
            not_after: "never".to_string(),
        };
        assert!(matches!(
            store.check("host:3389", "AA:BB"),
            PinCheck::Unknown
        ));
        store.remember("host:3389", &info).unwrap();
        assert!(matches!(store.check("host:3389", "AA:BB"), PinCheck::Match));
        assert!(matches!(
            store.check("host:3389", "CC:DD"),
            PinCheck::Mismatch(_)
        ));
        store.forget("host:3389").unwrap();
        assert!(matches!(
            store.check("host:3389", "AA:BB"),
            PinCheck::Unknown
        ));
    }

    #[cfg(unix)]
    #[test]
    fn pin_store_is_private() {
        use std::os::unix::fs::PermissionsExt as _;
        let dir = tempfile::tempdir().unwrap();
        let store = PinStore::new(dir.path().join("known_hosts.json"));
        store
            .remember(
                "h:1",
                &CertificateInfo {
                    fingerprint_sha256: "AA".to_string(),
                    ..Default::default()
                },
            )
            .unwrap();
        let mode = fs::metadata(&store.path).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o600);
    }
}
