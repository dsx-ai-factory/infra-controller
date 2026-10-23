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

// src/config.rs
// This module re-exports FirmwareSpec and defines the firmware configuration
// types FlashSpec, FlashOptions, and FirmwareFlasherProfile. I originally had
// this single SupernicFirmwareConfig type in here, but it started to get
// kind of messy when I tried to implement it. By breaking it up into a
// a structured separation of concerns, I ended up with a pretty nice RAII
// model for initializing a new FirmwareFlasher, and then using the mix
// of specs + options to drive further operations/management.

use std::path::{Path, PathBuf};

pub use carbide_libmlx_model::firmware::FirmwareSpec;
use rpc::protos::mlx_device::{
    FirmwareFlasherProfile as FirmwareFlasherProfilePb, FlashOptions as FlashOptionsPb,
    FlashSpec as FlashSpecPb,
};
use serde::{Deserialize, Deserializer, Serialize};

use crate::firmware::credentials::Credentials;
use crate::firmware::error::{FirmwareError, FirmwareResult};
use crate::firmware::reset::DEFAULT_RESET_LEVEL;
use crate::firmware::source::FirmwareSource;

/// `FlashSpec` identifies the image and optional configuration sources used by
/// preparation and standalone image verification.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FlashSpec {
    /// `firmware_url` locates the image using a local path or a `file://`,
    /// `http://`, `https://`, or `ssh://` URL.
    pub firmware_url: String,
    /// `firmware_credentials` selects authentication for the image source.
    /// Omission uses the source's default authentication.
    pub firmware_credentials: Option<Credentials>,
    /// `device_conf_url` locates optional configuration applied with
    /// `mlxconfig apply` before burn. It accepts the same formats as
    /// `firmware_url`; omission skips configuration apply.
    pub device_conf_url: Option<String>,
    /// `device_conf_credentials` selects authentication for the optional config
    /// source. Omission uses that source's default authentication.
    pub device_conf_credentials: Option<Credentials>,
    /// `verify_from_cache` makes standalone `verify_image` prefer an existing
    /// file named after the source basename in `cache_dir`. A miss resolves the
    /// source. Only callers or older library versions populate that basename
    /// cache; preparation does not write it. Defaults to `false`. `apply` always
    /// verifies the image it prepared for that burn.
    #[serde(default)]
    pub verify_from_cache: bool,
    /// `cache_dir` contains unique preparation directories with separate image
    /// and configuration files. Defaults to `std::env::temp_dir()` joined with
    /// `mlxconfig-firmware`. Successful workflows remove their inputs.
    /// Interrupted preparation, uncertain execution, and every failure after
    /// burn retain inputs for reconciliation, including version mismatches.
    /// Retained copies have no automatic expiry or count limit.
    /// Supervised operations override this parent with the private slot directory.
    pub cache_dir: Option<PathBuf>,
}

impl FlashSpec {
    /// Resolve caller-local inputs before a long-lived supervisor receives them.
    /// Remote SSH paths remain relative to the remote account, and resolving a
    /// local path does not require that it still exists when rereading a receipt.
    /// SSH usernames and default key paths are captured from this caller;
    /// agent sockets and known-hosts policy remain owned by the supervisor.
    #[cfg(target_os = "linux")]
    pub(in crate::firmware) fn normalize_submission_paths(
        &mut self,
        directory: &Path,
    ) -> FirmwareResult<()> {
        normalize_submission_source(
            &mut self.firmware_url,
            &mut self.firmware_credentials,
            directory,
            "firmware_url",
        )?;
        if let Some(source) = &mut self.device_conf_url {
            normalize_submission_source(
                source,
                &mut self.device_conf_credentials,
                directory,
                "device_conf_url",
            )?;
        }
        for (credentials, field) in [
            (&mut self.firmware_credentials, "firmware_credentials.path"),
            (
                &mut self.device_conf_credentials,
                "device_conf_credentials.path",
            ),
        ] {
            if let Some(Credentials::SshKey { path, .. }) = credentials {
                *path = absolute_submission_path(path, directory, field)?;
            }
        }
        Ok(())
    }

