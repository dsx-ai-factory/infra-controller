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

use clap::Parser;
use model::site_explorer::UNRECOGNIZED_HARDWARE_CLASS;
use rpc::forge::{AttestationCoverageEntry, AttesterSelectionMode};

use super::*;
use crate::async_write::CapturedOutput;
use crate::attestation::Cmd as AttestationCmd;
use crate::attestation::spdm::Cmd as SpdmCmd;
use crate::cfg::cli_options::{CliCommand, CliOptions};

struct FakeClient(GetAttestationCoverageResponse);

impl CoverageClient for FakeClient {
    async fn coverage(&self) -> Result<GetAttestationCoverageResponse, Status> {
        Ok(self.0.clone())
    }
}

/// Parses through the public command path, so the test covers what an operator
/// can actually type.
fn parse() -> Args {
    let options = CliOptions::try_parse_from(["nico-admin-cli", "attestation", "spdm", "coverage"])
        .expect("the coverage command parses");
    let Some(CliCommand::Attestation(AttestationCmd::Spdm(SpdmCmd::Coverage(args)))) =
        options.commands
    else {
        panic!("expected the public attestation coverage command path");
    };
    args
}

async fn execute(
    coverage: GetAttestationCoverageResponse,
    format: OutputFormat,
) -> (CarbideCliResult<()>, Vec<u8>) {
    let mut captured = CapturedOutput::new();
    let result = parse()
        .execute(&FakeClient(coverage), format, captured.writer())
        .await;
    (result, captured.into_bytes().await)
}

fn rows(output: &[u8]) -> Vec<Vec<String>> {
    String::from_utf8(output.to_vec())
        .expect("the table is text")
        .lines()
        .filter(|line| line.starts_with('|'))
        .map(|line| {
            line.trim_matches('|')
                .split('|')
                .map(|cell| cell.trim().to_string())
                .collect()
        })
        .collect()
}

fn entry(
    hardware_class: &str,
    endpoints: i32,
    coverage: AttestationCoverage,
    mode: Option<AttesterSelectionMode>,
) -> AttestationCoverageEntry {
    AttestationCoverageEntry {
        hardware_class: hardware_class.to_string(),
        endpoints,
        coverage: coverage.into(),
        mode: mode.map(Into::into),
    }
}

const HEADERS: [&str; 4] = [
    "HARDWARE CLASS",
    "EXPLORED ENDPOINTS",
    "OWN PROFILE",
    "WOULD USE",
];

/// Which classes a site has is written down nowhere else, and whether a class
/// is attested at all depends on a fallback that is not keyed to hardware. The
/// two phases are the same site before and after an `any` profile is stored,
/// which is what moves a class from covered by nothing to covered by the
/// fallback.
#[tokio::test]
async fn the_coverage_table_names_what_would_attest_each_class_the_site_has() {
    let without_fallback = GetAttestationCoverageResponse {
        entries: vec![
            entry(
                "Gb200",
                72,
                AttestationCoverage::OwnProfile,
                Some(AttesterSelectionMode::Allowlist),
            ),
            entry("LenovoGb300", 18, AttestationCoverage::NoProfile, None),
            entry(
                UNRECOGNIZED_HARDWARE_CLASS,
                2,
                AttestationCoverage::ClassUnrecognized,
                None,
            ),
            entry("", 1, AttestationCoverage::ClassNotRecorded, None),
        ],
        any_profile_mode: None,
    };

    let (result, output) = execute(without_fallback.clone(), OutputFormat::AsciiTable).await;
    result.unwrap();
    assert_eq!(
        rows(&output),
        [
            HEADERS.to_vec(),
            vec!["Gb200", "72", "yes", "its own profile (allowlist)"],
            vec![
                "LenovoGb300",
                "18",
                "no",
                "nothing: no profile for this class and no any fallback"
            ],
            vec![
                UNRECOGNIZED_HARDWARE_CLASS,
                "2",
                "n/a",
                "nothing: exploration did not recognise this hardware"
            ],
            vec![
                NO_CLASS_RECORDED,
                "1",
                "n/a",
                "nothing: no class recorded; explore these endpoints again"
            ],
            vec![
                ANY_HARDWARE_CLASS,
                NOT_APPLICABLE,
                "no",
                "nothing: no any fallback is stored"
            ],
        ]
    );

    // Storing `any` covers the classes with no profile of their own, and the
    // hardware exploration did not recognise, but not the endpoints carrying no
    // class at all: resolution reads the class off the endpoint, so there is
    // nothing there to resolve.
    let with_fallback = GetAttestationCoverageResponse {
        entries: vec![
            without_fallback.entries[0].clone(),
            entry(
                "LenovoGb300",
                18,
                AttestationCoverage::AnyFallback,
                Some(AttesterSelectionMode::All),
            ),
            entry(
                UNRECOGNIZED_HARDWARE_CLASS,
                2,
                AttestationCoverage::AnyFallback,
                Some(AttesterSelectionMode::All),
            ),
            without_fallback.entries[3].clone(),
        ],
        any_profile_mode: Some(AttesterSelectionMode::All.into()),
    };

    let (result, output) = execute(with_fallback.clone(), OutputFormat::AsciiTable).await;
    result.unwrap();
    assert_eq!(
        rows(&output),
        [
            HEADERS.to_vec(),
            vec!["Gb200", "72", "yes", "its own profile (allowlist)"],
            vec!["LenovoGb300", "18", "no", "any (all)"],
            vec![UNRECOGNIZED_HARDWARE_CLASS, "2", "no", "any (all)"],
            vec![
                NO_CLASS_RECORDED,
                "1",
                "n/a",
                "nothing: no class recorded; explore these endpoints again"
            ],
            vec![
                ANY_HARDWARE_CLASS,
                NOT_APPLICABLE,
                "yes",
                "its own profile (all)"
            ],
        ]
    );

    // Serialized, the class of the endpoints carrying none is absent rather
    // than the label the table substitutes, and so is the count of a row that
    // is not keyed to hardware.
    let (result, output) = execute(with_fallback, OutputFormat::Json).await;
    result.unwrap();
    let reported: serde_json::Value = serde_json::from_slice(&output).unwrap();
    assert_eq!(
        reported[3],
        serde_json::json!({
            "hardware_class": null,
            "explored_endpoints": 1,
            "own_profile": "n/a",
            "would_use": "nothing: no class recorded; explore these endpoints again",
        })
    );
    assert_eq!(
        reported[4],
        serde_json::json!({
            "hardware_class": ANY_HARDWARE_CLASS,
            "explored_endpoints": null,
            "own_profile": "yes",
            "would_use": "its own profile (all)",
        })
    );
}
