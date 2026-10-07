/*
 * SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
 * SPDX-License-Identifier: Apache-2.0
 *
 * Licensed under the Apache License, Version 2.0 (the "License");
 * you may not use this file except in compliance with the License.
 * You may obtain a copy of the License at
 *
 * http://www.apache.org/licenses/LICENSE-2.0
 *
 * Unless required by applicable law or agreed to in writing, software
 * distributed under the License is distributed on an "AS IS" BASIS,
 * WITHOUT WARRANTIES OR CONDITIONS OF ANY KIND, either express or implied.
 * See the License for the specific language governing permissions and
 * limitations under the License.
 */
use std::io::BufReader;
use std::path::{Path, PathBuf};
use std::str::FromStr;
use std::{env, fs};

use serde::Deserialize;
use tonic::transport::Uri;

use crate::default as tls_default;
pub const CONFIG_FILE_LOCATION: &str = ".config/nico_api_cli.json";
/// Previous name for [`CONFIG_FILE_LOCATION`]. Still read as a fallback (with a
/// deprecation warning) so existing setups keep working after the rename.
pub const LEGACY_CONFIG_FILE_LOCATION: &str = ".config/carbide_api_cli.json";

#[derive(thiserror::Error, Debug)]
pub enum ClientConfigError {
    #[error("unable to parse url: {0}")]
    UrlParseError(String),
}

#[derive(Clone, Debug)]
pub struct ClientCert {
    pub cert_path: String,
    pub key_path: String,
}

#[derive(Debug, Deserialize)]
pub struct FileConfig {
    pub api_url: Option<String>,
    pub root_ca_path: Option<String>,
    pub client_key_path: Option<String>,
    pub client_cert_path: Option<String>,
    pub rms_root_ca_path: Option<String>,
}

pub fn get_api_url(api_url: Option<String>, file_config: Option<&FileConfig>) -> String {
    // First from command line, second env var.
    if let Some(api) = api_url {
        return ensure_url_has_scheme(&api);
    }

    // Third config file
    if let Some(file_config) = file_config
        && let Some(api_url) = file_config.api_url.as_ref()
    {
        return ensure_url_has_scheme(api_url);
    }

    // TODO configurable default api_url
    // Otherwise we assume the admin-cli is called from inside a kubernetes pod
    "https://carbide-api.forge-system.svc.cluster.local:1079".to_string()
}

/// Ensures a URL has an HTTP(S) scheme, defaulting to `https://` if missing
fn ensure_url_has_scheme(url: &str) -> String {
    let url = url.trim();
    let lower = url.to_ascii_lowercase();
    if lower.starts_with("http://") || lower.starts_with("https://") {
        url.to_string()
    } else {
        format!("https://{url}")
    }
}

