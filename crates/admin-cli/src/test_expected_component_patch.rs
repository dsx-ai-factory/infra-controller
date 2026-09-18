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

use std::convert::Infallible;
use std::panic::AssertUnwindSafe;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use clap::Parser;
use futures::{FutureExt, stream};
use http_body_util::combinators::UnsyncBoxBody;
use http_body_util::{BodyExt, Empty, StreamBody};
use hyper::body::{Bytes, Frame, Incoming};
use hyper::server::conn::http2;
use hyper::service::service_fn;
use hyper::{Request, Response, header};
use hyper_util::rt::{TokioExecutor, TokioIo};
use prost::Message;
use rpc::admin_cli::OutputFormat;
use rpc::forge;
use rpc::forge_api_client::{EXPECTED_SWITCH_UPDATE_MASK_HEADER, ForgeApiClient};
use rpc::forge_tls_client::{ApiConfig, ForgeClientConfig};
use tokio::net::TcpListener;
use tonic::Code;

use crate::cfg::cli_options::{CliCommand, CliOptions, SortField};
use crate::cfg::dispatch::Dispatch;
use crate::cfg::runtime::{RuntimeConfig, RuntimeContext};
use crate::errors::{CarbideCliError, CarbideCliResult};
use crate::rpc::ApiClient;

const ID: &str = "12345678-1234-5678-90ab-cdef01234567";
const MAC: &str = "00:11:22:33:44:55";
const CORE_ERROR: &str = "request rejected by Core";

#[tokio::test]
async fn machine_flags_select_only_supplied_fields() {
    struct Case {
        scenario: &'static str,
        args: Vec<&'static str>,
        methods: &'static [&'static str],
        paths: &'static [&'static str],
        expected: forge::ExpectedMachine,
    }
    for case in [
        Case {
            scenario: "ID selection sends false and empty resets without reading the record",
            args: vec![
                "expected-machine",
                "patch",
                "--id",
                ID,
                "--dpf-enabled",
                "false",
                "--default_pause_ingestion_and_poweron",
                "false",
                "--bmc-retain-credentials",
                "false",
                "--disable-lockdown",
                "false",
                "--interfaces",
                "[]",
                "--dpu-policy",
                "unspecified",
                "--bmc-ip-allocation",
                "auto",
                "--meta-name",
                "",
                "--meta-description",
                "",
            ],
            methods: &["PatchExpectedMachine"],
            paths: &[
                "is_dpf_enabled",
                "default_pause_ingestion_and_poweron",
                "bmc_retain_credentials",
                "host_lifecycle_profile.disable_lockdown",
                "host_nics",
                "dpu_mode",
                "bmc_ip_allocation",
                "metadata.name",
                "metadata.description",
            ],
            expected: forge::ExpectedMachine {
                id: Some(rpc_id()),
                is_dpf_enabled: Some(false),
                default_pause_ingestion_and_poweron: Some(false),
                bmc_retain_credentials: Some(false),
                host_lifecycle_profile: Some(forge::HostLifecycleProfile {
                    disable_lockdown: Some(false),
                }),
                dpu_mode: Some(forge::DpuMode::Unspecified as i32),
                bmc_ip_allocation: Some(forge::BmcIpAllocationType::Auto as i32),
                metadata: Some(forge::Metadata::default()),
                ..Default::default()
            },
        },
        Case {
            scenario: "interface replacement sends omitted policies and explicit resets to Core",
            args: vec![
                "expected-machine",
                "patch",
                "--id",
                ID,
                "--interfaces",
                r#"[{"mac_address":"00:11:22:33:44:66"},{"mac_address":"00:11:22:33:44:77","role":"unspecified","ip_allocation":"unspecified"}]"#,
            ],
            methods: &["PatchExpectedMachine"],
            paths: &["host_nics"],
            expected: forge::ExpectedMachine {
                id: Some(rpc_id()),
                host_nics: vec![
                    forge::ExpectedInterface {
                        mac_address: "00:11:22:33:44:66".to_string(),
                        ..Default::default()
                    },
                    forge::ExpectedInterface {
                        mac_address: "00:11:22:33:44:77".to_string(),
                        role: Some(forge::ExpectedInterfaceRole::Unspecified as i32),
                        ip_allocation: Some(
                            forge::ExpectedInterfaceIpAllocation::Unspecified as i32,
                        ),
                        ..Default::default()
                    },
                ],
                ..Default::default()
            },
        },
        Case {
            scenario: "MAC lookup supplies only the ID, never stored credentials or metadata",
            args: vec![
                "expected-machine",
                "patch",
                "--bmc-mac-address",
                MAC,
                "--sku-id",
                "DGX-H100-640GB",
            ],
            methods: &["GetExpectedMachine", "PatchExpectedMachine"],
            paths: &["sku_id"],
            expected: forge::ExpectedMachine {
                id: Some(rpc_id()),
                sku_id: Some("DGX-H100-640GB".to_string()),
                ..Default::default()
            },
        },
    ] {
        let (result, requests) = dispatch(&case.args, Code::Ok).await;
        result.unwrap_or_else(|error| panic!("{}: {error}", case.scenario));
        assert_methods(&requests, case.methods);
        let request: forge::PatchExpectedMachineRequest = requests.last().unwrap().decode();
        assert_paths(request.update_mask.unwrap().paths, case.paths);
        assert_eq!(
            request.expected_machine,
            Some(case.expected),
            "{}",
            case.scenario
        );
    }
}

