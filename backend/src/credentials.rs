use std::time::Duration;

use dbx_plugin_sdk::{PluginError, UserInputPrompt, host_client};
use serde_json::Value;
use zeroize::Zeroizing;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SecurityMode {
    Nla,
    Tls,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CertificatePolicy {
    Prompt,
    Strict,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PerformanceProfile {
    Balanced,
    Quality,
    Speed,
}

#[derive(Debug)]
pub struct RdpConnectOptions {
    pub connection_id: String,
    pub name: String,
    /// Logical host/port from the saved connection. Used for certificate
    /// pinning and UI labels; not necessarily the dialed endpoint.
    pub connection_host: String,
    pub connection_port: u16,
    /// Final endpoint after DBX transport layers.
    pub host: String,
    pub port: u16,
    pub username: String,
    pub domain: Option<String>,
    pub password: Zeroizing<String>,
    pub ask_password: bool,
    pub security_mode: SecurityMode,
    pub certificate_policy: CertificatePolicy,
    pub clipboard_sync: bool,
    pub performance_profile: PerformanceProfile,
    pub connect_timeout: Duration,
    /// Initial RDP desktop size. In fixed mode this is also the final size.
    pub desktop_width: u16,
    pub desktop_height: u16,
    /// `true` only for the "fit" mode; the workbench may then request resizes.
    pub auto_resize: bool,
    /// Disable RDP lossy bitmap compression.
    pub lossless: bool,
    /// Redirect a local directory as `\\tsclient\DBX RDP`.
    pub share_folder: bool,
    /// Custom shared directory; `None` uses the plugin data directory.
    pub shared_folder: Option<String>,
    /// Play remote audio through the sidecar's system audio output.
    pub audio_playback: bool,
    /// Map macOS Command shortcuts to Windows Ctrl equivalents.
    pub remap_shortcuts: bool,
    /// Windows keyboard layout HKL low word, when the user picked one.
    pub keyboard_layout: Option<u32>,
}

impl RdpConnectOptions {
    pub fn from_lifecycle(params: &Value) -> Result<Self, PluginError> {
        let connection = params
            .get("connection")
            .ok_or_else(|| PluginError::new(-32602, "Missing connection payload"))?;
        let connection_id = connection
            .get("id")
            .and_then(Value::as_str)
            .filter(|id| !id.trim().is_empty())
            .ok_or_else(|| PluginError::new(-32602, "Missing connection id"))?
            .to_string();
        let name = connection
            .get("name")
            .and_then(Value::as_str)
            .filter(|name| !name.trim().is_empty())
            .unwrap_or("Windows RDP")
            .to_string();
        let external = connection.get("external_config").unwrap_or(&Value::Null);
        let secrets = connection.get("connection_secrets").unwrap_or(&Value::Null);
        let runtime = params.get("runtime").unwrap_or(&Value::Null);

        let host = runtime
            .get("host")
            .and_then(Value::as_str)
            .filter(|host| !host.trim().is_empty())
            .or_else(|| connection.get("host").and_then(Value::as_str))
            .ok_or_else(|| PluginError::new(-32602, "Missing host"))?
            .trim()
            .to_string();
        let connection_host = connection
            .get("host")
            .and_then(Value::as_str)
            .filter(|host| !host.trim().is_empty())
            .map(|host| host.trim().to_string())
            .unwrap_or_else(|| host.clone());
        let connection_port = connection
            .get("port")
            .and_then(Value::as_u64)
            .unwrap_or(3389)
            .try_into()
            .map_err(|_| PluginError::new(-32602, "Invalid connection port"))?;
        let port = runtime
            .get("port")
            .and_then(Value::as_u64)
            .or_else(|| connection.get("port").and_then(Value::as_u64))
            .unwrap_or(3389)
            .try_into()
            .map_err(|_| PluginError::new(-32602, "Invalid port"))?;
        let raw_username = connection
            .get("username")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .trim()
            .to_string();
        let explicit_domain = external
            .get("domain")
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|domain| !domain.is_empty());
        let (username, domain) = split_username(&raw_username, explicit_domain);

        // DBX serializes `password` as an empty string for plugin connections
        // and puts the saved secret in `connection_secrets`. Treat empty and
        // masked values as absent; otherwise `Some("")` would short-circuit
        // the secret fallback and force a password prompt on every connect.
        let password = connection
            .get("password")
            .and_then(Value::as_str)
            .filter(|value| !is_absent_password(value))
            .or_else(|| {
                secrets
                    .get("password")
                    .and_then(Value::as_str)
                    .filter(|value| !value.is_empty())
            })
            .unwrap_or_default()
            .to_string();

        let ask_password = external
            .get("ask_password")
            .and_then(Value::as_bool)
            .unwrap_or(false);
        let security_mode = match external.get("security_mode").and_then(Value::as_str) {
            Some("tls") => SecurityMode::Tls,
            _ => SecurityMode::Nla,
        };
        let certificate_policy = match external.get("certificate_policy").and_then(Value::as_str) {
            Some("strict") => CertificatePolicy::Strict,
            _ => CertificatePolicy::Prompt,
        };
        let clipboard_sync = external
            .get("clipboard_sync")
            .and_then(Value::as_bool)
            .unwrap_or(true);
        let performance_profile = match external.get("performance_profile").and_then(Value::as_str)
        {
            Some("quality") => PerformanceProfile::Quality,
            Some("speed") => PerformanceProfile::Speed,
            _ => PerformanceProfile::Balanced,
        };
        let timeout_secs = external
            .get("connect_timeout_secs")
            .and_then(Value::as_u64)
            .unwrap_or(20)
            .clamp(5, 120);
        let connect_timeout = Duration::from_secs(timeout_secs);
        let (desktop_width, desktop_height, auto_resize) =
            parse_resolution(external.get("resolution").and_then(Value::as_str));
        let lossless = external.get("image_quality").and_then(Value::as_str) != Some("balanced");
        let share_folder = external
            .get("share_local_folder")
            .and_then(Value::as_bool)
            .unwrap_or(false);
        let shared_folder = external
            .get("shared_folder_path")
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .map(str::to_string);
        let audio_playback = external
            .get("audio_playback")
            .and_then(Value::as_bool)
            .unwrap_or(true);
        let remap_shortcuts = external
            .get("remap_macos_shortcuts")
            .and_then(Value::as_bool)
            .unwrap_or(true);
        let keyboard_layout =
            parse_keyboard_layout(external.get("keyboard_layout").and_then(Value::as_str));

        Ok(Self {
            connection_id,
            name,
            connection_host,
            connection_port,
            host,
            port,
            username,
            domain,
            password: Zeroizing::new(password),
            ask_password,
            security_mode,
            certificate_policy,
            clipboard_sync,
            performance_profile,
            connect_timeout,
            desktop_width,
            desktop_height,
            auto_resize,
            lossless,
            share_folder,
            shared_folder,
            audio_playback,
            remap_shortcuts,
            keyboard_layout,
        })
    }

    /// Prompts through the DBX UI when the saved password must not be used.
    /// Cancellation and timeouts fail closed.
    pub fn ensure_password(&mut self) -> Result<(), PluginError> {
        if !self.ask_password && !self.password.is_empty() {
            return Ok(());
        }
        let client = host_client().ok_or_else(|| {
            PluginError::new(-32001, "DBX UI is unavailable for the password prompt")
        })?;
        let prompt = UserInputPrompt::secret(format!(
            "Password for {}@{} / 该账号的密码",
            self.username, self.connection_host
        ))
        .with_title(format!("RDP · {}", self.name))
        .with_timeout_secs(300);
        let answer = client.request_user_input(&prompt)?;
        let Some(password) = answer.submitted().map(str::to_owned) else {
            return Err(PluginError::new(-32013, "Password prompt was cancelled"));
        };
        self.password = Zeroizing::new(password);
        Ok(())
    }

    pub fn pin_key(&self) -> String {
        pin_key(&self.connection_host, self.connection_port)
    }
}

pub fn pin_key(host: &str, port: u16) -> String {
    if host.contains(':') && !host.starts_with('[') {
        format!("[{host}]:{port}")
    } else {
        format!("{host}:{port}")
    }
}

fn is_absent_password(value: &str) -> bool {
    value.is_empty() || value.starts_with("__DBX_SAVED_CREDENTIAL__:")
}

/// Maps the manifest layout id to the Windows HKL low word. `auto`/unknown
/// values leave the server default untouched.
fn parse_keyboard_layout(value: Option<&str>) -> Option<u32> {
    match value {
        Some("en-US") => Some(0x0409),
        Some("zh-CN") => Some(0x0804),
        Some("ja-JP") => Some(0x0411),
        Some("de-DE") => Some(0x0407),
        Some("fr-FR") => Some(0x040c),
        Some("es-ES") => Some(0x0c0a),
        Some("ru-RU") => Some(0x0419),
        Some("pt-BR") => Some(0x0416),
        _ => None,
    }
}

/// Resolves the connection-level desktop size. Fixed presets keep a standard
/// Windows layout; "fit" lets the workbench drive Display Control resizes.
fn parse_resolution(value: Option<&str>) -> (u16, u16, bool) {
    match value {
        // "retina" was removed; keep saved connections dynamic instead of
        // silently switching them to a fixed 1080p desktop.
        Some("fit") | Some("retina") => (1280, 800, true),
        Some("1280x720") => (1280, 720, false),
        Some("2560x1440") => (2560, 1440, false),
        Some("3840x2160") => (3840, 2160, false),
        _ => (1920, 1080, false),
    }
}

fn split_username(username: &str, explicit_domain: Option<&str>) -> (String, Option<String>) {
    if let Some((domain, user)) = username.split_once('\\') {
        let domain = domain.trim();
        let user = user.trim();
        if !user.is_empty() {
            let resolved = if domain.is_empty() {
                explicit_domain
                    .map(str::trim)
                    .filter(|value| !value.is_empty())
            } else {
                Some(domain)
            };
            return (user.to_string(), resolved.map(str::to_string));
        }
    }
    let domain = explicit_domain
        .map(str::trim)
        .filter(|domain| !domain.is_empty())
        .map(str::to_string);
    (username.trim().to_string(), domain)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn payload() -> Value {
        json!({
            "connection": {
                "id": "conn-1",
                "name": "Office PC",
                "host": "10.0.0.5",
                "port": 3389,
                "username": "ACME\\alice",
                "password": "secret",
                "external_config": {
                    "security_mode": "nla",
                    "certificate_policy": "prompt",
                    "clipboard_sync": true,
                    "connect_timeout_secs": 30
                },
                "connection_secrets": {}
            },
            "runtime": { "host": "127.0.0.1", "port": 49152 }
        })
    }

    #[test]
    fn parses_domain_from_username() {
        let options = RdpConnectOptions::from_lifecycle(&payload()).unwrap();
        assert_eq!(options.username, "alice");
        assert_eq!(options.domain.as_deref(), Some("ACME"));
        assert_eq!(options.host, "127.0.0.1");
        assert_eq!(options.port, 49152);
        assert_eq!(options.connect_timeout, Duration::from_secs(30));
    }

    #[test]
    fn reads_secret_when_password_field_is_empty() {
        let mut value = payload();
        value["connection"]["password"] = json!("");
        value["connection"]["connection_secrets"] = json!({ "password": "saved-secret" });
        let options = RdpConnectOptions::from_lifecycle(&value).unwrap();
        assert_eq!(options.password.as_str(), "saved-secret");
    }

    #[test]
    fn ignores_masked_saved_credential() {
        let mut value = payload();
        value["connection"]["password"] = json!("__DBX_SAVED_CREDENTIAL__:masked");
        value["connection"]["connection_secrets"] = json!({ "password": "saved-secret" });
        let options = RdpConnectOptions::from_lifecycle(&value).unwrap();
        assert_eq!(options.password.as_str(), "saved-secret");
    }

    #[test]
    fn keeps_upn_and_plain_names() {
        assert_eq!(
            split_username("alice@acme.test", None),
            ("alice@acme.test".to_string(), None)
        );
        assert_eq!(
            split_username("alice", Some("ACME")),
            ("alice".to_string(), Some("ACME".to_string()))
        );
    }

    #[test]
    fn defaults_to_fixed_1080p() {
        let options = RdpConnectOptions::from_lifecycle(&payload()).unwrap();
        assert_eq!(
            (options.desktop_width, options.desktop_height),
            (1920, 1080)
        );
        assert!(!options.auto_resize);
        assert!(options.lossless);
    }

    #[test]
    fn parses_fit_and_fixed_resolutions() {
        for (value, width, height, auto_resize) in [
            ("fit", 1280, 800, true),
            ("retina", 1280, 800, true),
            ("1280x720", 1280, 720, false),
            ("2560x1440", 2560, 1440, false),
            ("3840x2160", 3840, 2160, false),
            ("1920x1080", 1920, 1080, false),
        ] {
            let mut input = payload();
            input["connection"]["external_config"]["resolution"] = json!(value);
            let options = RdpConnectOptions::from_lifecycle(&input).unwrap();
            assert_eq!(
                (options.desktop_width, options.desktop_height),
                (width, height),
                "resolution {value}"
            );
            assert_eq!(options.auto_resize, auto_resize, "resolution {value}");
        }
    }

    #[test]
    fn parses_image_quality() {
        let mut input = payload();
        input["connection"]["external_config"]["image_quality"] = json!("balanced");
        let options = RdpConnectOptions::from_lifecycle(&input).unwrap();
        assert!(!options.lossless);

        input["connection"]["external_config"]["image_quality"] = json!("lossless");
        let options = RdpConnectOptions::from_lifecycle(&input).unwrap();
        assert!(options.lossless);
    }

    #[test]
    fn parses_keyboard_layout_and_shortcut_remap() {
        let options = RdpConnectOptions::from_lifecycle(&payload()).unwrap();
        assert!(options.remap_shortcuts);
        assert_eq!(options.keyboard_layout, None);

        for (value, expected) in [
            ("en-US", 0x0409),
            ("zh-CN", 0x0804),
            ("fr-FR", 0x040c),
            ("pt-BR", 0x0416),
            ("auto", 0),
            ("bogus", 0),
        ] {
            let mut input = payload();
            input["connection"]["external_config"]["keyboard_layout"] = json!(value);
            let options = RdpConnectOptions::from_lifecycle(&input).unwrap();
            assert_eq!(
                options.keyboard_layout.unwrap_or(0),
                expected,
                "layout {value}"
            );
        }

        let mut input = payload();
        input["connection"]["external_config"]["remap_macos_shortcuts"] = json!(false);
        assert!(
            !RdpConnectOptions::from_lifecycle(&input)
                .unwrap()
                .remap_shortcuts
        );
    }

    #[test]
    fn unknown_resolution_falls_back_to_1080p() {
        let mut input = payload();
        input["connection"]["external_config"]["resolution"] = json!("bogus");
        let options = RdpConnectOptions::from_lifecycle(&input).unwrap();
        assert_eq!(
            (options.desktop_width, options.desktop_height),
            (1920, 1080)
        );
        assert!(!options.auto_resize);
    }

    #[test]
    fn pins_ipv6_hosts() {
        assert_eq!(pin_key("10.0.0.5", 3389), "10.0.0.5:3389");
        assert_eq!(pin_key("fe80::1", 3389), "[fe80::1]:3389");
    }
}
