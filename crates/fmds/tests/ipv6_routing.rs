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

//! Exercise the production binary and kernel routing inside an isolated Docker
//! container, never the host network. Start the container with `--network=none`,
//! `--sysctl net.ipv6.conf.all.forwarding=1`, `NET_ADMIN` and `SYS_ADMIN`,
//! iproute2, curl, and util-linux installed, and `FMDS_ROUTING_TEST_ISOLATED=1`.
//! Invoke the test binary through `docker exec ... unshare --net ...` so PID 1
//! stays in the router namespace and FMDS runs in a separate service namespace.
//! No host network mounts or writable `/proc/sys` are needed.

use std::path::Path;
use std::process::Stdio;
use std::time::Duration;

use rpc::fmds::fmds_config_service_client::FmdsConfigServiceClient;
use rpc::fmds::{FmdsConfigUpdate, UpdateConfigRequest};
use tokio::process::{Child, Command};
use tokio::time::{Instant, sleep, timeout};
use tonic::transport::{Channel, Endpoint};

/// A tenant with no IPv4 address reaches FMDS through a router, and a process
/// restart repairs stale default routes without duplicating reply rules. Both
/// families use off-link clients absent from the service's main routing table,
/// so successful responses depend on production policy setup.
#[tokio::test]
#[ignore = "requires an isolated Docker container with NET_ADMIN, SYS_ADMIN, iproute2, curl and util-linux"]
async fn ipv6_tenant_metadata_survives_restart_and_repairs_stale_routes() {
    assert_eq!(
        std::env::var("FMDS_ROUTING_TEST_ISOLATED").as_deref(),
        Ok("1"),
        "explicit container opt-in is required"
    );
    assert!(
        Path::new("/.dockerenv").exists(),
        "this test must not change the host network"
    );
    assert_ne!(
        std::fs::read_link("/proc/self/ns/net").expect("service namespace"),
        std::fs::read_link("/proc/1/ns/net").expect("container router namespace"),
        "invoke the test binary through unshare --net inside the container"
    );
    timeout(Duration::from_secs(90), exercise_routed_metadata())
        .await
        .expect("routed metadata test deadline");
}

