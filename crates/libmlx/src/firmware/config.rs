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

// FlashSpec specifies source locations and caching options for
// flash and verify_image operations. Contains everything needed
// to download and apply firmware and device config.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FlashSpec {
    // firmware_url is the location of the firmware binary. Supports
    // local paths, file://, https://, and ssh:// URLs.
    pub firmware_url: String,
    // firmware_credentials is the optional authentication for
    // downloading the firmware binary.
    pub firmware_credentials: Option<Credentials>,
    // device_conf_url is the optional location of the device config
    // to apply before flashing. When present, the config is applied
    // via `mlxconfig apply` before burning the firmware.
    // Supports the same URL formats as firmware_url.
    pub device_conf_url: Option<String>,
    // device_conf_credentials is the optional authentication for
    // downloading the device config.
    pub device_conf_credentials: Option<Credentials>,
    // verify_from_cache controls whether verify_image() uses the
    // cached firmware from cache_dir instead of re-pulling.
    #[serde(default)]
    pub verify_from_cache: bool,
    // cache_dir is the directory for staging downloaded firmware.
    // Defaults to a temporary directory if not specified.
    pub cache_dir: Option<PathBuf>,
}

impl FlashSpec {
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

impl From<FlashOptionsPb> for FlashOptions {
    fn from(proto: FlashOptionsPb) -> Self {
        FlashOptions {
            verify_image: proto.verify_image,
            verify_version: proto.verify_version,
            reset: proto.reset,
            reset_level: proto.reset_level as u8,
        }
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
        let flash_options = proto.flash_options.map(Into::into).unwrap_or_default();

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
        let converted: FlashOptions = proto.into();
        assert_eq!(original.verify_image, converted.verify_image);
        assert_eq!(original.verify_version, converted.verify_version);
        assert_eq!(original.reset, converted.reset);
        assert_eq!(original.reset_level, converted.reset_level);
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