#[tokio::test]
async fn machine_files_preserve_their_metadata_and_interface_selection_contract() {
    struct Case {
        scenario: &'static str,
        filename: &'static str,
        paths: &'static [&'static str],
    }
    for case in [
        Case {
            scenario: "omitted metadata clears its leaves while omitted interfaces preserve",
            filename: concat!(
                env!("CARGO_MANIFEST_DIR"),
                "/testdata/expected-machine-patch-omitted.json"
            ),
            paths: &[
                "bmc_username",
                "bmc_password",
                "chassis_serial_number",
                "metadata.name",
                "metadata.description",
                "metadata.labels",
            ],
        },
        Case {
            scenario: "null metadata clears its leaves and an empty interface list replaces",
            filename: concat!(
                env!("CARGO_MANIFEST_DIR"),
                "/testdata/expected-machine-patch-empty.json"
            ),
            paths: &[
                "bmc_username",
                "bmc_password",
                "chassis_serial_number",
                "metadata.name",
                "metadata.description",
                "metadata.labels",
                "host_nics",
            ],
        },
    ] {
        let (result, requests) = dispatch(
            &["expected-machine", "update", "--filename", case.filename],
            Code::Ok,
        )
        .await;
        result.unwrap_or_else(|error| panic!("{}: {error}", case.scenario));
        assert_methods(&requests, &["GetExpectedMachine", "PatchExpectedMachine"]);
        let request: forge::PatchExpectedMachineRequest = requests[1].decode();
        assert_paths(request.update_mask.unwrap().paths, case.paths);
        assert_eq!(
            request.expected_machine,
            Some(forge::ExpectedMachine {
                id: Some(rpc_id()),
                bmc_username: "file-user".to_string(),
                bmc_password: "file-password".to_string(),
                chassis_serial_number: "FILE-001".to_string(),
                metadata: Some(forge::Metadata::default()),
                ..Default::default()
            }),
            "{}",
            case.scenario
        );
    }
}