/// Use the container's preconfigured router without writing `/proc/sys`.
/// FMDS shares the driver's namespace, and the tenant namespace is child-owned.
async fn exercise_routed_metadata() {
    let router_pid = 1;
    assert_eq!(
        run(
            Some(router_pid),
            &["sysctl", "-n", "net.ipv6.conf.all.forwarding"]
        )
        .await,
        "1",
        "start the isolated container with --sysctl net.ipv6.conf.all.forwarding=1"
    );
    let mut tenant = network_namespace().await;
    let tenant_pid = tenant.id().expect("tenant PID");
    let router_id = router_pid.to_string();
    let tenant_id = tenant_pid.to_string();

    // Only HBN's stand-in has both families. The tenant link is IPv6-only.
    run(
        None,
        &[
            "sh",
            "-ec",
            r#"
                ip link set lo up
                ip link add fmds-test type veth peer name metadata-router netns "$1"
                ip link add router-tenant type veth peer name tenant-test
                ip link set router-tenant netns "$1"
                ip link set tenant-test netns "$2"
                ip link add fmds-stale type dummy
                ip link set fmds-stale up
                ip address add 192.0.2.2/30 dev fmds-stale
                ip address add fd00:7176::2/127 dev fmds-stale
            "#,
            "--",
            &router_id,
            &tenant_id,
        ],
    )
    .await;
    run(
        Some(router_pid),
        &[
            "sh",
            "-ec",
            r#"
            ip link set lo up
            ip link set metadata-router up
            ip address add 169.254.169.253/30 dev metadata-router
            ip address add fd00:76b9::255/127 dev metadata-router
            ip address replace 198.51.100.17/32 dev lo
            ip link set router-tenant up
            ip address add 2001:db8:7176::1/64 dev router-tenant
        "#,
        ],
    )
    .await;
    run(
        Some(tenant_pid),
        &[
            "sh",
            "-ec",
            r#"
            ip link set lo up
            ip link set tenant-test up
            ip address add 2001:db8:7176::2/64 dev tenant-test
            ip -6 route add fd00:76b9::254/128 via 2001:db8:7176::1 dev tenant-test
        "#,
        ],
    )
    .await;
    assert!(
        run(
            Some(tenant_pid),
            &["ip", "-4", "-o", "address", "show", "dev", "tenant-test"]
        )
        .await
        .is_empty()
    );
    for (attempt, hostname) in [
        // The first start must assign both addresses and create return routing.
        "tenant-before-restart",
        // Keep addresses/rules, but damage each default route before restarting.
        "tenant-after-restart",
    ]
    .into_iter()
    .enumerate()
    {
        if attempt > 0 {
            // A route on another attachment must be replaced, not accepted
            // merely because table 100 already contains a default route.
            run(
                None,
                &[
                    "sh",
                    "-ec",
                    r#"
                    ip -4 route replace default via 192.0.2.1 dev fmds-stale table 100
                    ip -6 route replace default via fd00:7176::3 dev fmds-stale table 100
                    "#,
                ],
            )
            .await;
            for family in ["-4", "-6"] {
                let routes = run(None, &["ip", family, "route", "show", "table", "100"]).await;
                assert!(
                    routes.contains("dev fmds-stale"),
                    "stale route fixture: {routes}"
                );
            }
        }
        let mut server = Command::new(env!("CARGO_BIN_EXE_carbide-fmds"))
            .args([
                "--interface-name",
                "fmds-test",
                "--interface-cidr-v6",
                "fd00:76b9::254/127",
                "--rest-address",
                "[::]:80",
                "--compatibility-rest-address",
                "0.0.0.0:7777",
                "--grpc-address",
                "127.0.0.1:50052",
                "--metrics-address",
                "127.0.0.1:8888",
            ])
            .stdout(Stdio::inherit())
            .stderr(Stdio::inherit())
            .kill_on_drop(true)
            .spawn()
            .expect("start production FMDS binary");
        let mut client = connect_fmds(&mut server).await;
        // Check after production setup: adding a main-table route must not
        // make the policy-route proof pass through an unintended return path.
        for (family, client_prefix) in [("-4", "198.51.100"), ("-6", "2001:db8:7176")] {
            let routes = run(None, &["ip", family, "route", "show", "table", "main"]).await;
            assert!(
                !routes.contains(client_prefix),
                "unexpected main route: {routes}"
            );
            assert!(
                !routes.lines().any(|line| line.starts_with("default ")),
                "unexpected main default: {routes}"
            );
        }
        // FMDS has now brought up its veth peer. DAD on the router and
        // tenant can finish independently of FMDS's own readiness check.
        // Observe both namespaces before sending traffic; a duplicate fails
        // immediately, and the command's five-second deadline bounds waiting.
        for namespace in [router_pid, tenant_pid] {
            run(
                Some(namespace),
                &[
                    "sh",
                    "-ec",
                    r#"
                        while :; do
                            addresses="$(ip -6 -o address show)"
                            case "$addresses" in
                                *dadfailed*) printf '%s\n' "$addresses" >&2; exit 1 ;;
                                *tentative*) sleep 0.1 ;;
                                *) break ;;
                            esac
                        done
                    "#,
                ],
            )
            .await;
        }
        client
            .update_config(UpdateConfigRequest {
                config_update: Some(FmdsConfigUpdate {
                    hostname: hostname.into(),
                    address_ipv6: "2001:db8:7176::2".into(),
                    ..Default::default()
                }),
            })
            .await
            .expect("push metadata through gRPC");

        for (namespace, source, url) in [
            // The remote IPv6-only tenant needs the source policy return route.
            (
                tenant_pid,
                "2001:db8:7176::2",
                "http://[fd00:76b9::254]/latest/meta-data/hostname",
            ),
            // An off-link IPv4 source needs table 100 too; the connected
            // metadata peer alone would not prove IPv4 reply routing.
            (
                router_pid,
                "198.51.100.17",
                "http://169.254.169.254/latest/meta-data/hostname",
            ),
            // The compatibility port must use the same preserved reply path.
            (
                router_pid,
                "198.51.100.17",
                "http://169.254.169.254:7777/latest/meta-data/hostname",
            ),
        ] {
            assert_eq!(
                run(
                    Some(namespace),
                    &[
                        "curl",
                        "--fail",
                        "--silent",
                        "--show-error",
                        "--noproxy",
                        "*",
                        "--max-time",
                        "3",
                        "--interface",
                        source,
                        url
                    ]
                )
                .await,
                hostname
            );
        }
        for (family, source, gateway) in [
            // Both families must remain idempotent when sharing table 100.
            ("-4", "169.254.169.254", "169.254.169.253"),
            ("-6", "fd00:76b9::254", "fd00:76b9::255"),
        ] {
            let rules = run(None, &["ip", family, "rule", "show"]).await;
            assert_eq!(
                rules
                    .lines()
                    .filter(|line| line.contains(source) && line.contains("lookup 100"))
                    .count(),
                1,
                "{rules}"
            );
            let routes = run(None, &["ip", family, "route", "show", "table", "100"]).await;
            assert_eq!(
                routes
                    .lines()
                    .filter(|line| line.starts_with("default ")
                        && line.contains(gateway)
                        && line.contains("dev fmds-test"))
                    .count(),
                1,
                "{routes}"
            );
            assert!(
                !routes.contains("fmds-stale"),
                "stale default retained: {routes}"
            );
        }
        // Restart the process, not its namespace, so rules and addresses survive.
        timeout(Duration::from_secs(5), server.kill())
            .await
            .expect("stop deadline")
            .expect("stop FMDS");
    }
    run(
        Some(router_pid),
        &["ip", "address", "del", "198.51.100.17/32", "dev", "lo"],
    )
    .await;
    timeout(Duration::from_secs(5), tenant.kill())
        .await
        .expect("tenant stop deadline")
        .expect("stop tenant namespace");
}

