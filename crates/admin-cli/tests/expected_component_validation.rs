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

use std::io::ErrorKind;
use std::net::TcpListener;
use std::process::Stdio;
use std::time::Duration;

use tokio::process::Command;

#[tokio::test]
async fn rejected_expected_component_arguments_exit_before_contacting_core() {
    struct Case {
        scenario: &'static str,
        args: &'static [&'static str],
        diagnostic: &'static str,
        usage: &'static str,
    }

    for case in [
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
    ] {
        let listener = TcpListener::bind("127.0.0.1:0").expect("bind private Core listener");
        listener
            .set_nonblocking(true)
            .expect("make listener nonblocking");
        let api_url = format!("http://{}", listener.local_addr().unwrap());
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