#[tokio::test]
async fn shelf_updates_select_supplied_values_without_replaying_lookup_fields() {
    struct Case {
        scenario: &'static str,
        args: &'static [&'static str],
        methods: &'static [&'static str],
        paths: &'static [&'static str],
        expected: forge::ExpectedPowerShelf,
    }
    for case in [
        Case {
            scenario: "serial by ID omits credentials and metadata without reading the record",
            args: &[
                "expected-power-shelf",
                "update",
                "--id",
                ID,
                "--shelf-serial-number",
                "SHELF-002",
            ],
            methods: &["PatchExpectedPowerShelf"],
            paths: &["shelf_serial_number"],
            expected: forge::ExpectedPowerShelf {
                expected_power_shelf_id: Some(rpc_id()),
                shelf_serial_number: "SHELF-002".to_string(),
                metadata: Some(forge::Metadata::default()),
                ..Default::default()
            },
        },
        Case {
            scenario: "MAC selection submits explicit false and empty metadata without replay",
            args: &[
                "expected-power-shelf",
                "update",
                "--bmc-mac-address",
                MAC,
                "--shelf-serial-number",
                "SHELF-002",
                "--bmc-retain-credentials",
                "false",
                "--meta-name",
                "",
            ],
            methods: &["GetExpectedPowerShelf", "PatchExpectedPowerShelf"],
            paths: &[
                "shelf_serial_number",
                "bmc_retain_credentials",
                "metadata.name",
            ],
            expected: forge::ExpectedPowerShelf {
                expected_power_shelf_id: Some(rpc_id()),
                bmc_mac_address: MAC.to_string(),
                shelf_serial_number: "SHELF-002".to_string(),
                bmc_retain_credentials: Some(false),
                metadata: Some(forge::Metadata::default()),
                ..Default::default()
            },
        },
    ] {
        let (result, requests) = dispatch(case.args, Code::Ok).await;
        result.unwrap_or_else(|error| panic!("{}: {error}", case.scenario));
        assert_methods(&requests, case.methods);
        let request: forge::PatchExpectedPowerShelfRequest = requests.last().unwrap().decode();
        assert_paths(request.update_mask.unwrap().paths, case.paths);
        assert_eq!(
            request.expected_power_shelf,
            Some(case.expected),
            "{}",
            case.scenario
        );
    }
}

#[tokio::test]
async fn switch_nvos_update_does_not_replay_bmc_credentials_or_select_empty_metadata() {
    let (result, requests) = dispatch(
        &[
            "expected-switch",
            "update",
            "--bmc-mac-address",
            MAC,
            "--nvos-username",
            "new-nvos-user",
            "--nvos-password",
            "new-nvos-password",
            "--meta-name",
            "",
            "--meta-description",
            "",
        ],
        Code::Ok,
    )
    .await;
    result.unwrap();
    assert_methods(&requests, &["GetExpectedSwitch", "PatchExpectedSwitch"]);
    let request: forge::PatchExpectedSwitchRequest = requests[1].decode();
    assert_paths(
        request.update_mask.unwrap().paths,
        &["nvos_username", "nvos_password"],
    );
    assert_eq!(
        request.expected_switch,
        Some(forge::ExpectedSwitch {
            expected_switch_id: Some(rpc_id()),
            bmc_mac_address: MAC.to_string(),
            nvos_username: Some("new-nvos-user".to_string()),
            nvos_password: Some("new-nvos-password".to_string()),
            metadata: Some(forge::Metadata::default()),
            ..Default::default()
        })
    );
}