    /// Reject wire inputs that would depend on the supervisor's working directory.
    #[cfg(target_os = "linux")]
    pub(in crate::firmware) fn validate_submission_paths(&self) -> FirmwareResult<()> {
        for (source, field) in [
            (Some(self.firmware_url.as_str()), "firmware_url"),
            (self.device_conf_url.as_deref(), "device_conf_url"),
        ] {
            if let Some(path) = source.and_then(local_submission_path) {
                require_absolute_submission_path(path, field)?;
            }
        }
        for (credentials, field) in [
            (&self.firmware_credentials, "firmware_credentials.path"),
            (
                &self.device_conf_credentials,
                "device_conf_credentials.path",
            ),
        ] {
            if let Some(Credentials::SshKey { path, .. }) = credentials {
                require_absolute_submission_path(path, field)?;
            }
        }
        Ok(())
    }

    // build_firmware_source constructs a FirmwareSource from the
    // firmware_url and firmware_credentials fields.
    pub fn build_firmware_source(&self) -> FirmwareResult<FirmwareSource> {
        let source = FirmwareSource::from_url(&self.firmware_url)?;
        Ok(match self.firmware_credentials.clone() {
            Some(cred) => source.with_credentials(cred),
            None => source,
        })
    }

    // build_device_conf_source constructs a FirmwareSource for the
    // device config, if device_conf_url is configured.
    pub fn build_device_conf_source(&self) -> FirmwareResult<Option<FirmwareSource>> {
        match &self.device_conf_url {
            Some(url) => {
                let source = FirmwareSource::from_url(url)?;
                Ok(Some(match self.device_conf_credentials.clone() {
                    Some(cred) => source.with_credentials(cred),
                    None => source,
                }))
            }
            None => Ok(None),
        }
    }

    // cache_dir_or_default returns the configured cache directory
    // or a default temporary directory.
    pub fn cache_dir_or_default(&self) -> PathBuf {
        self.cache_dir
            .clone()
            .unwrap_or_else(|| std::env::temp_dir().join("mlxconfig-firmware"))
    }
}

// Match FirmwareSource::from_url: file:// is a local-path prefix, while an
// unrecognized prefix is part of a local filename, not another URL scheme.
#[cfg(target_os = "linux")]
fn local_submission_path(source: &str) -> Option<&str> {
    if source.starts_with("http://")
        || source.starts_with("https://")
        || source.starts_with("ssh://")
    {
        None
    } else {
        Some(source.strip_prefix("file://").unwrap_or(source))
    }
}

#[cfg(target_os = "linux")]
fn normalize_submission_source(
    source: &mut String,
    credentials: &mut Option<Credentials>,
    directory: &Path,
    field: &str,
) -> FirmwareResult<()> {
    if let Some(path) = local_submission_path(source) {
        let path = absolute_submission_path(path, directory, field)?;
        *source = if source.starts_with("file://") {
            format!("file://{path}")
        } else {
            path
        };
    } else if source.starts_with("ssh://") {
        let FirmwareSource::Ssh(ssh) = FirmwareSource::from_url(source).map_err(|_| {
            FirmwareError::ConfigError(format!("{field} is not a valid SSH source"))
        })?
        else {
            return Err(FirmwareError::ConfigError(format!(
                "{field} is not an SSH source"
            )));
        };
        if ssh.username.contains('@') || ssh.username.contains(':') {
            return Err(FirmwareError::ConfigError(format!(
                "{field} SSH username cannot contain @ or :"
            )));
        }
        let host = if ssh.host.contains(':') {
            format!("[{}]", ssh.host)
        } else {
            ssh.host
        };
        *source = format!("ssh://{}@{host}:{}", ssh.username, ssh.remote_path);
        if credentials.is_none() {
            // Match forge_ssh's default authentication, not an agent fallback.
            let home = std::env::var("HOME").unwrap_or_else(|_| "/root".into());
            let key = Path::new(&home).join(".ssh/id_rsa");
            *credentials = Some(Credentials::ssh_key(key.to_string_lossy()));
        }
    }
    Ok(())
}