pub fn get_client_cert_info(
    client_cert_path: Option<String>,
    client_key_path: Option<String>,
    file_config: Option<&FileConfig>,
) -> ClientCert {
    // First from command line, second env var.
    if let (Some(client_key_path), Some(client_cert_path)) = (client_key_path, client_cert_path) {
        return ClientCert {
            cert_path: client_cert_path,
            key_path: client_key_path,
        };
    }

    // Third config file
    if let Some(file_config) = file_config
        && let (Some(client_key_path), Some(client_cert_path)) = (
            file_config.client_key_path.as_ref(),
            file_config.client_cert_path.as_ref(),
        )
    {
        return ClientCert {
            cert_path: client_cert_path.clone(),
            key_path: client_key_path.clone(),
        };
    }

    // this is the location for most k8s pods
    if Path::new("/var/run/secrets/spiffe.io/tls.crt").exists()
        && Path::new("/var/run/secrets/spiffe.io/tls.key").exists()
    {
        return ClientCert {
            cert_path: "/var/run/secrets/spiffe.io/tls.crt".to_string(),
            key_path: "/var/run/secrets/spiffe.io/tls.key".to_string(),
        };
    }

    // this is the location for most compiled clients executing on x86 hosts or DPUs
    if Path::new(tls_default::CLIENT_CERT).exists() && Path::new(tls_default::CLIENT_KEY).exists() {
        return ClientCert {
            cert_path: tls_default::CLIENT_CERT.to_string(),
            key_path: tls_default::CLIENT_KEY.to_string(),
        };
    }

    // and this is the location for developers executing from within infra-controller's repo
    if let Ok(project_root) = env::var("REPO_ROOT") {
        //TODO: actually fix this cert and give it one that's valid for like 10 years.
        let cert_path = format!("{project_root}/dev/certs/server_identity.pem");
        let key_path = format!("{project_root}/dev/certs/server_identity.key");
        if Path::new(cert_path.as_str()).exists() && Path::new(key_path.as_str()).exists() {
            return ClientCert {
                cert_path,
                key_path,
            };
        }
    }

    // if you make it here, you'll just have to tell me where the client cert is.
    panic!(
        r###"Unknown client cert location. Set (will be read in same sequence.)
           1. --client-cert-path and --client-key-path flag or
           2. environment variables CLIENT_KEY_PATH and CLIENT_CERT_PATH or
           3. add client_key_path and client_cert_path in $HOME/{CONFIG_FILE_LOCATION}.
           4. a file existing at "/var/run/secrets/spiffe.io/tls.crt" and "/var/run/secrets/spiffe.io/tls.key".
           5. a file existing at "{}" and "{}"."###,
        tls_default::CLIENT_CERT,
        tls_default::CLIENT_KEY
    )
}

pub fn get_root_ca_path(root_ca_path: Option<String>, file_config: Option<&FileConfig>) -> String {
    // First from command line, second env var.
    if let Some(root_ca_path) = root_ca_path {
        return root_ca_path;
    }

    // Third config file
    if let Some(file_config) = file_config
        && let Some(root_ca_path) = file_config.root_ca_path.as_ref()
    {
        return root_ca_path.clone();
    }

    // this is the location for most k8s pods
    if Path::new("/var/run/secrets/spiffe.io/ca.crt").exists() {
        return "/var/run/secrets/spiffe.io/ca.crt".to_string();
    }

    // this is the location for most compiled clients executing on x86 hosts or DPUs
    if Path::new(tls_default::ROOT_CA).exists() {
        return tls_default::ROOT_CA.to_string();
    }

    // and this is the location for developers executing from within infra-controller's repo
    if let Ok(project_root) = env::var("REPO_ROOT") {
        let path = format!("{project_root}/dev/certs/localhost/ca.crt");
        if Path::new(path.as_str()).exists() {
            return path;
        }
    }

    // if you make it here, you'll just have to tell me where the root CA is.
    panic!(
        r###"Unknown ROOT_CA_PATH. Set (will be read in same sequence.)
           1. --root-ca-path flag or
           2. environment variable ROOT_CA_PATH or
           3. add root_ca_path in $HOME/{CONFIG_FILE_LOCATION}.
           4. a file existing at "/var/run/secrets/spiffe.io/ca.crt".
           5. a file existing at "{}"."###,
        tls_default::ROOT_CA
    )
}

fn get_config_file_location() -> Result<Option<PathBuf>, ClientConfigError> {
    let Ok(home) = env::var("HOME") else {
        return Ok(None);
    };
    let home = Path::new(&home);

    let config_path = home.join(CONFIG_FILE_LOCATION);
    if config_path.is_file() {
        return Ok(Some(config_path));
    }

    let legacy_path = home.join(LEGACY_CONFIG_FILE_LOCATION);
    if legacy_path.is_file() {
        eprintln!(
            "warning: config file `$HOME/{LEGACY_CONFIG_FILE_LOCATION}` is deprecated; \
             rename it to `$HOME/{CONFIG_FILE_LOCATION}`."
        );
        return Ok(Some(legacy_path));
    }

    Ok(None)
}
pub fn get_config_from_file() -> Option<FileConfig> {
    // Third config file
    let config_file_path = get_config_file_location().ok()?;

    if let Some(cfg_file) = config_file_path {
        let file = fs::File::open(cfg_file).unwrap();
        let reader = BufReader::new(file);
        let file_config: FileConfig = serde_json::from_reader(reader).unwrap();
        return Some(file_config);
    }
    None
}