#[tokio::test]
async fn unsupported_machine_patches_use_the_original_read_merge_update() {
    struct Case {
        scenario: &'static str,
        args: &'static [&'static str],
        patch_reply: PatchReply,
        lookup_id: Option<&'static str>,
        methods: &'static [&'static str],
        selector: forge::ExpectedMachineRequest,
        expected: forge::ExpectedMachine,
    }
    for case in [
        Case {
            scenario: "flag patch by ID merges stored fields after Unimplemented",
            args: &[
                "expected-machine",
                "patch",
                "--id",
                ID,
                "--sku-id",
                "DGX-H100-640GB",
                "--meta-name",
                "",
                "--interfaces",
                "[]",
            ],
            patch_reply: PatchReply::Grpc(Code::Unimplemented),
            lookup_id: Some(ID),
            methods: &[
                "PatchExpectedMachine",
                "GetExpectedMachine",
                "UpdateExpectedMachine",
            ],
            selector: forge::ExpectedMachineRequest {
                id: Some(rpc_id()),
                ..Default::default()
            },
            expected: forge::ExpectedMachine {
                sku_id: Some("DGX-H100-640GB".to_string()),
                metadata: Some(forge::Metadata {
                    name: String::new(),
                    ..stored_metadata()
                }),
                host_nics: Vec::new(),
                replace_host_nics: true,
                #[allow(deprecated)]
                dpf_enabled: true,
                is_dpf_enabled: None,
                ..stored_machine()
            },
        },
        Case {
            scenario: "file update preserves omission rules after an old site's HTTP 403",
            args: &[
                "expected-machine",
                "update",
                "--filename",
                concat!(
                    env!("CARGO_MANIFEST_DIR"),
                    "/testdata/expected-machine-patch-omitted.json"
                ),
            ],
            patch_reply: PatchReply::HttpForbidden,
            lookup_id: Some(ID),
            methods: &[
                "GetExpectedMachine",
                "PatchExpectedMachine",
                "GetExpectedMachine",
                "UpdateExpectedMachine",
            ],
            selector: forge::ExpectedMachineRequest {
                bmc_mac_address: MAC.to_string(),
                id: None,
            },
            expected: forge::ExpectedMachine {
                bmc_username: "file-user".to_string(),
                bmc_password: "file-password".to_string(),
                chassis_serial_number: "FILE-001".to_string(),
                metadata: Some(forge::Metadata::default()),
                #[allow(deprecated)]
                dpf_enabled: true,
                is_dpf_enabled: None,
                ..stored_machine()
            },
        },
        Case {
            scenario: "MAC lookup without an ID uses the original merged update without PATCH",
            args: &[
                "expected-machine",
                "patch",
                "--bmc-mac-address",
                MAC,
                "--sku-id",
                "DGX-H100-640GB",
            ],
            patch_reply: PatchReply::Grpc(Code::Ok),
            lookup_id: None,
            methods: &[
                "GetExpectedMachine",
                "GetExpectedMachine",
                "UpdateExpectedMachine",
            ],
            selector: forge::ExpectedMachineRequest {
                bmc_mac_address: MAC.to_string(),
                id: None,
            },
            expected: forge::ExpectedMachine {
                id: None,
                sku_id: Some("DGX-H100-640GB".to_string()),
                #[allow(deprecated)]
                dpf_enabled: true,
                is_dpf_enabled: None,
                ..stored_machine()
            },
        },
    ] {
        let (result, requests) =
            dispatch_with_replies(case.args, case.patch_reply, Code::Ok, case.lookup_id).await;
        result.unwrap_or_else(|error| panic!("{}: {error}", case.scenario));
        assert_methods(&requests, case.methods);
        let lookup: forge::ExpectedMachineRequest = requests[requests.len() - 2].decode();
        assert_eq!(lookup, case.selector, "{}", case.scenario);
        let update: forge::ExpectedMachine = requests.last().unwrap().decode();
        assert_eq!(update, case.expected, "{}", case.scenario);
    }
}

#[tokio::test]
async fn unsupported_shelf_patch_preserves_the_legacy_request_and_result() {
    for legacy_code in [Code::Ok, Code::PermissionDenied] {
        let (result, requests) = dispatch_with_replies(
            &[
                "expected-power-shelf",
                "update",
                "--bmc-mac-address",
                MAC,
                "--shelf-serial-number",
                "SHELF-002",
                "--bmc-retain-credentials",
                "false",
                "--meta-name",
                "replacement-name",
            ],
            PatchReply::Grpc(Code::Unimplemented),
            legacy_code,
            Some(ID),
        )
        .await;
        if legacy_code == Code::Ok {
            result.unwrap();
        } else {
            assert_core_error(result, legacy_code);
        }
        assert_methods(
            &requests,
            &[
                "GetExpectedPowerShelf",
                "PatchExpectedPowerShelf",
                "UpdateExpectedPowerShelf",
            ],
        );
        let update: forge::ExpectedPowerShelf = requests[2].decode();
        assert_eq!(
            update,
            forge::ExpectedPowerShelf {
                bmc_mac_address: MAC.to_string(),
                shelf_serial_number: "SHELF-002".to_string(),
                bmc_retain_credentials: Some(false),
                metadata: Some(forge::Metadata {
                    name: "replacement-name".to_string(),
                    ..Default::default()
                }),
                ..Default::default()
            }
        );
    }
}

