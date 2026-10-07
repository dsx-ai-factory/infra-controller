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

use std::path::{Component, Path};

use carbide_firmware::resolve_files_firmware_artifact;
pub(crate) use carbide_firmware::{ResolvedFirmwareArtifact, ResolvedFirmwareArtifactSource};
use carbide_utils::none_if_empty::NoneIfEmpty;
use eyre::eyre;
use model::firmware::{FirmwareEntry, FirmwareFileArtifact};
use state_controller::state_handler::StateHandlerError;

use crate::rpc::scout_firmware_upgrade::FileArtifact;

pub(crate) fn resolve_firmware_artifact(
    firmware_download_cache_directory: &Path,
    firmware: &FirmwareEntry,
    pos: u32,
) -> Result<ResolvedFirmwareArtifact, StateHandlerError> {
    match resolve_files_firmware_artifact(firmware_download_cache_directory, firmware, pos)
        .map_err(StateHandlerError::GenericError)?
    {
        Some(artifact) => Ok(artifact),
        None => Ok(ResolvedFirmwareArtifact {
            local_path: firmware.get_filename(pos),
            source: ResolvedFirmwareArtifactSource::Local,
        }),
    }
}

pub(crate) fn resolve_scout_file_artifact(
    pxe_public_base_url: &str,
    firmware_directory: &Path,
    artifact: &FirmwareFileArtifact,
) -> Result<FileArtifact, StateHandlerError> {
    let url = artifact.url.as_deref().map(str::trim).none_if_empty();

    let filename = artifact.filename.as_deref().map(str::trim).none_if_empty();

    let url = if let Some(url) = url {
        url.to_owned()
    } else if let Some(filename) = filename {
        firmware_artifact_url(pxe_public_base_url, firmware_directory, filename)?
    } else {
        return Err(StateHandlerError::GenericError(eyre!(
            "scout firmware artifact has no filename or URL"
        )));
    };

    Ok(FileArtifact {
        url,
        sha256: artifact.sha256.clone(),
    })
}

