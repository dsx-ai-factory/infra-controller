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

use std::cell::RefCell;

use clap::Parser;

use super::*;
use crate::async_write::CapturedOutput;
use crate::attestation::Cmd as AttestationCmd;
use crate::attestation::spdm::Cmd as SpdmCmd;
use crate::cfg::cli_options::{CliCommand, CliOptions};

const VERSION: &str = "V7-T1789080000000000";

#[derive(Debug, PartialEq)]
enum Request {
    List,
    Get(GetAttestationProfileRequest),
    Create(CreateAttestationProfileRequest),
    Update(UpdateAttestationProfileRequest),
    Delete(DeleteAttestationProfileRequest),
}

#[derive(Default)]
struct FakeClient {
    profiles: Vec<AttestationProfile>,
    requests: RefCell<Vec<Request>>,
}

impl FakeClient {
    /// The profile a read or a write reports back. The commands print what the
    /// server returned rather than what was asked for.
    fn reported(&self) -> AttestationProfile {
        self.profiles.first().cloned().unwrap_or_default()
    }
}

impl ProfileClient for FakeClient {
    async fn list(&self) -> Result<Vec<AttestationProfile>, Status> {
        self.requests.borrow_mut().push(Request::List);
        Ok(self.profiles.clone())
    }

    async fn get(
        &self,
        request: GetAttestationProfileRequest,
    ) -> Result<AttestationProfile, Status> {
        self.requests.borrow_mut().push(Request::Get(request));
        Ok(self.reported())
    }

    async fn create(
        &self,
        request: CreateAttestationProfileRequest,
    ) -> Result<AttestationProfile, Status> {
        self.requests.borrow_mut().push(Request::Create(request));
        Ok(self.reported())
    }

    async fn update(
        &self,
        request: UpdateAttestationProfileRequest,
    ) -> Result<AttestationProfile, Status> {
        self.requests.borrow_mut().push(Request::Update(request));
        Ok(self.reported())
    }

    async fn delete(&self, request: DeleteAttestationProfileRequest) -> Result<(), Status> {
        self.requests.borrow_mut().push(Request::Delete(request));
        Ok(())
    }
}

/// Parses through the public command path, so the tests cover what an operator
/// can actually type.
fn parse(args: &[&str]) -> Cmd {
    let argv: Vec<_> = ["nico-admin-cli", "attestation", "spdm", "profile"]
        .into_iter()
        .chain(args.iter().copied())
        .collect();
    let options = CliOptions::try_parse_from(argv).expect("the profile command parses");
    let Some(CliCommand::Attestation(AttestationCmd::Spdm(SpdmCmd::Profile(command)))) =
        options.commands
    else {
        panic!("expected the public attestation profile command path");
    };
    command
}