#[tokio::test]
async fn unsupported_switch_patch_keeps_the_legacy_typed_mask_header() {
    let (result, requests) = dispatch_with_replies(
        &[
            "expected-switch",
            "update",
            "--id",
            ID,
            "--nvos-username",
            "new-nvos-user",
            "--meta-name",
            "",
        ],
        PatchReply::Grpc(Code::PermissionDenied),
        Code::Ok,
        Some(ID),
    )
    .await;
    result.unwrap();
    assert_methods(&requests, &["PatchExpectedSwitch", "UpdateExpectedSwitch"]);
    let update: forge::ExpectedSwitch = requests[1].decode();
    assert_eq!(
        update,
        forge::ExpectedSwitch {
            expected_switch_id: Some(rpc_id()),
            nvos_username: Some("new-nvos-user".to_string()),
            metadata: Some(forge::Metadata::default()),
            ..Default::default()
        }
    );
    assert_eq!(
        requests[1].headers[EXPECTED_SWITCH_UPDATE_MASK_HEADER],
        "nvos_username,nvos_password"
    );
}

#[tokio::test]
async fn shelf_lookup_without_an_id_uses_the_original_mac_update() {
    let (result, requests) = dispatch_with_replies(
        &[
            "expected-power-shelf",
            "update",
            "--bmc-mac-address",
            MAC,
            "--shelf-serial-number",
            "SHELF-002",
            "--bmc-retain-credentials",
            "false",
        ],
        PatchReply::Grpc(Code::Ok),
        Code::Ok,
        None,
    )
    .await;
    result.unwrap();
    assert_methods(
        &requests,
        &["GetExpectedPowerShelf", "UpdateExpectedPowerShelf"],
    );
    let update: forge::ExpectedPowerShelf = requests[1].decode();
    assert_eq!(
        update,
        forge::ExpectedPowerShelf {
            bmc_mac_address: MAC.to_string(),
            shelf_serial_number: "SHELF-002".to_string(),
            bmc_retain_credentials: Some(false),
            metadata: Some(forge::Metadata::default()),
            ..Default::default()
        }
    );
}

#[tokio::test]
async fn switch_lookup_without_an_id_uses_the_original_mac_update_and_mask() {
    let (result, requests) = dispatch_with_replies(
        &[
            "expected-switch",
            "update",
            "--bmc-mac-address",
            MAC,
            "--nvos-username",
            "new-nvos-user",
            "--nvos-password",
            "new-nvos-password",
        ],
        PatchReply::Grpc(Code::Ok),
        Code::Ok,
        None,
    )
    .await;
    result.unwrap();
    assert_methods(&requests, &["GetExpectedSwitch", "UpdateExpectedSwitch"]);
    let update: forge::ExpectedSwitch = requests[1].decode();
    assert_eq!(
        update,
        forge::ExpectedSwitch {
            bmc_mac_address: MAC.to_string(),
            nvos_username: Some("new-nvos-user".to_string()),
            nvos_password: Some("new-nvos-password".to_string()),
            metadata: Some(forge::Metadata::default()),
            ..Default::default()
        }
    );
    assert_eq!(
        requests[1].headers[EXPECTED_SWITCH_UPDATE_MASK_HEADER],
        "nvos_username,nvos_password"
    );
}