pub fn get_proxy_info() -> Result<Option<String>, ClientConfigError> {
    std::env::var("http_proxy")
        .ok()
        .or_else(|| std::env::var("https_proxy").ok())
        .or_else(|| std::env::var("HTTP_PROXY").ok())
        .or_else(|| std::env::var("HTTPS_PROXY").ok())
        .map_or(Ok(None), |proxy| {
            let uri = Uri::from_str(&proxy).map_err(|_| ClientConfigError::UrlParseError(proxy))?;
            if uri
                .scheme_str()
                .is_some_and(|s| !s.eq_ignore_ascii_case("socks5"))
            {
                return Err(ClientConfigError::UrlParseError(
                    "Only SOCKS5 Proxy supported".to_owned(),
                ));
            }
            let host = uri.host().map_or("".to_owned(), |h| h.to_owned());
            let port = uri
                .port_u16()
                .map_or("".to_owned(), |port| port.to_string());
            if host.is_empty() {
                Ok(None)
            } else {
                Ok(Some(host + ":" + &port))
            }
        })
}

#[cfg(test)]
mod tests {
    use std::sync::Mutex;
    use std::time::{SystemTime, UNIX_EPOCH};

    use carbide_test_support::Outcome::*;
    use carbide_test_support::{scenarios, value_scenarios};

    use super::*;

    static ENV_LOCK: Mutex<()> = Mutex::new(());
    const PROXY_ENV_KEYS: &[&str] = &["http_proxy", "https_proxy", "HTTP_PROXY", "HTTPS_PROXY"];

    #[derive(Debug, PartialEq)]
    struct ClientCertSummary {
        cert_path: String,
        key_path: String,
    }

    #[derive(Debug, PartialEq)]
    struct FileConfigSummary {
        api_url: Option<String>,
        root_ca_path: Option<String>,
        client_key_path: Option<String>,
        client_cert_path: Option<String>,
        rms_root_ca_path: Option<String>,
    }

    #[derive(Debug)]
    struct ProxyInput {
        vars: &'static [(&'static str, &'static str)],
    }

    struct EnvSnapshot {
        values: Vec<(&'static str, Option<String>)>,
    }

    impl EnvSnapshot {
        fn capture(keys: &[&'static str]) -> Self {
            Self {
                values: keys.iter().map(|key| (*key, env::var(key).ok())).collect(),
            }
        }
    }

    impl Drop for EnvSnapshot {
        fn drop(&mut self) {
            for (key, value) in &self.values {
                match value {
                    Some(value) => set_env(key, value),
                    None => remove_env(key),
                }
            }
        }
    }

    fn set_env(key: &str, value: &str) {
        // SAFETY: Initial lint enablement: callers hold the module-local `ENV_LOCK`,
        // which serializes these writers but does not prove Unix process-wide exclusion
        // from environment readers. This needs owner review.
        unsafe { env::set_var(key, value) }
    }

    fn remove_env(key: &str) {
        // SAFETY: Initial lint enablement: callers hold the module-local `ENV_LOCK`,
        // which serializes these writers but does not prove Unix process-wide exclusion
        // from environment readers. This needs owner review.
        unsafe { env::remove_var(key) }
    }

    fn clear_env(keys: &[&str]) {
        for key in keys {
            remove_env(key);
        }
    }

    fn file_config(
        api_url: Option<&str>,
        root_ca_path: Option<&str>,
        client_key_path: Option<&str>,
        client_cert_path: Option<&str>,
        rms_root_ca_path: Option<&str>,
    ) -> FileConfig {
        FileConfig {
            api_url: api_url.map(str::to_string),
            root_ca_path: root_ca_path.map(str::to_string),
            client_key_path: client_key_path.map(str::to_string),
            client_cert_path: client_cert_path.map(str::to_string),
            rms_root_ca_path: rms_root_ca_path.map(str::to_string),
        }
    }