async fn execute(
    args: &[&str],
    client: &FakeClient,
    format: OutputFormat,
) -> (CarbideCliResult<()>, Vec<u8>) {
    let mut captured = CapturedOutput::new();
    let output = captured.writer();
    let result = match parse(args) {
        Cmd::List(args) => args.execute(client, format, output).await,
        Cmd::Get(args) => args.execute(client, format, output).await,
        Cmd::Create(args) => args.execute(client, format, output).await,
        Cmd::Update(args) => args.execute(client, format, output).await,
        Cmd::Delete(args) => args.execute(client).await,
    };
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

fn pattern(pattern: component_id_match::Pattern) -> ComponentIdMatch {
    ComponentIdMatch {
        pattern: Some(pattern),
    }
}

fn profile(
    hardware_class: &str,
    mode: AttesterSelectionMode,
    updated_by: &str,
) -> AttestationProfile {
    AttestationProfile {
        hardware_class: hardware_class.to_string(),
        version: VERSION.to_string(),
        selection: Some(AttesterSelection {
            mode: mode.into(),
            component_ids: match mode {
                AttesterSelectionMode::Allowlist | AttesterSelectionMode::Denylist => vec![
                    pattern(component_id_match::Pattern::Exact("VERA_CPU_0".to_string())),
                    pattern(component_id_match::Pattern::Prefix(
                        "HGX_IRoT_GPU_".to_string(),
                    )),
                ],
                _ => Vec::new(),
            },
        }),
        updated_at: Some(Default::default()),
        updated_by: updated_by.to_string(),
    }
}

/// The guard exists because `any` supplies the policy for every class without
/// one of its own, so these two edits stop attesting hardware that is not
/// named on the command line. Every other edit reaches the server unguarded.
#[tokio::test]
async fn only_edits_that_stop_attesting_unprofiled_hardware_require_force() {
    for (args, reaches_server) in [
        (&["update", "any", "--mode", "none"][..], false),
        (&["update", "any", "--mode", "none", "--force"][..], true),
        // `any` keeps attesting, so there is nothing to confirm.
        (&["update", "any", "--mode", "all"][..], true),
        // One class stops attesting, and it is the one named.
        (&["update", "Gb200", "--mode", "none"][..], true),
        (&["delete", "any"][..], false),
        (&["delete", "any", "--force"][..], true),
        (&["delete", "Gb200"][..], true),
    ] {
        let client = FakeClient::default();
        let (result, _) = execute(args, &client, OutputFormat::AsciiTable).await;

        assert_eq!(
            !client.requests.borrow().is_empty(),
            reaches_server,
            "{args:?}"
        );
        match result {
            Ok(()) => assert!(reaches_server, "{args:?} was expected to be refused"),
            Err(error) => {
                let error = error.to_string();
                assert!(error.contains("--force"), "{args:?}: {error}");
            }
        }
    }
}

/// Resolution reads the class off an explored endpoint, so a profile keyed to a
/// class exploration never records could never apply to a machine. The server
/// refuses it too; refusing here spends no round trip and names the classes
/// that would have worked.
#[tokio::test]
async fn create_refuses_a_hardware_class_exploration_never_records() {
    let client = FakeClient::default();
    let (result, _) = execute(
        &["create", "Gb2000", "--mode", "all"],
        &client,
        OutputFormat::AsciiTable,
    )
    .await;

    let error = result
        .expect_err("a misspelled class is not a profile key")
        .to_string();
    assert!(error.contains("Gb2000"), "{error}");
    assert!(error.contains("Gb200"), "the accepted classes: {error}");
    assert!(client.requests.borrow().is_empty(), "{error}");

    for hardware_class in ["Gb200", ANY_HARDWARE_CLASS] {
        let client = FakeClient::default();
        let (result, _) = execute(
            &["create", hardware_class, "--mode", "all"],
            &client,
            OutputFormat::AsciiTable,
        )
        .await;
        result.unwrap_or_else(|error| panic!("{hardware_class} is a profile key: {error}"));
        assert_eq!(client.requests.borrow().len(), 1, "{hardware_class}");
    }
}

/// A command line does not record the order two flags were interleaved in, so
/// the patterns are sent `--exact` first and then `--prefix`, each in the order
/// given.
#[tokio::test]
async fn the_selection_flags_build_the_selection_that_is_stored() {
    let client = FakeClient::default();
    let (result, _) = execute(
        &[
            "update",
            "Gb200",
            "--mode",
            "allowlist",
            "--prefix",
            "HGX_IRoT_GPU_",
            "--exact",
            "VERA_CPU_0",
            "--prefix",
            "HGX_BMC_",
            "--if-version-match",
            VERSION,
        ],
        &client,
        OutputFormat::AsciiTable,
    )
    .await;
    result.unwrap();

    assert_eq!(
        *client.requests.borrow(),
        [Request::Update(UpdateAttestationProfileRequest {
            hardware_class: "Gb200".to_string(),
            selection: Some(AttesterSelection {
                mode: AttesterSelectionMode::Allowlist.into(),
                component_ids: vec![
                    pattern(component_id_match::Pattern::Exact("VERA_CPU_0".to_string())),
                    pattern(component_id_match::Pattern::Prefix(
                        "HGX_IRoT_GPU_".to_string()
                    )),
                    pattern(component_id_match::Pattern::Prefix("HGX_BMC_".to_string())),
                ],
            }),
            if_version_match: Some(VERSION.to_string()),
        })]
    );
}

/// The table carries the version because that is what `--if-version-match`
/// takes, and the patterns because a mode alone does not say what is attested.
/// A mode that takes no patterns leaves that cell empty rather than inventing a
/// placeholder.
#[tokio::test]
async fn the_profile_table_shows_each_profile_with_the_version_to_match() {
    let client = FakeClient {
        profiles: vec![
            profile(
                "Gb200",
                AttesterSelectionMode::Allowlist,
                "operator@example.com",
            ),
            profile(ANY_HARDWARE_CLASS, AttesterSelectionMode::All, ""),
        ],
        ..Default::default()
    };

    let (result, output) = execute(&["list"], &client, OutputFormat::AsciiTable).await;
    result.unwrap();
    assert_eq!(
        rows(&output),
        [
            vec![
                "HARDWARE CLASS",
                "MODE",
                "COMPONENT IDS",
                "VERSION",
                "UPDATED BY"
            ],
            vec![
                "Gb200",
                "allowlist",
                "exact:VERA_CPU_0, prefix:HGX_IRoT_GPU_",
                VERSION,
                "operator@example.com"
            ],
            vec![ANY_HARDWARE_CLASS, "all", "", VERSION, ""],
        ]
    );
    assert_eq!(*client.requests.borrow(), [Request::List]);

    // Reading one profile prints the same columns, and prints it as one profile
    // rather than as a list of one.
    let (result, output) = execute(&["get", "Gb200"], &client, OutputFormat::Json).await;
    result.unwrap();
    let reported: serde_json::Value = serde_json::from_slice(&output).unwrap();
    assert_eq!(
        reported,
        serde_json::json!({
            "hardware_class": "Gb200",
            // Named as `--mode` spells it, not as the wire integer.
            "mode": "allowlist",
            "component_ids": ["exact:VERA_CPU_0", "prefix:HGX_IRoT_GPU_"],
            "version": VERSION,
            "updated_at": "1970-01-01T00:00:00Z",
            "updated_by": "operator@example.com",
        })
    );
}