#[tokio::test]
async fn core_patch_errors_propagate_without_legacy_fallback() {
    struct Case {
        args: &'static [&'static str],
        method: &'static str,
        code: Code,
    }
    for case in [
        Case {
            args: &[
                "expected-machine",
                "patch",
                "--id",
                ID,
                "--sku-id",
                "DGX-H100-640GB",
            ],
            method: "PatchExpectedMachine",
            code: Code::Unavailable,
        },
        Case {
            args: &[
                "expected-power-shelf",
                "update",
                "--id",
                ID,
                "--bmc-username",
                "user",
                "--bmc-password",
                "",
            ],
            method: "PatchExpectedPowerShelf",
            code: Code::InvalidArgument,
        },
        Case {
            args: &[
                "expected-switch",
                "update",
                "--id",
                ID,
                "--switch-serial-number",
                "SWITCH-003",
            ],
            method: "PatchExpectedSwitch",
            code: Code::NotFound,
        },
    ] {
        let (result, requests) = dispatch(case.args, case.code).await;
        assert_core_error(result, case.code);
        assert_methods(&requests, &[case.method]);
    }
}

fn assert_core_error(result: CarbideCliResult<()>, code: Code) {
    let error = result.expect_err("Core failure must fail the command");
    let CarbideCliError::EyreReport(report) = &error else {
        panic!("unexpected command error: {error}");
    };
    let status = report
        .downcast_ref::<tonic::Status>()
        .expect("Core status remains in the command's error chain");
    assert_eq!(status.code(), code);
    assert_eq!(status.message(), CORE_ERROR);
}

#[derive(Clone, Copy)]
enum PatchReply {
    Grpc(Code),
    HttpForbidden,
}

#[derive(Debug)]
struct RecordedRequest {
    method: String,
    headers: hyper::HeaderMap,
    payload: Bytes,
}

impl RecordedRequest {
    fn decode<T: Message + Default>(&self) -> T {
        T::decode(self.payload.clone()).expect("recorded request decodes")
    }
}

fn assert_methods(requests: &[RecordedRequest], expected: &[&str]) {
    assert_eq!(
        requests
            .iter()
            .map(|request| request.method.as_str())
            .collect::<Vec<_>>(),
        expected,
    );
}

fn assert_paths(mut actual: Vec<String>, expected: &[&str]) {
    let mut expected = expected.to_vec();
    actual.sort();
    expected.sort();
    assert_eq!(actual, expected);
}

fn rpc_id() -> rpc::common::Uuid {
    rpc::common::Uuid {
        value: ID.to_string(),
    }
}

async fn dispatch(args: &[&str], code: Code) -> (CarbideCliResult<()>, Vec<RecordedRequest>) {
    dispatch_with_replies(args, PatchReply::Grpc(code), Code::Ok, Some(ID)).await
}