#[cfg(target_os = "linux")]
fn absolute_submission_path(path: &str, directory: &Path, field: &str) -> FirmwareResult<String> {
    if path.is_empty() {
        return Err(FirmwareError::ConfigError(format!(
            "{field} must not be empty"
        )));
    }
    // Do not canonicalize: a retry must still identify accepted work after its
    // original source has been removed, and symlinks retain their path semantics.
    let absolute = directory.join(path);
    require_absolute_submission_path(&absolute, field)?;
    absolute
        .into_os_string()
        .into_string()
        .map_err(|_| FirmwareError::ConfigError(format!("{field} must resolve to a UTF-8 path")))
}

#[cfg(target_os = "linux")]
fn require_absolute_submission_path(path: impl AsRef<Path>, field: &str) -> FirmwareResult<()> {
    if path.as_ref().is_absolute() {
        Ok(())
    } else {
        Err(FirmwareError::ConfigError(format!(
            "{field} must be absolute for firmware supervisor submission"
        )))
    }
}

fn default_reset_level() -> u8 {
    DEFAULT_RESET_LEVEL
}

// FlashOptions contains lifecycle flags for the apply() workflow
// orchestrator, which takes a FirmwareFlasherProfile. This controls
// which post-flash steps are executed, and their parameters.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FlashOptions {
    // verify_image controls whether the firmware image is verified
    // against the source binary after flashing.
    #[serde(default)]
    pub verify_image: bool,
    // verify_version controls whether the firmware version on the
    // device is checked after flashing.
    #[serde(default)]
    pub verify_version: bool,
    // reset controls whether the device is reset via mlxfwreset
    // after flashing.
    #[serde(default)]
    pub reset: bool,
    // reset_level is the mlxfwreset level to use. Defaults to 3.
    #[serde(default = "default_reset_level")]
    pub reset_level: u8,
}

impl Default for FlashOptions {
    fn default() -> Self {
        Self {
            verify_image: false,
            verify_version: false,
            reset: false,
            reset_level: DEFAULT_RESET_LEVEL,
        }
    }
}

// FirmwareFlasherProfile bundles a FirmwareSpec, FlashSpec, and
// FlashOptions into a complete firmware management profile. This
// is the top-level configuration type used in the API runtime config
// and sent to scout via OpCode::ApplyFirmware.
//
// The #[serde(flatten)] attributes allow TOML config to stay flat:
//   [[supernic_firmware_profiles]]
//   part_number = "900-9D3B4-00CV-TA0"
//   psid = "MT_0000000884"
//   version = "32.43.1014"
//   firmware_url = "https://..."
//   reset = true
#[derive(Debug, Clone, Serialize)]
pub struct FirmwareFlasherProfile {
    #[serde(flatten)]
    pub firmware_spec: FirmwareSpec,
    #[serde(flatten)]
    pub flash_spec: FlashSpec,
    #[serde(flatten, default)]
    pub flash_options: FlashOptions,
}

impl<'de> Deserialize<'de> for FirmwareFlasherProfile {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        // `deny_unknown_fields` cannot be combined with `flatten`, so use one
        // strict flat representation and rebuild the three public sections.
        #[derive(Deserialize)]
        #[serde(deny_unknown_fields)]
        struct FlatFirmwareFlasherProfile {
            part_number: String,
            psid: String,
            version: String,
            firmware_url: String,
            firmware_credentials: Option<Credentials>,
            device_conf_url: Option<String>,
            device_conf_credentials: Option<Credentials>,
            #[serde(default)]
            verify_from_cache: bool,
            cache_dir: Option<PathBuf>,
            #[serde(default)]
            verify_image: bool,
            #[serde(default)]
            verify_version: bool,
            #[serde(default)]
            reset: bool,
            #[serde(default = "default_reset_level")]
            reset_level: u8,
        }