    fn summarize_client_cert(
        (client_cert_path, client_key_path, file_config): (
            Option<String>,
            Option<String>,
            Option<FileConfig>,
        ),
    ) -> ClientCertSummary {
        let cert = get_client_cert_info(client_cert_path, client_key_path, file_config.as_ref());
        ClientCertSummary {
            cert_path: cert.cert_path,
            key_path: cert.key_path,
        }
    }

    fn summarize_file_config(config: Option<FileConfig>) -> Option<FileConfigSummary> {
        config.map(|config| FileConfigSummary {
            api_url: config.api_url,
            root_ca_path: config.root_ca_path,
            client_key_path: config.client_key_path,
            client_cert_path: config.client_cert_path,
            rms_root_ca_path: config.rms_root_ca_path,
        })
    }

    /// Caller must hold `ENV_LOCK` before invoking this function.
    fn parse_proxy(input: ProxyInput) -> Result<Option<String>, &'static str> {
        clear_env(PROXY_ENV_KEYS);
        for (key, value) in input.vars {
            set_env(key, value);
        }
        get_proxy_info().map_err(proxy_error_kind)
    }

    fn proxy_error_kind(error: ClientConfigError) -> &'static str {
        match error {
            ClientConfigError::UrlParseError(_) => "url-parse",
        }
    }

    fn unique_home() -> PathBuf {
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        env::temp_dir().join(format!("carbide-tls-test-{}-{nanos}", std::process::id()))
    }

    struct TempHome {
        path: PathBuf,
    }

    impl TempHome {
        fn create() -> Self {
            let path = unique_home();
            fs::create_dir_all(&path).unwrap();
            Self { path }
        }

        fn path(&self) -> &Path {
            &self.path
        }
    }