async fn dispatch_with_replies(
    args: &[&str],
    patch_reply: PatchReply,
    legacy_code: Code,
    lookup_id: Option<&'static str>,
) -> (CarbideCliResult<()>, Vec<RecordedRequest>) {
    let options =
        CliOptions::try_parse_from(std::iter::once("nico-admin-cli").chain(args.iter().copied()))
            .expect("public expected-component command parses");
    let command = options.commands.expect("command is present");
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let request_timeout = Duration::from_secs(5);
    let client_config = ForgeClientConfig {
        request_timeout: Some(request_timeout),
        ..Default::default()
    };
    let ctx = RuntimeContext {
        api_client: ApiClient(ForgeApiClient::new(&ApiConfig::new(
            &format!("http://{address}"),
            &client_config,
        ))),
        config: RuntimeConfig {
            format: OutputFormat::AsciiTable,
            request_timeout: client_config.request_timeout,
            page_size: 25,
            extended: false,
            cloud_unsafe_op: None,
            sort_by: SortField::PrimaryId,
        },
        output_file: Box::new(tokio::io::sink()),
    };
    let requests = Arc::new(Mutex::new(Vec::new()));
    let recorded = Arc::clone(&requests);
    let server = tokio::spawn(async move {
        let (connection, _) = listener.accept().await.unwrap();
        http2::Builder::new(TokioExecutor::new())
            .serve_connection(
                TokioIo::new(connection),
                service_fn(move |request| {
                    mock_request(
                        request,
                        Arc::clone(&recorded),
                        patch_reply,
                        legacy_code,
                        lookup_id,
                    )
                }),
            )
            .await
            .expect("mock serves the expected-component connection");
    });
    let result = AssertUnwindSafe(tokio::time::timeout(request_timeout, async move {
        match command {
            CliCommand::ExpectedMachine(command) => command.dispatch(ctx).await,
            CliCommand::ExpectedPowerShelf(command) => command.dispatch(ctx).await,
            CliCommand::ExpectedSwitch(command) => command.dispatch(ctx).await,
            _ => panic!("expected an expected-component command"),
        }
    }))
    .catch_unwind()
    .await;
    // Join before asserting so even failed dispatch leaves no listener behind.
    server.abort();
    if let Err(error) = server.await {
        assert!(
            error.is_cancelled(),
            "mock expected-component server failed: {error}"
        );
    }
    let result = result
        .expect("command dispatch did not panic")
        .expect("command dispatch finishes within five seconds");
    let requests = std::mem::take(&mut *requests.lock().unwrap());
    (result, requests)
}

async fn mock_request(
    request: Request<Incoming>,
    requests: Arc<Mutex<Vec<RecordedRequest>>>,
    patch_reply: PatchReply,
    legacy_code: Code,
    lookup_id: Option<&'static str>,
) -> Result<Response<UnsyncBoxBody<Bytes, Infallible>>, Infallible> {
    let method = request
        .uri()
        .path()
        .strip_prefix("/forge.Forge/")
        .unwrap()
        .to_string();
    if method == "Version" {
        return Ok(grpc_reply(
            forge::BuildInfo::default().encode_to_vec(),
            Code::Ok,
        ));
    }
    let (parts, body) = request.into_parts();
    let body = body.collect().await.unwrap().to_bytes();
    assert_eq!(
        body.first(),
        Some(&0),
        "request uses an uncompressed gRPC frame"
    );
    let recorded = RecordedRequest {
        method,
        headers: parts.headers,
        payload: body.slice(5..),
    };
    // Lookup responses contain fields a read-modify-write client would replay.
    // PATCH must copy only the ID; the legacy machine update must merge them.
    let response = match recorded.method.as_str() {
        "GetExpectedMachine" => {
            let request: forge::ExpectedMachineRequest = recorded.decode();
            let expected = if request.id.is_some() {
                forge::ExpectedMachineRequest {
                    id: Some(rpc_id()),
                    ..Default::default()
                }
            } else {
                forge::ExpectedMachineRequest {
                    bmc_mac_address: MAC.to_string(),
                    id: None,
                }
            };
            assert_eq!(request, expected);
            grpc_reply(
                forge::ExpectedMachine {
                    id: lookup_id.map(|id| rpc::common::Uuid {
                        value: id.to_string(),
                    }),
                    ..stored_machine()
                }
                .encode_to_vec(),
                Code::Ok,
            )
        }
        "GetExpectedPowerShelf" => {
            let request: forge::ExpectedPowerShelfRequest = recorded.decode();
            assert_eq!(
                request,
                forge::ExpectedPowerShelfRequest {
                    bmc_mac_address: MAC.to_string(),
                    expected_power_shelf_id: None,
                }
            );
            grpc_reply(
                forge::ExpectedPowerShelf {
                    expected_power_shelf_id: lookup_id.map(|id| rpc::common::Uuid {
                        value: id.to_string(),
                    }),
                    bmc_mac_address: MAC.to_string(),
                    bmc_username: "stored-bmc-user".to_string(),
                    bmc_password: "stored-bmc-password".to_string(),
                    shelf_serial_number: "STORED-002".to_string(),
                    metadata: Some(stored_metadata()),
                    ..Default::default()
                }
                .encode_to_vec(),
                Code::Ok,
            )
        }
        "GetExpectedSwitch" => {
            let request: forge::ExpectedSwitchRequest = recorded.decode();
            assert_eq!(
                request,
                forge::ExpectedSwitchRequest {
                    bmc_mac_address: MAC.to_string(),
                    expected_switch_id: None,
                }
            );
            grpc_reply(
                forge::ExpectedSwitch {
                    expected_switch_id: lookup_id.map(|id| rpc::common::Uuid {
                        value: id.to_string(),
                    }),
                    bmc_mac_address: MAC.to_string(),
                    bmc_username: "stored-bmc-user".to_string(),
                    bmc_password: "stored-bmc-password".to_string(),
                    nvos_username: Some("stored-nvos-user".to_string()),
                    nvos_password: Some("stored-nvos-password".to_string()),
                    switch_serial_number: "STORED-003".to_string(),
                    metadata: Some(stored_metadata()),
                    nvos_mac_addresses: vec!["00:11:22:33:44:77".to_string()],
                    ..Default::default()
                }
                .encode_to_vec(),
                Code::Ok,
            )
        }
        "PatchExpectedMachine" | "PatchExpectedPowerShelf" | "PatchExpectedSwitch" => {
            match patch_reply {
                PatchReply::Grpc(code) => grpc_reply(Vec::new(), code),
                PatchReply::HttpForbidden => Response::builder()
                    .status(hyper::StatusCode::FORBIDDEN)
                    .body(Empty::<Bytes>::new().boxed_unsync())
                    .unwrap(),
            }
        }
        "UpdateExpectedMachine" | "UpdateExpectedPowerShelf" | "UpdateExpectedSwitch" => {
            grpc_reply(Vec::new(), legacy_code)
        }
        method => panic!("unexpected mock Forge method: {method}"),
    };
    requests.lock().unwrap().push(recorded);
    Ok(response)
}