        let profile = FlatFirmwareFlasherProfile::deserialize(deserializer)?;
        Ok(Self {
            firmware_spec: FirmwareSpec {
                part_number: profile.part_number,
                psid: profile.psid,
                version: profile.version,
            },
            flash_spec: FlashSpec {
                firmware_url: profile.firmware_url,
                firmware_credentials: profile.firmware_credentials,
                device_conf_url: profile.device_conf_url,
                device_conf_credentials: profile.device_conf_credentials,
                verify_from_cache: profile.verify_from_cache,
                cache_dir: profile.cache_dir,
            },
            flash_options: FlashOptions {
                verify_image: profile.verify_image,
                verify_version: profile.verify_version,
                reset: profile.reset,
                reset_level: profile.reset_level,
            },
        })
    }
}

impl FirmwareFlasherProfile {
    // from_file reads a FirmwareFlasherProfile from a TOML file.
    pub fn from_file(path: impl AsRef<Path>) -> FirmwareResult<Self> {
        let content = std::fs::read_to_string(path.as_ref()).map_err(FirmwareError::Io)?;
        Self::from_toml(&content)
    }

    // from_toml parses a FirmwareFlasherProfile from a TOML string.
    pub fn from_toml(toml_str: &str) -> FirmwareResult<Self> {
        toml::from_str(toml_str).map_err(|e| {
            FirmwareError::ConfigError(format!("Failed to parse firmware profile: {e}"))
        })
    }
}

// From implementations for converting FlashSpec
// to/from a FlashSpecPb protobuf message and back.
impl From<FlashSpec> for FlashSpecPb {
    fn from(spec: FlashSpec) -> Self {
        FlashSpecPb {
            firmware_url: spec.firmware_url,
            firmware_credentials: spec.firmware_credentials.map(Into::into),
            device_conf_url: spec.device_conf_url,
            device_conf_credentials: spec.device_conf_credentials.map(Into::into),
            verify_from_cache: spec.verify_from_cache,
            cache_dir: spec.cache_dir.map(|p| p.to_string_lossy().into_owned()),
        }
    }
}

impl TryFrom<FlashSpecPb> for FlashSpec {
    type Error = String;

    fn try_from(proto: FlashSpecPb) -> Result<Self, Self::Error> {
        Ok(FlashSpec {
            firmware_url: proto.firmware_url,
            firmware_credentials: proto
                .firmware_credentials
                .map(TryInto::try_into)
                .transpose()?,
            device_conf_url: proto.device_conf_url,
            device_conf_credentials: proto
                .device_conf_credentials
                .map(TryInto::try_into)
                .transpose()?,
            verify_from_cache: proto.verify_from_cache,
            cache_dir: proto.cache_dir.map(PathBuf::from),
        })
    }
}

// From implementations for converting FlashOptions
// to/from a FlashOptionsPb protobuf message and back.
impl From<FlashOptions> for FlashOptionsPb {
    fn from(opts: FlashOptions) -> Self {
        FlashOptionsPb {
            verify_image: opts.verify_image,
            verify_version: opts.verify_version,
            reset: opts.reset,
            reset_level: opts.reset_level as u32,
        }
    }
}

impl TryFrom<FlashOptionsPb> for FlashOptions {
    type Error = String;

    fn try_from(proto: FlashOptionsPb) -> Result<Self, Self::Error> {
        Ok(FlashOptions {
            verify_image: proto.verify_image,
            verify_version: proto.verify_version,
            reset: proto.reset,
            reset_level: u8::try_from(proto.reset_level)
                .map_err(|_| "reset_level must be in 0..=255".to_string())?,
        })
    }
}

// From implementations for converting FirmwareFlasherProfile
// to/from a FirmwareFlasherProfilePb protobuf message and back.
impl From<FirmwareFlasherProfile> for FirmwareFlasherProfilePb {
    fn from(profile: FirmwareFlasherProfile) -> Self {
        FirmwareFlasherProfilePb {
            firmware_spec: Some(profile.firmware_spec.into()),
            flash_spec: Some(profile.flash_spec.into()),
            flash_options: Some(profile.flash_options.into()),
        }
    }
}

impl TryFrom<FirmwareFlasherProfilePb> for FirmwareFlasherProfile {
    type Error = String;