fn firmware_artifact_url(
    pxe_public_base_url: &str,
    firmware_directory: &Path,
    path: &str,
) -> Result<String, StateHandlerError> {
    let relative = Path::new(path)
        .strip_prefix(firmware_directory)
        .map_err(|_| {
            StateHandlerError::GenericError(eyre!(
                "firmware artifact path {path} is outside firmware directory {}",
                firmware_directory.display()
            ))
        })?;

    if !relative
        .components()
        .all(|component| matches!(component, Component::Normal(_)))
    {
        return Err(StateHandlerError::GenericError(eyre!(
            "firmware artifact path {path} contains unsafe path components"
        )));
    }

    let relative = relative.to_str().ok_or_else(|| {
        StateHandlerError::GenericError(eyre!("firmware artifact path {path} is not valid UTF-8"))
    })?;

    Ok(format!(
        "{}/public/firmware/{relative}",
        pxe_public_base_url.trim_end_matches('/')
    ))
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use carbide_test_support::Outcome::*;
    use carbide_test_support::{Case, check_cases};

    use super::*;

    const FIRMWARE_DOWNLOAD_CACHE_DIRECTORY: &str = "/mnt/persistence/fw/download-cache";
    const FIRMWARE_DIRECTORY: &str = "/opt/nico/firmware";
    const PXE_PUBLIC_BASE_URL: &str = "http://carbide-pxe.forge:8080/";

    struct ResolutionInput {
        firmware: FirmwareEntry,
        pos: u32,
    }

    struct ScoutInput {
        filename: Option<&'static str>,
        url: Option<&'static str>,
    }

    #[derive(Debug, PartialEq, Eq)]
    enum ComparableError {
        Generic(String),
        Other(String),
    }

    #[test]
    fn resolve_firmware_artifact_cases() {
        check_cases(
            [
                Case {
                    scenario: "shared files artifact delegates successfully",
                    input: ResolutionInput {
                        firmware: firmware_with_files(vec![file(
                            Some("/opt/nico/firmware/fw.bin"),
                            None,
                        )]),
                        pos: 0,
                    },
                    expect: Yields(local_artifact("/opt/nico/firmware/fw.bin")),
                },
                Case {
                    scenario: "shared resolver error maps to generic error",
                    input: ResolutionInput {
                        firmware: firmware_with_files(vec![file(None, None)]),
                        pos: 0,
                    },
                    expect: FailsWith(ComparableError::Generic(
                        "firmware version 1.0 files[] artifact at index 0 has no filename or URL"
                            .to_string(),
                    )),
                },
                Case {
                    scenario: "legacy artifact selects indexed filename and remains local",
                    input: ResolutionInput {
                        firmware: FirmwareEntry {
                            version: "1.0".to_string(),
                            filenames: vec![
                                "/opt/nico/firmware/first.bin".to_string(),
                                "/opt/nico/firmware/second.bin".to_string(),
                            ],
                            url: Some("https://firmware.example.invalid/legacy.bin".to_string()),
                            checksum: Some("legacy-sha".to_string()),
                            ..FirmwareEntry::default()
                        },
                        pos: 1,
                    },
                    expect: Yields(local_artifact("/opt/nico/firmware/second.bin")),
                },
            ],
            |ResolutionInput { firmware, pos }| {
                resolve_firmware_artifact(
                    Path::new(FIRMWARE_DOWNLOAD_CACHE_DIRECTORY),
                    &firmware,
                    pos,
                )
                .map_err(comparable_error)
            },
        );
    }

    #[test]
    fn resolve_scout_file_artifact_cases() {
        check_cases(
            [
                Case {
                    scenario: "URL takes precedence over filename",
                    input: ScoutInput {
                        filename: Some("/opt/nico/firmware/nvidia/fw.bin"),
                        url: Some("https://firmware.example.invalid/fw.bin"),
                    },
                    expect: Yields(scout_artifact(
                        "https://firmware.example.invalid/fw.bin",
                    )),
                },
                Case {
                    scenario: "filename becomes PXE public URL",
                    input: ScoutInput {
                        filename: Some("/opt/nico/firmware/nvidia/fw.bin"),
                        url: None,
                    },
                    expect: Yields(scout_artifact(
                        "http://carbide-pxe.forge:8080/public/firmware/nvidia/fw.bin",
                    )),
                },
                Case {
                    scenario: "filename and URL are missing",
                    input: ScoutInput {
                        filename: None,
                        url: None,
                    },
                    expect: FailsWith(ComparableError::Generic(
                        "scout firmware artifact has no filename or URL".to_string(),
                    )),
                },
                Case {
                    scenario: "same-prefix sibling is outside firmware directory",
                    input: ScoutInput {
                        filename: Some(
                            "/opt/nico/firmware2/nvidia/dgxh100/cx7/cx7.bin",
                        ),
                        url: None,
                    },
                    expect: FailsWith(ComparableError::Generic(
                        "firmware artifact path /opt/nico/firmware2/nvidia/dgxh100/cx7/cx7.bin is outside firmware directory /opt/nico/firmware"
                            .to_string(),
                    )),
                },
                Case {
                    scenario: "parent traversal is unsafe",
                    input: ScoutInput {
                        filename: Some("/opt/nico/firmware/../cx7.bin"),
                        url: None,
                    },
                    expect: FailsWith(ComparableError::Generic(
                        "firmware artifact path /opt/nico/firmware/../cx7.bin contains unsafe path components"
                            .to_string(),
                    )),
                },
            ],
            |ScoutInput { filename, url }| {
                resolve_scout_file_artifact(
                    PXE_PUBLIC_BASE_URL,
                    Path::new(FIRMWARE_DIRECTORY),
                    &file(filename, url),
                )
                .map_err(comparable_error)
            },
        );
    }

    fn firmware_with_files(files: Vec<FirmwareFileArtifact>) -> FirmwareEntry {
        FirmwareEntry {
            version: "1.0".to_string(),
            files,
            ..FirmwareEntry::default()
        }
    }

    fn file(filename: Option<&str>, url: Option<&str>) -> FirmwareFileArtifact {
        FirmwareFileArtifact {
            filename: filename.map(str::to_string),
            url: url.map(str::to_string),
            sha256: "abc123".to_string(),
        }
    }

    fn local_artifact(path: &str) -> ResolvedFirmwareArtifact {
        ResolvedFirmwareArtifact {
            local_path: PathBuf::from(path),
            source: ResolvedFirmwareArtifactSource::Local,
        }
    }

    fn scout_artifact(url: &str) -> FileArtifact {
        FileArtifact {
            url: url.to_string(),
            sha256: "abc123".to_string(),
        }
    }

    fn comparable_error(error: StateHandlerError) -> ComparableError {
        match error {
            StateHandlerError::GenericError(error) => ComparableError::Generic(error.to_string()),
            error => ComparableError::Other(error.to_string()),
        }
    }
}