fn stored_machine() -> forge::ExpectedMachine {
    forge::ExpectedMachine {
        id: Some(rpc_id()),
        bmc_mac_address: MAC.to_string(),
        bmc_username: "stored-bmc-user".to_string(),
        bmc_password: "stored-bmc-password".to_string(),
        chassis_serial_number: "STORED-001".to_string(),
        sku_id: Some("stored-sku".to_string()),
        metadata: Some(stored_metadata()),
        dpu_mode: Some(forge::DpuMode::NoDpu as i32),
        is_dpf_enabled: Some(true),
        host_nics: vec![forge::ExpectedInterface {
            mac_address: "00:11:22:33:44:66".to_string(),
            ..Default::default()
        }],
        ..Default::default()
    }
}

fn stored_metadata() -> forge::Metadata {
    forge::Metadata {
        name: "stored-name".to_string(),
        description: "stored-description".to_string(),
        labels: vec![forge::Label {
            key: "stored-label".to_string(),
            value: Some("stored-value".to_string()),
        }],
    }
}

fn grpc_reply(payload: Vec<u8>, code: Code) -> Response<UnsyncBoxBody<Bytes, Infallible>> {
    let mut data = vec![0];
    data.extend_from_slice(&u32::try_from(payload.len()).unwrap().to_be_bytes());
    data.extend_from_slice(&payload);
    let mut trailers = hyper::HeaderMap::new();
    trailers.insert("grpc-status", (code as i32).to_string().parse().unwrap());
    if code != Code::Ok {
        trailers.insert("grpc-message", header::HeaderValue::from_static(CORE_ERROR));
    }
    let body = StreamBody::new(stream::iter([
        Ok::<_, Infallible>(Frame::data(Bytes::from(data))),
        Ok(Frame::trailers(trailers)),
    ]))
    .boxed_unsync();
    Response::builder()
        .header(header::CONTENT_TYPE, "application/grpc+tonic")
        .body(body)
        .unwrap()
}