    fn try_from(proto: FirmwareFlasherProfilePb) -> Result<Self, Self::Error> {
        let firmware_spec = proto.firmware_spec.ok_or("missing firmware_spec")?.into();
        let flash_spec: FlashSpec = proto.flash_spec.ok_or("missing flash_spec")?.try_into()?;
        let flash_options = proto
            .flash_options
            .map(TryInto::try_into)
            .transpose()?
            .unwrap_or_default();

        Ok(FirmwareFlasherProfile {
            firmware_spec,
            flash_spec,
            flash_options,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Resolve caller-local references without reading files, so retries still
    /// identify accepted work after the original inputs have been removed.
    #[cfg(target_os = "linux")]
    #[test]
    fn submission_source_paths_preserve_source_semantics() {
        use carbide_test_support::Outcome::{Fails, Yields};
        use carbide_test_support::scenarios;

        let normalize = |(source, directory): (&str, &str)| -> Result<String, String> {
            let mut source = source.to_string();
            normalize_submission_source(
                &mut source,
                &mut None,
                Path::new(directory),
                "firmware_url",
            )
            .map_err(|error| error.to_string())?;
            Ok(source)
        };
        scenarios!(normalize:
            "caller-local references become absolute without reading files" {
                ("image.bin", "/submitter") => Yields("/submitter/image.bin".to_string()),
                ("file://config.bin", "/submitter") => Yields("file:///submitter/config.bin".to_string()),
            }
            "already independent references remain unchanged" {
                ("/missing/image.bin", "") => Yields("/missing/image.bin".to_string()),
                ("https://host/image.bin?token=secret", "") => Yields("https://host/image.bin?token=secret".to_string()),
            }
            "a relative input needs the submitting directory" {
                ("image.bin", "") => Fails,
            }
        );
    }

    /// SSH keys belong to the submitter's filesystem; remote paths and their
    /// punctuation must not be reinterpreted as local paths.
    #[cfg(target_os = "linux")]
    #[test]
    fn submission_ssh_keys_are_local_and_remote_paths_remain_remote() {
        let mut spec = FlashSpec {
            firmware_url: "ssh://deploy@[2001:db8::1]:images/fw@version:1.bin".into(),
            firmware_credentials: Some(Credentials::ssh_key_with_passphrase(
                "keys/image",
                "secret",
            )),
            device_conf_url: Some("ssh://deploy@host:config/shared.bin".into()),
            device_conf_credentials: Some(Credentials::ssh_key("keys/config")),
            verify_from_cache: false,
            cache_dir: None,
        };
        spec.normalize_submission_paths(Path::new("/submitter"))
            .unwrap();
        spec.validate_submission_paths().unwrap();
        assert_eq!(
            spec.firmware_url,
            "ssh://deploy@[2001:db8::1]:images/fw@version:1.bin"
        );
        assert_eq!(
            spec.device_conf_url.as_deref(),
            Some("ssh://deploy@host:config/shared.bin")
        );
        assert!(matches!(
            spec.firmware_credentials,
            Some(Credentials::SshKey { path, passphrase })
                if path == "/submitter/keys/image" && passphrase.as_deref() == Some("secret")
        ));
        assert!(matches!(
            spec.device_conf_credentials,
            Some(Credentials::SshKey { path, passphrase: None })
                if path == "/submitter/keys/config"
        ));
    }

    /// Capture implicit account/key choices before a reused daemon can apply
    /// its own environment, while retaining explicitly requested agent auth.
    #[cfg(target_os = "linux")]
    #[tokio::test]
    async fn submission_captures_ssh_defaults() {
        const CHILD: &str = "LIBMLX_TEST_SUBMISSION_CONTEXT";
        if std::env::var_os(CHILD).is_none() {
            // Only this subprocess changes authentication-related environment;
            // parallel tests in the parent keep their original environment.
            let output = tokio::time::timeout(
                std::time::Duration::from_secs(10),
                tokio::process::Command::new(std::env::current_exe().unwrap())
                    .env_clear()
                    .env(CHILD, "1")
                    .env("USER", "submitter")
                    .env("LOGNAME", "other-account")
                    .env("HOME", "relative-home")
                    .args([
                        "--exact",
                        "firmware::config::tests::submission_captures_ssh_defaults",
                        "--nocapture",
                    ])
                    .kill_on_drop(true)
                    .output(),
            )
            .await
            .expect("submission environment fixture exceeded its watchdog")
            .unwrap();
            assert!(
                output.status.success(),
                "{}",
                String::from_utf8_lossy(&output.stderr)
            );
            assert!(String::from_utf8_lossy(&output.stdout).contains("1 passed"));
            return;
        }

        let mut spec = FlashSpec {
            firmware_url: "ssh://[2001:db8::1]:images/fw@version:1.bin".into(),
            firmware_credentials: None,
            device_conf_url: Some("ssh://host:config/shared.bin".into()),
            device_conf_credentials: Some(Credentials::ssh_agent()),
            verify_from_cache: false,
            cache_dir: None,
        };
        spec.normalize_submission_paths(Path::new("/submission-directory"))
            .unwrap();
        assert_eq!(
            spec.firmware_url,
            "ssh://submitter@[2001:db8::1]:images/fw@version:1.bin"
        );
        assert_eq!(
            spec.device_conf_url.as_deref(),
            Some("ssh://submitter@host:config/shared.bin")
        );
        assert!(matches!(
            spec.firmware_credentials,
            Some(Credentials::SshKey { path, passphrase: None })
                if path == "/submission-directory/relative-home/.ssh/id_rsa"
        ));
        assert!(matches!(
            spec.device_conf_credentials,
            Some(Credentials::SshAgent {})
        ));
    }

    /// Raw submissions cannot rely on the daemon's working directory, and
    /// rejection identifies the field without disclosing source or key paths.
    #[cfg(target_os = "linux")]
    #[test]
    fn submission_validation_rejects_unresolved_local_fields() {
        struct Case {
            source: &'static str,
            configuration: Option<&'static str>,
            key: Option<&'static str>,
            invalid_field: &'static str,
        }
        for case in [
            // Image paths are resolved even when no configuration is requested.
            Case {
                source: "private-image.bin",
                configuration: None,
                key: None,
                invalid_field: "firmware_url",
            },
            // An absolute image cannot hide an unresolved configuration path.
            Case {
                source: "/image.bin",
                configuration: Some("file://private-config.bin"),
                key: None,
                invalid_field: "device_conf_url",
            },
            // Remote input still needs an absolute local authentication key.
            Case {
                source: "ssh://deploy@host:remote-image.bin",
                configuration: None,
                key: Some("private-key"),
                invalid_field: "firmware_credentials.path",
            },
        ] {
            let mut spec = FlashSpec {
                firmware_url: case.source.into(),
                firmware_credentials: case.key.map(Credentials::ssh_key),
                device_conf_url: case.configuration.map(str::to_string),
                device_conf_credentials: None,
                verify_from_cache: false,
                cache_dir: None,
            };
            let error = spec.validate_submission_paths().unwrap_err().to_string();
            assert!(error.contains(case.invalid_field), "{error}");
            assert!(
                !error.contains("private-"),
                "source and key paths stay out of diagnostics"
            );
            spec.normalize_submission_paths(Path::new("/submitter"))
                .unwrap();
            spec.validate_submission_paths().unwrap();
        }
    }

    #[test]
    fn test_flash_options_default() {
        let opts = FlashOptions::default();
        assert!(!opts.verify_image);
        assert!(!opts.verify_version);
        assert!(!opts.reset);
        assert_eq!(opts.reset_level, 3);
    }

    #[test]
    fn test_flash_options_roundtrip() {
        let original = FlashOptions {
            verify_image: true,
            verify_version: true,
            reset: true,
            reset_level: 5,
        };
        let proto: FlashOptionsPb = original.clone().into();
        let converted: FlashOptions = proto.try_into().unwrap();
        assert_eq!(original.verify_image, converted.verify_image);
        assert_eq!(original.verify_version, converted.verify_version);
        assert_eq!(original.reset, converted.reset);
        assert_eq!(original.reset_level, converted.reset_level);
    }

    #[test]
    fn profile_options_preserve_omission_and_reject_reset_level_overflow() {
        use carbide_test_support::Outcome::{Fails, Yields};
        use carbide_test_support::scenarios;

        let convert = |level: Option<u32>| {
            FirmwareFlasherProfile::try_from(FirmwareFlasherProfilePb {
                firmware_spec: Some(Default::default()),
                flash_spec: Some(FlashSpecPb::default()),
                flash_options: level.map(|reset_level| FlashOptionsPb {
                    reset_level,
                    ..Default::default()
                }),
            })
            .map(|profile| profile.flash_options.reset_level)
        };
        scenarios!(convert:
            "an absent options message uses the library default" {
                None => Yields(3),
            }
            "a present options message preserves the wire value" {
                Some(0) => Yields(0),
                Some(255) => Yields(255),
            }
            "out-of-range levels cannot alias accepted work" {
                Some(256) => Fails,
            }
        );
    }

    #[test]
    fn test_profile_full_roundtrip() {
        let original = FirmwareFlasherProfile {
            firmware_spec: FirmwareSpec {
                part_number: "900-9D3B4-00CV-TA0".to_string(),
                psid: "MT_0000000884".to_string(),
                version: "32.43.1014".to_string(),
            },
            flash_spec: FlashSpec {
                firmware_url: "https://artifacts.nvidia.com/fw.bin".to_string(),
                firmware_credentials: Some(Credentials::bearer_token("token123")),
                device_conf_url: Some("https://artifacts.nvidia.com/debug.conf".to_string()),
                device_conf_credentials: Some(Credentials::basic_auth("user", "pass")),
                verify_from_cache: true,
                cache_dir: Some(PathBuf::from("/var/cache/fw")),
            },
            flash_options: FlashOptions {
                verify_image: true,
                verify_version: true,
                reset: true,
                reset_level: 3,
            },
        };
        let proto: FirmwareFlasherProfilePb = original.clone().into();
        let converted: FirmwareFlasherProfile = proto.try_into().unwrap();

        assert_eq!(
            original.firmware_spec.part_number,
            converted.firmware_spec.part_number
        );
        assert_eq!(original.firmware_spec.psid, converted.firmware_spec.psid);
        assert_eq!(
            original.firmware_spec.version,
            converted.firmware_spec.version
        );
        assert_eq!(
            original.flash_spec.firmware_url,
            converted.flash_spec.firmware_url
        );
        assert_eq!(
            original.flash_spec.device_conf_url,
            converted.flash_spec.device_conf_url
        );
        assert_eq!(
            original.flash_spec.verify_from_cache,
            converted.flash_spec.verify_from_cache
        );
        assert!(converted.flash_spec.firmware_credentials.is_some());
        assert!(converted.flash_spec.device_conf_credentials.is_some());
        assert!(converted.flash_options.verify_image);
        assert!(converted.flash_options.verify_version);
        assert!(converted.flash_options.reset);
        assert_eq!(converted.flash_options.reset_level, 3);
    }

    #[test]
    fn test_profile_minimal_roundtrip() {
        let original = FirmwareFlasherProfile {
            firmware_spec: FirmwareSpec {
                part_number: "900-9D3B4-00CV-TA0".to_string(),
                psid: "MT_0000000884".to_string(),
                version: "32.43.1014".to_string(),
            },
            flash_spec: FlashSpec {
                firmware_url: "/local/path/fw.bin".to_string(),
                firmware_credentials: None,
                device_conf_url: None,
                device_conf_credentials: None,
                verify_from_cache: false,
                cache_dir: None,
            },
            flash_options: FlashOptions::default(),
        };
        let proto: FirmwareFlasherProfilePb = original.clone().into();
        let converted: FirmwareFlasherProfile = proto.try_into().unwrap();

        assert_eq!(
            original.firmware_spec.part_number,
            converted.firmware_spec.part_number
        );
        assert_eq!(
            original.flash_spec.firmware_url,
            converted.flash_spec.firmware_url
        );
        assert!(converted.flash_spec.firmware_credentials.is_none());
        assert!(converted.flash_spec.device_conf_url.is_none());
        assert!(!converted.flash_options.reset);
        assert!(!converted.flash_options.verify_image);
        assert!(!converted.flash_options.verify_version);
        assert_eq!(converted.flash_options.reset_level, 3);
    }

    #[test]
    fn test_profile_toml_roundtrip() {
        let toml_str = r#"
part_number = "900-9D3B4-00CV-TA0"
psid = "MT_0000000884"
version = "32.43.1014"
firmware_url = "https://artifacts.nvidia.com/fw.bin"
reset = true
verify_image = true
verify_version = true
"#;
        let profile = FirmwareFlasherProfile::from_toml(toml_str).unwrap();
        assert_eq!(profile.firmware_spec.part_number, "900-9D3B4-00CV-TA0");
        assert_eq!(profile.firmware_spec.version, "32.43.1014");
        assert_eq!(
            profile.flash_spec.firmware_url,
            "https://artifacts.nvidia.com/fw.bin"
        );
        assert!(profile.flash_options.reset);
        assert!(profile.flash_options.verify_image);
        assert!(profile.flash_options.verify_version);
        assert_eq!(profile.flash_options.reset_level, 3); // default
    }

    #[test]
    fn profile_toml_rejects_unknown_fields() {
        let toml_str = r#"
part_number = "900-9D3B4-00CV-TA0"
psid = "MT_0000000884"
version = "32.43.1014"
firmware_url = "https://artifacts.nvidia.com/fw.bin"
firmware_urll = "https://typo.example.com/fw.bin"
"#;

        let error = FirmwareFlasherProfile::from_toml(toml_str).unwrap_err();
        assert!(error.to_string().contains("firmware_urll"));
    }

    #[test]
    fn profile_serde_round_trip_preserves_every_field() {
        let original = FirmwareFlasherProfile {
            firmware_spec: FirmwareSpec {
                part_number: "900-9D3B4-00CV-TA0".to_string(),
                psid: "MT_0000000884".to_string(),
                version: "32.43.1014".to_string(),
            },
            flash_spec: FlashSpec {
                firmware_url: "https://artifacts.nvidia.com/fw.bin".to_string(),
                firmware_credentials: Some(Credentials::bearer_token("token123")),
                device_conf_url: Some("https://artifacts.nvidia.com/debug.conf".to_string()),
                device_conf_credentials: Some(Credentials::basic_auth("user", "pass")),
                verify_from_cache: true,
                cache_dir: Some(PathBuf::from("/var/cache/fw")),
            },
            flash_options: FlashOptions {
                verify_image: true,
                verify_version: true,
                reset: true,
                reset_level: 5,
            },
        };

        let encoded = toml::to_string(&original).expect("profile serializes");
        let decoded = FirmwareFlasherProfile::from_toml(&encoded)
            .expect("serialized profile must deserialize under the strict schema");

        assert_eq!(
            decoded.firmware_spec.part_number,
            original.firmware_spec.part_number
        );
        assert_eq!(decoded.firmware_spec.psid, original.firmware_spec.psid);
        assert_eq!(
            decoded.firmware_spec.version,
            original.firmware_spec.version
        );
        assert_eq!(
            decoded.flash_spec.firmware_url,
            original.flash_spec.firmware_url
        );
        assert_eq!(
            decoded.flash_spec.device_conf_url,
            original.flash_spec.device_conf_url
        );
        assert!(decoded.flash_spec.verify_from_cache);
        assert_eq!(decoded.flash_spec.cache_dir, original.flash_spec.cache_dir);
        assert!(matches!(
            decoded.flash_spec.firmware_credentials,
            Some(Credentials::BearerToken { token }) if token == "token123"
        ));
        assert!(matches!(
            decoded.flash_spec.device_conf_credentials,
            Some(Credentials::BasicAuth { username, password })
                if username == "user" && password == "pass"
        ));
        assert!(decoded.flash_options.verify_image);
        assert!(decoded.flash_options.verify_version);
        assert!(decoded.flash_options.reset);
        assert_eq!(
            decoded.flash_options.reset_level,
            original.flash_options.reset_level
        );
    }
}
