use std::net::SocketAddr;
use std::path::Path;

use figment::Figment;
use figment::providers::{Env, Format, Serialized, Toml};
use serde::{Deserialize, Serialize};

use crate::error::RvsError;

/// Top-level RVS service configuration.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct Config {
    /// gRPC listen address (future inbound RPCs).
    pub listen: SocketAddr,
    /// Prometheus metrics / liveness probe endpoint.
    pub metrics_endpoint: SocketAddr,
    /// Paths to scenario definition TOMLs (one per rack model/release).
    pub scenario_config_paths: Vec<String>,
    /// Path to the SOT JSON file -- the release source-of-truth RVS reads to
    /// resolve `sotpath` artifacts (drivers, packages) under
    /// `BoardSKUs[].Components.Software`.
    ///
    /// TODO[#416]: stopgap. The SOT should come from a single authoritative
    /// source, not a file hand-placed per RVS deployment. #1861 moved
    /// firmware-object management to RMS and made the SOT push-only, so NICo
    /// no longer exposes it for read -- we likely need to extend the API
    /// surface to serve the SOT (or its Software-package URLs) and resolve one
    /// per scenario release. Until then, point this at a local SOT file.
    pub sot_path: Option<String>,
    /// How long to wait between validation poll cycles (seconds).
    pub poll_interval_secs: u64,
    /// NICo connection settings.
    pub nico: NicoConfig,
    /// TLS / mTLS certificate paths.
    pub tls: TlsConfig,
    /// Artifact cache settings.
    pub artifact_cache: ArtifactCacheConfig,
}

/// NICo (Carbide API) connection settings.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct NicoConfig {
    /// NICo gRPC endpoint URL.
    pub url: String,
    /// Per-RPC timeout in seconds.
    ///
    /// TODO[#416]: not yet wired - ApiConfig has no timeout field. Wire via
    /// tokio::time::timeout per call or a tower timeout layer once available.
    pub rpc_timeout_secs: u64,
}

/// SPIFFE-based mTLS certificate paths.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct TlsConfig {
    /// Client certificate PEM path.
    pub identity_pemfile_path: String,
    /// Client key PEM path.
    pub identity_keyfile_path: String,
    /// Root CA PEM path.
    pub root_cafile_path: String,
}

/// Artifact pre-cache settings.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct ArtifactCacheConfig {
    /// Directory for cached artifacts.
    pub cache_dir: String,
    /// Download timeout per artifact (seconds).
    pub download_timeout_secs: u64,
    /// Max parallel artifact downloads.
    pub max_concurrent_downloads: u32,
    /// Port for the HTTP artifact cache server (nodes pull from this).
    pub serve_port: u16,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            listen: "[::]:1089".parse().unwrap(),
            metrics_endpoint: "[::]:9019".parse().unwrap(),
            scenario_config_paths: vec!["/etc/forge/rvs/scenario.toml".to_string()],
            sot_path: None,
            poll_interval_secs: 30,
            nico: NicoConfig::default(),
            tls: TlsConfig::default(),
            artifact_cache: ArtifactCacheConfig::default(),
        }
    }
}

impl Default for NicoConfig {
    fn default() -> Self {
        Self {
            url: "https://carbide-api.forge-system.svc.cluster.local:1079".to_string(),
            rpc_timeout_secs: 30,
        }
    }
}

impl Default for TlsConfig {
    fn default() -> Self {
        Self {
            identity_pemfile_path: "/var/run/secrets/spiffe.io/tls.crt".to_string(),
            identity_keyfile_path: "/var/run/secrets/spiffe.io/tls.key".to_string(),
            root_cafile_path: "/var/run/secrets/spiffe.io/ca.crt".to_string(),
        }
    }
}

impl Default for ArtifactCacheConfig {
    fn default() -> Self {
        Self {
            cache_dir: "/rvs-cache".to_string(),
            download_timeout_secs: 600,
            max_concurrent_downloads: 4,
            serve_port: 8080,
        }
    }
}

impl Config {
    /// Load config: defaults -> TOML file -> CARBIDE_RVS__* env vars.
    pub fn load(config_path: Option<&Path>) -> Result<Self, RvsError> {
        let mut figment = Figment::new().merge(Serialized::defaults(Config::default()));

        if let Some(path) = config_path {
            figment = figment.merge(Toml::file(path));
        }

        figment = figment.merge(Env::prefixed("CARBIDE_RVS__").split("__"));

        let config: Config = figment
            .extract()
            .map_err(|e| RvsError::Config(format!("failed to load config: {e}")))?;

        Ok(config)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_defaults_are_valid() {
        let config = Config::default();
        assert_eq!(config.listen, "[::]:1089".parse::<SocketAddr>().unwrap());
        assert_eq!(
            config.metrics_endpoint,
            "[::]:9019".parse::<SocketAddr>().unwrap()
        );
        assert_eq!(config.nico.rpc_timeout_secs, 30);
        assert_eq!(config.artifact_cache.max_concurrent_downloads, 4);
    }

    #[test]
    fn test_load_without_file_uses_defaults() {
        let config = Config::load(None).unwrap();
        assert_eq!(
            config.nico.url,
            "https://carbide-api.forge-system.svc.cluster.local:1079"
        );
        assert_eq!(
            config.tls.root_cafile_path,
            "/var/run/secrets/spiffe.io/ca.crt"
        );
    }

    #[test]
    fn test_load_from_example_file() {
        let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("doc/example_config.toml");
        let config = Config::load(Some(&path)).unwrap();
        assert_eq!(config.listen, "[::]:1089".parse::<SocketAddr>().unwrap());
        assert_eq!(config.nico.rpc_timeout_secs, 30);
        assert_eq!(config.artifact_cache.cache_dir, "/rvs-cache");
    }
}
