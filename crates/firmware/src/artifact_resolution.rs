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

use std::path::{Path, PathBuf};

use eyre::{Result, eyre};
use model::firmware::FirmwareEntry;

use crate::artifact_cache::firmware_cache_filename;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ResolvedFirmwareArtifact {
    pub local_path: PathBuf,
    pub source: ResolvedFirmwareArtifactSource,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ResolvedFirmwareArtifactSource {
    Remote { url: String, sha256: String },
    Local,
}

pub fn resolve_files_firmware_artifact(
    firmware_cache_directory: &Path,
    firmware: &FirmwareEntry,
    pos: u32,
) -> Result<Option<ResolvedFirmwareArtifact>> {
    if firmware.files.is_empty() {
        return Ok(None);
    }

    let index = usize::try_from(pos).unwrap_or(usize::MAX);
    let artifact = firmware.files.get(index).ok_or_else(|| {
        eyre!(
            "firmware version {} has no files[] artifact at index {}",
            firmware.version,
            pos
        )
    })?;

    let url = artifact
        .url
        .as_deref()
        .map(str::trim)
        .filter(|url| !url.is_empty());

    let filename = artifact
        .filename
        .as_deref()
        .map(str::trim)
        .filter(|filename| !filename.is_empty());

    let local_path = if let Some(url) = url {
        firmware_cache_filename(firmware_cache_directory, url).ok_or_else(|| {
            eyre!(
                "firmware version {} files[] artifact at index {} URL does not include a filename",
                firmware.version,
                pos
            )
        })?
    } else if let Some(filename) = filename {
        PathBuf::from(filename)
    } else {
        return Err(eyre!(
            "firmware version {} files[] artifact at index {} has no filename or URL",
            firmware.version,
            pos
        ));
    };

    let source = match url {
        Some(url) => ResolvedFirmwareArtifactSource::Remote {
            url: url.to_owned(),
            sha256: artifact.sha256.clone(),
        },
        None => ResolvedFirmwareArtifactSource::Local,
    };

    Ok(Some(ResolvedFirmwareArtifact { local_path, source }))
}

#[cfg(test)]
mod tests {
    use carbide_test_support::Outcome::*;
    use carbide_test_support::{Case, check_cases};
    use model::firmware::FirmwareFileArtifact;

    use super::*;

    const CACHE_DIRECTORY: &str = "/mnt/persistence/fw/download-cache";

    struct ResolutionInput {
        files: Vec<FirmwareFileArtifact>,
        pos: u32,
    }

    #[test]
    fn resolve_files_firmware_artifact_cases() {
        let remote_url = "https://firmware.example.invalid/path/fw.bin";
        let second_url = "https://firmware.example.invalid/second.bin";

        check_cases(
            [
                Case {
                    scenario: "no files",
                    input: ResolutionInput {
                        files: Vec::new(),
                        pos: 0,
                    },
                    expect: Yields(None),
                },
                Case {
                    scenario: "URL only",
                    input: ResolutionInput {
                        files: vec![file(None, Some(remote_url), "abc123")],
                        pos: 0,
                    },
                    expect: Yields(Some(remote_artifact(remote_url, "abc123"))),
                },
                Case {
                    scenario: "filename only",
                    input: ResolutionInput {
                        files: vec![file(
                            Some("/opt/carbide/firmware/fw.bin"),
                            None,
                            "abc123",
                        )],
                        pos: 0,
                    },
                    expect: Yields(Some(local_artifact("/opt/carbide/firmware/fw.bin"))),
                },
                Case {
                    scenario: "URL takes precedence over filename",
                    input: ResolutionInput {
                        files: vec![file(
                            Some("/opt/carbide/firmware/local.bin"),
                            Some(remote_url),
                            "abc123",
                        )],
                        pos: 0,
                    },
                    expect: Yields(Some(remote_artifact(remote_url, "abc123"))),
                },
                Case {
                    scenario: "requested index selects matching artifact",
                    input: ResolutionInput {
                        files: vec![
                            file(
                                Some("/opt/carbide/firmware/first.bin"),
                                Some("https://firmware.example.invalid/first.bin"),
                                "first-sha",
                            ),
                            file(
                                Some("/opt/carbide/firmware/second.bin"),
                                Some(second_url),
                                "second-sha",
                            ),
                        ],
                        pos: 1,
                    },
                    expect: Yields(Some(remote_artifact(second_url, "second-sha"))),
                },
                Case {
                    scenario: "requested index is out of range",
                    input: ResolutionInput {
                        files: vec![file(
                            Some("/opt/carbide/firmware/fw.bin"),
                            None,
                            "abc123",
                        )],
                        pos: 1,
                    },
                    expect: FailsWith(
                        "firmware version 1.0 has no files[] artifact at index 1".to_string(),
                    ),
                },
                Case {
                    scenario: "URL has no filename",
                    input: ResolutionInput {
                        files: vec![file(
                            None,
                            Some("https://firmware.example.invalid/"),
                            "abc123",
                        )],
                        pos: 0,
                    },
                    expect: FailsWith(
                        "firmware version 1.0 files[] artifact at index 0 URL does not include a filename"
                            .to_string(),
                    ),
                },
                Case {
                    scenario: "filename and URL are missing",
                    input: ResolutionInput {
                        files: vec![file(None, None, "abc123")],
                        pos: 0,
                    },
                    expect: FailsWith(
                        "firmware version 1.0 files[] artifact at index 0 has no filename or URL"
                            .to_string(),
                    ),
                },
                Case {
                    scenario: "surrounding URL whitespace is trimmed",
                    input: ResolutionInput {
                        files: vec![file(
                            None,
                            Some("  https://firmware.example.invalid/trimmed.bin \n"),
                            "abc123",
                        )],
                        pos: 0,
                    },
                    expect: Yields(Some(remote_artifact(
                        "https://firmware.example.invalid/trimmed.bin",
                        "abc123",
                    ))),
                },
                Case {
                    scenario: "surrounding filename whitespace is trimmed",
                    input: ResolutionInput {
                        files: vec![file(
                            Some(" \t/opt/carbide/firmware/trimmed.bin  "),
                            None,
                            "abc123",
                        )],
                        pos: 0,
                    },
                    expect: Yields(Some(local_artifact(
                        "/opt/carbide/firmware/trimmed.bin",
                    ))),
                },
                Case {
                    scenario: "blank filename and URL are missing",
                    input: ResolutionInput {
                        files: vec![file(Some(" \t "), Some(" \n "), "abc123")],
                        pos: 0,
                    },
                    expect: FailsWith(
                        "firmware version 1.0 files[] artifact at index 0 has no filename or URL"
                            .to_string(),
                    ),
                },
            ],
            |ResolutionInput { files, pos }| {
                resolve_files_firmware_artifact(
                    Path::new(CACHE_DIRECTORY),
                    &firmware_with_files(files),
                    pos,
                )
                .map_err(|error| error.to_string())
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

    fn file(filename: Option<&str>, url: Option<&str>, sha256: &str) -> FirmwareFileArtifact {
        FirmwareFileArtifact {
            filename: filename.map(str::to_string),
            url: url.map(str::to_string),
            sha256: sha256.to_string(),
        }
    }

    fn remote_artifact(url: &str, sha256: &str) -> ResolvedFirmwareArtifact {
        ResolvedFirmwareArtifact {
            local_path: firmware_cache_filename(Path::new(CACHE_DIRECTORY), url).unwrap(),
            source: ResolvedFirmwareArtifactSource::Remote {
                url: url.to_string(),
                sha256: sha256.to_string(),
            },
        }
    }

    fn local_artifact(path: &str) -> ResolvedFirmwareArtifact {
        ResolvedFirmwareArtifact {
            local_path: PathBuf::from(path),
            source: ResolvedFirmwareArtifactSource::Local,
        }
    }
}
