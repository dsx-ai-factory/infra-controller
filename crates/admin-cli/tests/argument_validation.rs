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

use std::ffi::OsStr;
use std::io::ErrorKind;
use std::net::TcpListener;
use std::process::Stdio;
use std::time::Duration;

use tokio::process::Command;

/// Verifies malformed invocations show Clap diagnostics and syntax guidance before contacting
/// Core and that an invalid Pod family or unacknowledged replacement leaves the requested
/// output path absent.
#[tokio::test]
async fn rejected_arguments_exit_before_contacting_core() {
    const ALLOCATION_USAGE: &str =
        "Usage: nico-admin-cli --cloud-unsafe-op <USERNAME> instance allocate";
    const REPLACEMENT_USAGE: &str =
        "Usage: nico-admin-cli --cloud-unsafe-op <USERNAME> instance update-extension-services";
    const SERVICE_SELECTION_USAGE: &str = "--extension-service <SERVICE_JSON>";

    struct Case {
        scenario: &'static str,
        args: &'static [&'static str],
        diagnostic: &'static str,
        usage: &'static str,
    }

    let output_cases = [
        // The Helm-only family must be rejected before the output path is opened.
        Case {
            scenario: "Pod registration rejects a service VPC family",
            args: &[
                "extension-service",
                "create",
                "--name=pod",
                "--type=kubernetes-pod",
                "--tenant-organization-id=fds34511233a",
                "--data={}",
                "--service-vpc-family=ipv4",
            ],
            diagnostic: "--service-vpc-family",
            usage: "Usage: nico-admin-cli extension-service create",
        },
        // A valid detach mode still needs the root acknowledgement before opening the output.
        Case {
            scenario: "service replacement requires the root unsafe-operation acknowledgement",
            args: &[
                "instance",
                "update-extension-services",
                "--instance=12345678-1234-5678-90ab-cdef01234567",
                "--detach-all",
            ],
            diagnostic: "supply --cloud-unsafe-op <USERNAME> before instance",
            usage: REPLACEMENT_USAGE,
        },
    ];
    let cases = [
        // Clap rejects family names outside the two supported values.
        Case {
            scenario: "registration update rejects an unknown service VPC family",
            args: &[
                "extension-service",
                "update",
                "--id=12345678-1234-5678-90ab-cdef01234567",
                "--data={}",
                "--service-vpc-family=unspecified",
            ],
            diagnostic: "invalid value 'unspecified'",
            usage: "--service-vpc-family <SERVICE_VPC_FAMILY>",
        },
        // A selection needs the exact attachable version rather than just its V1 number.
        Case {
            scenario: "attachment rejects an incomplete version",
            args: &[
                "instance",
                "update-extension-services",
                "--instance=12345678-1234-5678-90ab-cdef01234567",
                r#"--extension-service={"service_id":"abcdef01-2345-6789-abcd-ef0123456789","version":"V1"}"#,
            ],
            diagnostic: "Failed to parse version as ConfigVersion",
            usage: SERVICE_SELECTION_USAGE,
        },
        // One service interface accepts one VPC selection.
        Case {
            scenario: "attachment rejects two selected VPCs",
            args: &[
                "instance",
                "update-extension-services",
                "--instance=12345678-1234-5678-90ab-cdef01234567",
                r#"--extension-service={"service_id":"abcdef01-2345-6789-abcd-ef0123456789","version":"V1-T1720000000000000","service_vpc_ids":["12345678-1234-5678-90ab-cdef01234567","abcdef01-2345-6789-abcd-ef0123456789"]}"#,
            ],
            diagnostic: "at most one service VPC may be selected",
            usage: SERVICE_SELECTION_USAGE,
        },
        // Reject a misspelled VPC field before it can be mistaken for an omitted selection.
        Case {
            scenario: "attachment rejects an unknown VPC field",
            args: &[
                "instance",
                "update-extension-services",
                "--instance=12345678-1234-5678-90ab-cdef01234567",
                r#"--extension-service={"service_id":"abcdef01-2345-6789-abcd-ef0123456789","version":"V1-T1720000000000000","service_vpc_id":"12345678-1234-5678-90ab-cdef01234567"}"#,
            ],
            diagnostic: "unknown field `service_vpc_id`",
            usage: SERVICE_SELECTION_USAGE,
        },
        // The CLI requires an explicit replacement so an empty invocation cannot detach services.
        Case {
            scenario: "instance service replacement requires a mode",
            args: &[
                "instance",
                "update-extension-services",
                "--instance=12345678-1234-5678-90ab-cdef01234567",
            ],
            diagnostic: "the following required arguments were not provided",
            usage: REPLACEMENT_USAGE,
        },
        // Different versions still select the same service ID on allocation.
        Case {
            scenario: "allocation rejects different versions of one extension service",
            args: &[
                "--cloud-unsafe-op=service-vpc-test",
                "instance",
                "allocate",
                "--prefix-name=eth0",
                "--vpc-prefix-id=23072a44-0eb6-4867-9c2d-c65130b7134c",
                r#"--extension-service={"service_id":"12345678-1234-5678-90ab-cdef01234567","version":"V1-T1720000000000000"}"#,
                r#"--extension-service={"service_id":"12345678-1234-5678-90ab-cdef01234567","version":"V2-T1720000000000001"}"#,
            ],
            diagnostic: "each --extension-service must specify a different service ID",
            usage: ALLOCATION_USAGE,
        },
        // An exact duplicate can otherwise collapse into an unchanged attachment set in Core.
        Case {
            scenario: "replacement rejects an identical service selection",
            args: &[
                "--cloud-unsafe-op=service-vpc-test",
                "instance",
                "update-extension-services",
                "--instance=23072a44-0eb6-4867-9c2d-c65130b7134c",
                r#"--extension-service={"service_id":"12345678-1234-5678-90ab-cdef01234567","version":"V1-T1720000000000000"}"#,
                r#"--extension-service={"service_id":"12345678-1234-5678-90ab-cdef01234567","version":"V1-T1720000000000000"}"#,
            ],
            diagnostic: "each --extension-service must specify a different service ID",
            usage: REPLACEMENT_USAGE,
        },
        // An unsupported output format must not mutate Core and then emit YAML.
        Case {
            scenario: "prefix append rejects CSV before mutation",
            args: &[
                "--format=csv",
                "--cloud-unsafe-op=prefix-validation",
                "network-segment",
                "add-prefix",
                "--id=12345678-1234-5678-90ab-cdef01234567",
                "--prefix=2001:db8::/64",
            ],
            diagnostic: "--format csv is not supported for prefix append",
            usage: "Usage: nico-admin-cli network-segment add-prefix",
        },
        // Cross-field errors must take Clap's exit path, before the unsafe-op guard or RPC.
        Case {
            scenario: "prefix append requires its IPv4 gateway",
            args: &[
                "--cloud-unsafe-op=prefix-validation",
                "network-segment",
                "add-prefix",
                "--id=12345678-1234-5678-90ab-cdef01234567",
                "--prefix=192.0.2.0/24",
            ],
            diagnostic: "--gateway is required for an IPv4 --prefix",
            usage: "Usage: nico-admin-cli network-segment add-prefix",
        },
        // A valid IPv4 gateway is still an invalid option for an IPv6 prefix.
        Case {
            scenario: "prefix append rejects an IPv6 gateway option",
            args: &[
                "--cloud-unsafe-op=prefix-validation",
                "network-segment",
                "add-prefix",
                "--id=12345678-1234-5678-90ab-cdef01234567",
                "--prefix=2001:db8::/64",
                "--gateway=192.0.2.1",
            ],
            diagnostic: "--gateway is not accepted for an IPv6 --prefix",
            usage: "Usage: nico-admin-cli network-segment add-prefix",
        },
        // Supplying the required gateway must not hide an IPv4/relay conflict.
        Case {
            scenario: "prefix append rejects a relay on IPv4",
            args: &[
                "--cloud-unsafe-op=prefix-validation",
                "network-segment",
                "add-prefix",
                "--id=12345678-1234-5678-90ab-cdef01234567",
                "--prefix=192.0.2.0/24",
                "--gateway=192.0.2.1",
                "--dhcpv6-link-address=2001:db8:ffff::1",
            ],
            diagnostic: "--dhcpv6-link-address requires an IPv6 --prefix",
            usage: "Usage: nico-admin-cli network-segment add-prefix",
        },
        Case {
            scenario: "excess IPv6 values fail before allocation",
            args: &[
                // Without `--cloud-unsafe-op`, that guard would reject the command
                // even if argument validation were missing.
                "--cloud-unsafe-op=ipv6-validation-test",
                "instance",
                "allocate",
                "--prefix-name=ipv6-test",
                "--vpc-prefix-id=00000000-0000-0000-0000-000000000001",
                "--ipv6-vpc-prefix-id=00000000-0000-0000-0000-000000000002",
                "--ipv6-vpc-prefix-id=00000000-0000-0000-0000-000000000003",
            ],
            diagnostic: "--ipv6-vpc-prefix-id has 2 values but --vpc-prefix-id has 1",
            usage: ALLOCATION_USAGE,
        },
        Case {
            scenario: "unconfirmed machine erase fails the command",
            args: &["expected-machine", "erase"],
            diagnostic: "--confirm is required to erase all expected machines",
            usage: "Usage: nico-admin-cli expected-machine erase",
        },
        Case {
            scenario: "unconfirmed switch erase fails the command",
            args: &["expected-switch", "erase"],
            diagnostic: "--confirm is required to erase all expected switches",
            usage: "Usage: nico-admin-cli expected-switch erase",
        },
        Case {
            scenario: "unconfirmed rack erase fails the command",
            args: &["expected-rack", "erase"],
            diagnostic: "--confirm is required to erase all expected racks",
            usage: "Usage: nico-admin-cli expected-rack erase",
        },
        Case {
            scenario: "duplicate machine DPU serials fail the command",
            args: &[
                "expected-machine",
                "patch",
                "--bmc-mac-address",
                "00:11:22:33:44:55",
                "--fallback-dpu-serial-number",
                "DPU-001",
                "--fallback-dpu-serial-number",
                "DPU-001",
            ],
            diagnostic: "duplicate --fallback-dpu-serial-number values; supply each serial number only once",
            usage: "Usage: nico-admin-cli expected-machine patch",
        },
        Case {
            scenario: "machine delete requires a selector",
            args: &["expected-machine", "delete"],
            diagnostic: "must specify either a BMC MAC address or --id",
            usage: "Usage: nico-admin-cli expected-machine delete",
        },
        Case {
            scenario: "machine delete rejects conflicting selectors",
            args: &[
                "expected-machine",
                "delete",
                "00:11:22:33:44:55",
                "--id",
                "12345678-1234-5678-90ab-cdef01234567",
            ],
            diagnostic: "cannot specify both a BMC MAC address and --id; provide only one",
            usage: "Usage: nico-admin-cli expected-machine delete",
        },
        Case {
            scenario: "machine show rejects conflicting selectors",
            args: &[
                "expected-machine",
                "show",
                "00:11:22:33:44:55",
                "--id",
                "12345678-1234-5678-90ab-cdef01234567",
            ],
            diagnostic: "cannot specify both a BMC MAC address and --id; provide only one",
            usage: "Usage: nico-admin-cli expected-machine show",
        },
        Case {
            scenario: "switch delete requires a selector",
            args: &["expected-switch", "delete"],
            diagnostic: "must specify either a BMC MAC address or --id",
            usage: "Usage: nico-admin-cli expected-switch delete",
        },
        Case {
            scenario: "switch delete rejects conflicting selectors",
            args: &[
                "expected-switch",
                "delete",
                "00:11:22:33:44:55",
                "--id",
                "12345678-1234-5678-90ab-cdef01234567",
            ],
            diagnostic: "cannot specify both a BMC MAC address and --id; provide only one",
            usage: "Usage: nico-admin-cli expected-switch delete",
        },
        Case {
            scenario: "switch show rejects conflicting selectors",
            args: &[
                "expected-switch",
                "show",
                "00:11:22:33:44:55",
                "--id",
                "12345678-1234-5678-90ab-cdef01234567",
            ],
            diagnostic: "cannot specify both a BMC MAC address and --id; provide only one",
            usage: "Usage: nico-admin-cli expected-switch show",
        },
        Case {
            scenario: "shelf delete requires a selector",
            args: &["expected-power-shelf", "delete"],
            diagnostic: "must specify either a BMC MAC address or --id",
            usage: "Usage: nico-admin-cli expected-power-shelf delete",
        },
        Case {
            scenario: "shelf delete rejects conflicting selectors",
            args: &[
                "expected-power-shelf",
                "delete",
                "00:11:22:33:44:55",
                "--id",
                "12345678-1234-5678-90ab-cdef01234567",
            ],
            diagnostic: "cannot specify both a BMC MAC address and --id; provide only one",
            usage: "Usage: nico-admin-cli expected-power-shelf delete",
        },
        Case {
            scenario: "shelf show rejects conflicting selectors",
            args: &[
                "expected-power-shelf",
                "show",
                "00:11:22:33:44:55",
                "--id",
                "12345678-1234-5678-90ab-cdef01234567",
            ],
            diagnostic: "cannot specify both a BMC MAC address and --id; provide only one",
            usage: "Usage: nico-admin-cli expected-power-shelf show",
        },
        Case {
            scenario: "unsupported shelf hostname rejects the entire update",
            args: &[
                "expected-power-shelf",
                "update",
                "--bmc-mac-address",
                "00:11:22:33:44:55",
                "--shelf-serial-number",
                "SHELF-002",
                "--host_name",
                "power-shelf-01",
            ],
            diagnostic: "--host_name is not supported for expected power shelf updates; remove it from the command",
            usage: "Usage: nico-admin-cli expected-power-shelf update",
        },
        Case {
            scenario: "shelf erase requires explicit confirmation",
            args: &["expected-power-shelf", "erase"],
            diagnostic: "--confirm is required to erase all expected power shelves",
            usage: "Usage: nico-admin-cli expected-power-shelf erase",
        },
        Case {
            scenario: "shelf update requires a selector",
            args: &[
                "expected-power-shelf",
                "update",
                "--shelf-serial-number",
                "SHELF-002",
            ],
            diagnostic: "must specify either --bmc-mac-address or --id",
            usage: "Usage: nico-admin-cli expected-power-shelf update",
        },
        Case {
            scenario: "shelf update rejects conflicting selectors",
            args: &[
                "expected-power-shelf",
                "update",
                "--id",
                "12345678-1234-5678-90ab-cdef01234567",
                "--bmc-mac-address",
                "00:11:22:33:44:55",
                "--shelf-serial-number",
                "SHELF-002",
            ],
            diagnostic: "cannot specify both --bmc-mac-address and --id; provide only one",
            usage: "Usage: nico-admin-cli expected-power-shelf update",
        },
        Case {
            scenario: "switch update requires a selector",
            args: &[
                "expected-switch",
                "update",
                "--switch-serial-number",
                "SWITCH-003",
            ],
            diagnostic: "must specify either --bmc-mac-address or --id",
            usage: "Usage: nico-admin-cli expected-switch update",
        },
        Case {
            scenario: "switch add requires an NVOS port",
            args: &[
                "expected-switch",
                "add",
                "--bmc-mac-address",
                "00:11:22:33:44:55",
                "--bmc-username",
                "admin",
                "--bmc-password",
                "test-password",
                "--switch-serial-number",
                "SWITCH-004",
            ],
            diagnostic: "--nvos-interface <MAC[,primary]>",
            usage: "Usage: nico-admin-cli expected-switch add",
        },
        Case {
            scenario: "switch update rejects conflicting selectors",
            args: &[
                "expected-switch",
                "update",
                "--id",
                "12345678-1234-5678-90ab-cdef01234567",
                "--bmc-mac-address",
                "00:11:22:33:44:55",
                "--switch-serial-number",
                "SWITCH-003",
            ],
            diagnostic: "cannot specify both --bmc-mac-address and --id; provide only one",
            usage: "Usage: nico-admin-cli expected-switch update",
        },
    ];

    // A missing path distinguishes validation from any attempt to create the output file.
    let directory = tempfile::tempdir().expect("create temp directory for CLI output");
    let output_path = directory.path().join("cli-output");
    for (case, check_output) in output_cases
        .into_iter()
        .map(|case| (case, true))
        .chain(cases.into_iter().map(|case| (case, false)))
    {
        // Leave the private listener idle so an attempted Core connection remains observable.
        let listener = TcpListener::bind("127.0.0.1:0").expect("bind private Core listener");
        listener
            .set_nonblocking(true)
            .expect("make listener nonblocking");
        let api_url = format!("http://{}", listener.local_addr().unwrap());
        let output_args = check_output.then_some([OsStr::new("--output"), output_path.as_os_str()]);

        // Run the invalid invocation against the private listener.
        // Ignore per-user CLI config and proxy settings so they cannot fail
        // before argument validation runs.
        let mut child = Command::new(env!("CARGO_BIN_EXE_nico-admin-cli"))
            .env_clear()
            .args([
                "--api-url",
                &api_url,
                "--root-ca-path",
                "/unused/ca.crt",
                "--client-cert-path",
                "/unused/client.crt",
                "--client-key-path",
                "/unused/client.key",
            ])
            .args(output_args.into_iter().flatten())
            .args(case.args)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .kill_on_drop(true)
            .spawn()
            .expect("launch admin CLI");
        match tokio::time::timeout(Duration::from_secs(5), child.wait()).await {
            Ok(result) => {
                result.expect("wait for admin CLI");
            }
            Err(_) => {
                child.kill().await.expect("kill timed-out admin CLI");
                panic!(
                    "{}: argument validation did not finish within five seconds",
                    case.scenario
                );
            }
        }
        let output = child
            .wait_with_output()
            .await
            .expect("read admin CLI output");
        // Check the Clap exit path before looking for any output-file or connection side effects.
        let stderr = String::from_utf8(output.stderr).expect("CLI diagnostics are UTF-8");
        assert_eq!(output.status.code(), Some(2), "{}: {stderr}", case.scenario);
        assert!(
            stderr.contains(case.diagnostic),
            "{}: {stderr}",
            case.scenario
        );
        assert!(stderr.contains(case.usage), "{}: {stderr}", case.scenario);
        assert!(stderr.contains("--help"), "{}: {stderr}", case.scenario);
        assert!(output.stdout.is_empty(), "{}", case.scenario);
        assert!(
            !check_output || !output_path.exists(),
            "{}: rejected invocation created the output file",
            case.scenario
        );
        assert_eq!(
            listener
                .accept()
                .expect_err("rejected arguments must not contact Core")
                .kind(),
            ErrorKind::WouldBlock,
            "{}",
            case.scenario,
        );
    }
}
