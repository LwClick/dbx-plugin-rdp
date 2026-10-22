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

        let password = connection
            .get("password")
            .and_then(Value::as_str)
            .or_else(|| secrets.get("password").and_then(Value::as_str))
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
    fn pins_ipv6_hosts() {
        assert_eq!(pin_key("10.0.0.5", 3389), "10.0.0.5:3389");
        assert_eq!(pin_key("fe80::1", 3389), "[fe80::1]:3389");
    }
}
