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
use rpc::forge::{AttestationCoverageEntry, AttesterSelectionMode, AttesterSet};

use super::*;
use crate::async_write::CapturedOutput;
use crate::attestation::Cmd as AttestationCmd;
use crate::attestation::spdm::Cmd as SpdmCmd;
use crate::cfg::cli_options::{CliCommand, CliOptions};

/// Two classes in the shape exploration derives, ordered as the grouped query
/// returns them, so the fixture is a response the server could actually send.
const UNPROFILED_CLASS: &str = "dell-inc_poweredge-r750";
const PROFILED_CLASS: &str = "nvidia_dgx-gb200";

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
        attester_sets: Vec::new(),
    }
}

/// The two sets the profiled class spans: seventy-one trays reporting eight
/// attesters and a single tray reporting seven, which is the drift the columns
/// exist for. Listed with the larger set first, so the table is shown sorting
/// the counts rather than echoing the order they arrive in.
fn attester_sets() -> Vec<AttesterSet> {
    vec![
        AttesterSet {
            digest: "1e05c4".to_string(),
            endpoints: 71,
            attesters: 8,
        },
        AttesterSet {
            digest: "9a7fb2".to_string(),
            endpoints: 1,
            attesters: 7,
        },
    ]
}

const HEADERS: [&str; 6] = [
    "HARDWARE CLASS",
    "EXPLORED ENDPOINTS",
    "ATTESTERS",
    "VARIANTS",
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
    let profiled = AttestationCoverageEntry {
        attester_sets: attester_sets(),
        ..entry(
            PROFILED_CLASS,
            72,
            AttestationCoverage::OwnProfile,
            Some(AttesterSelectionMode::Allowlist),
        )
    };
    let without_fallback = GetAttestationCoverageResponse {
        entries: vec![
            entry(UNPROFILED_CLASS, 2, AttestationCoverage::NoProfile, None),
            profiled,
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
            vec![
                UNPROFILED_CLASS,
                "2",
                "",
                "0",
                "no",
                "nothing: no profile for this class and no any fallback"
            ],
            // Two sets of differing size, so the class has no single attester
            // count and the cell has to report both.
            vec![
                PROFILED_CLASS,
                "72",
                "7, 8",
                "2",
                "yes",
                "its own profile (allowlist)"
            ],
            vec![
                NO_CLASS_RECORDED,
                "1",
                "",
                "0",
                "n/a",
                "nothing: no class recorded and no any profile"
            ],
            vec![
                ANY_HARDWARE_CLASS,
                NOT_APPLICABLE,
                NOT_APPLICABLE,
                NOT_APPLICABLE,
                "no",
                "nothing: no any fallback is stored"
            ],
        ]
    );

    // Storing `any` covers the class with no profile of its own and the
    // endpoints carrying no class at all, which are the two rows the table
    // reported as covered by nothing.
    let with_fallback = GetAttestationCoverageResponse {
        entries: vec![
            entry(
                UNPROFILED_CLASS,
                2,
                AttestationCoverage::AnyFallback,
                Some(AttesterSelectionMode::All),
            ),
            without_fallback.entries[1].clone(),
            entry(
                "",
                1,
                AttestationCoverage::AnyFallback,
                Some(AttesterSelectionMode::All),
            ),
        ],
        any_profile_mode: Some(AttesterSelectionMode::All.into()),
    };

    let (result, output) = execute(with_fallback.clone(), OutputFormat::AsciiTable).await;
    result.unwrap();
    assert_eq!(
        rows(&output),
        [
            HEADERS.to_vec(),
            vec![UNPROFILED_CLASS, "2", "", "0", "no", "any (all)"],
            vec![
                PROFILED_CLASS,
                "72",
                "7, 8",
                "2",
                "yes",
                "its own profile (allowlist)"
            ],
            vec![NO_CLASS_RECORDED, "1", "", "0", "n/a", "any (all)"],
            vec![
                ANY_HARDWARE_CLASS,
                NOT_APPLICABLE,
                NOT_APPLICABLE,
                NOT_APPLICABLE,
                "yes",
                "its own profile (all)"
            ],
        ]
    );

    // Serialized, the class of the endpoints carrying none is absent rather
    // than the label the table substitutes, and so is the count of a row that
    // is not keyed to hardware. The per-set endpoint counts are here rather
    // than in the table, where 71 against 1 is what names the outlier.
    let (result, output) = execute(with_fallback, OutputFormat::Json).await;
    result.unwrap();
    let reported: serde_json::Value = serde_json::from_slice(&output).unwrap();
    assert_eq!(
        reported[1]["attester_sets"],
        serde_json::json!([
            {"digest": "1e05c4", "endpoints": 71, "attesters": 8},
            {"digest": "9a7fb2", "endpoints": 1, "attesters": 7},
        ])
    );
    assert_eq!(
        reported[2],
        serde_json::json!({
            "hardware_class": null,
            "explored_endpoints": 1,
            "attester_sets": [],
            "own_profile": "n/a",
            "would_use": "any (all)",
        })
    );
    assert_eq!(
        reported[3],
        serde_json::json!({
            "hardware_class": ANY_HARDWARE_CLASS,
            "explored_endpoints": null,
            "attester_sets": null,
            "own_profile": "yes",
            "would_use": "its own profile (all)",
        })
    );
}