    impl Drop for TempHome {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.path);
        }
    }

    #[test]
    fn resolves_api_url_precedence() {
        value_scenarios!(
            run = |(api_url, file_config): (Option<String>, Option<FileConfig>)| {
                get_api_url(api_url, file_config.as_ref())
            };
            "command line" {
                (
                    Some("https://cli.example.com".to_string()),
                    Some(file_config(
                        Some("https://file.example.com"),
                        None,
                        None,
                        None,
                        None,
                    )),
                ) => "https://cli.example.com".to_string(),
            }

            "config file" {
                (
                    None,
                    Some(file_config(
                        Some("https://file.example.com"),
                        None,
                        None,
                        None,
                        None,
                    )),
                ) => "https://file.example.com".to_string(),
            }

            "default" {
                (None, None) => "https://carbide-api.forge-system.svc.cluster.local:1079".to_string(),
            }
        );
    }

    #[test]
    fn resolves_client_cert_precedence() {
        value_scenarios!(
            run = summarize_client_cert;
            "command line" {
                (
                    Some("/cli/client.crt".to_string()),
                    Some("/cli/client.key".to_string()),
                    Some(file_config(
                        None,
                        None,
                        Some("/file/client.key"),
                        Some("/file/client.crt"),
                        None,
                    )),
                ) => ClientCertSummary {
                    cert_path: "/cli/client.crt".to_string(),
                    key_path: "/cli/client.key".to_string(),
                },
            }

            "config file" {
                (
                    None,
                    None,
                    Some(file_config(
                        None,
                        None,
                        Some("/file/client.key"),
                        Some("/file/client.crt"),
                        None,
                    )),
                ) => ClientCertSummary {
                    cert_path: "/file/client.crt".to_string(),
                    key_path: "/file/client.key".to_string(),
                },
            }

            "partial command line falls through" {
                (
                    Some("/cli/client.crt".to_string()),
                    None,
                    Some(file_config(
                        None,
                        None,
                        Some("/file/client.key"),
                        Some("/file/client.crt"),
                        None,
                    )),
                ) => ClientCertSummary {
                    cert_path: "/file/client.crt".to_string(),
                    key_path: "/file/client.key".to_string(),
                },
            }
        );
    }

    #[test]
    fn resolves_root_ca_precedence() {
        value_scenarios!(
            run = |(root_ca_path, file_config): (Option<String>, Option<FileConfig>)| {
                get_root_ca_path(root_ca_path, file_config.as_ref())
            };
            "command line" {
                (
                    Some("/cli/root-ca.pem".to_string()),
                    Some(file_config(None, Some("/file/root-ca.pem"), None, None, None)),
                ) => "/cli/root-ca.pem".to_string(),
            }

            "config file" {
                (
                    None,
                    Some(file_config(None, Some("/file/root-ca.pem"), None, None, None)),
                ) => "/file/root-ca.pem".to_string(),
            }
        );
    }

    #[test]
    fn reads_config_file_from_home() {
        let _lock = ENV_LOCK.lock().unwrap();
        let _snapshot = EnvSnapshot::capture(&["HOME"]);
        let home = TempHome::create();
        let config_path = home.path().join(CONFIG_FILE_LOCATION);
        fs::create_dir_all(config_path.parent().unwrap()).unwrap();
        fs::write(
            &config_path,
            r#"{
                "api_url": "https://file.example.com",
                "root_ca_path": "/file/root-ca.pem",
                "client_key_path": "/file/client.key",
                "client_cert_path": "/file/client.crt",
                "rms_root_ca_path": "/file/rms-root-ca.pem"
            }"#,
        )
        .unwrap();
        set_env("HOME", home.path().to_str().unwrap());

        assert_eq!(
            summarize_file_config(get_config_from_file()),
            Some(FileConfigSummary {
                api_url: Some("https://file.example.com".to_string()),
                root_ca_path: Some("/file/root-ca.pem".to_string()),
                client_key_path: Some("/file/client.key".to_string()),
                client_cert_path: Some("/file/client.crt".to_string()),
                rms_root_ca_path: Some("/file/rms-root-ca.pem".to_string()),
            })
        );
    }

    #[test]
    fn reports_no_config_without_home() {
        let _lock = ENV_LOCK.lock().unwrap();
        let _snapshot = EnvSnapshot::capture(&["HOME"]);
        remove_env("HOME");

        assert!(get_config_from_file().is_none());
    }

    #[test]
    fn ensure_url_has_scheme_adds_https_when_missing() {
        use super::ensure_url_has_scheme;

        assert_eq!(
            ensure_url_has_scheme("https://example.com:1079"),
            "https://example.com:1079"
        );
        assert_eq!(
            ensure_url_has_scheme("http://localhost:8080"),
            "http://localhost:8080"
        );

        assert_eq!(
            ensure_url_has_scheme("localhost:1079"),
            "https://localhost:1079"
        );

        assert_eq!(
            ensure_url_has_scheme("HTTPS://example.com"),
            "HTTPS://example.com"
        );

        assert_eq!(
            ensure_url_has_scheme("  localhost:1079  "),
            "https://localhost:1079"
        );
    }

    #[test]
    fn parses_proxy_environment() {
        let _lock = ENV_LOCK.lock().unwrap();
        let _snapshot = EnvSnapshot::capture(PROXY_ENV_KEYS);

        scenarios!(
            run = parse_proxy;
            "not configured" {
                ProxyInput { vars: &[] } => Yields(None),
            }

            "socks5 proxy" {
                ProxyInput {
                    vars: &[("http_proxy", "socks5://localhost:1080")]
                } => Yields(Some("localhost:1080".to_string())),
            }

            "proxy precedence" {
                ProxyInput {
                    vars: &[
                        ("HTTPS_PROXY", "socks5://fallback.example.com:1080"),
                        ("http_proxy", "socks5://first.example.com:2222"),
                    ]
                } => Yields(Some("first.example.com:2222".to_string())),
            }

            "unsupported proxy" {
                ProxyInput {
                    vars: &[("http_proxy", "http://localhost:8080")]
                } => FailsWith("url-parse"),
                ProxyInput {
                    vars: &[("http_proxy", "not a uri")]
                } => FailsWith("url-parse"),
            }
        );
    }
}