/// Keep a namespace alive without named namespace bind mounts. Observe the
/// actual namespace change before moving interfaces into the child process.
async fn network_namespace() -> Child {
    let mut child = Command::new("unshare")
        .args(["--net", "sleep", "120"])
        .kill_on_drop(true)
        .spawn()
        .expect("create isolated network namespace");
    let path = format!("/proc/{}/ns/net", child.id().expect("namespace PID"));
    let original = std::fs::read_link("/proc/self/ns/net").expect("container namespace");
    timeout(Duration::from_secs(5), async {
        loop {
            assert!(
                child
                    .try_wait()
                    .expect("namespace process status")
                    .is_none(),
                "unshare failed; this test needs SYS_ADMIN"
            );
            if std::fs::read_link(&path).expect("child namespace") != original {
                break;
            }
            sleep(Duration::from_millis(20)).await;
        }
    })
    .await
    .expect("namespace creation deadline");
    child
}

/// Run one setup or observation command with a deadline and kill-on-drop, so
/// a failed networking assertion cannot leave an unbounded subprocess.
async fn run(namespace: Option<u32>, arguments: &[&str]) -> String {
    let mut command = if let Some(pid) = namespace {
        let mut command = Command::new("nsenter");
        command.args(["--target", &pid.to_string(), "--net", "--"]);
        command.args(arguments);
        command
    } else {
        let mut command = Command::new(arguments[0]);
        command.args(&arguments[1..]);
        command
    };
    command.kill_on_drop(true);
    let output = timeout(Duration::from_secs(5), command.output())
        .await
        .expect("command deadline")
        .expect("execute network test command");
    assert!(
        output.status.success(),
        "{arguments:?}: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8(output.stdout)
        .expect("command output")
        .trim()
        .to_owned()
}

/// Poll the actual control listener, rather than guessing when address setup
/// and DAD have finished. A dead server fails immediately with its exit status.
async fn connect_fmds(server: &mut Child) -> FmdsConfigServiceClient<Channel> {
    let endpoint = Endpoint::from_static("http://127.0.0.1:50052")
        .connect_timeout(Duration::from_secs(1))
        .timeout(Duration::from_secs(5));
    let deadline = Instant::now() + Duration::from_secs(15);
    loop {
        assert!(
            server.try_wait().expect("FMDS status").is_none(),
            "FMDS exited during startup"
        );
        match endpoint.connect().await {
            Ok(channel) => return FmdsConfigServiceClient::new(channel),
            Err(error) if Instant::now() >= deadline => panic!("FMDS startup deadline: {error}"),
            Err(_) => sleep(Duration::from_millis(100)).await,
        }
    }
}
