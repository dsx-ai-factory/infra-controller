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

use std::path::Path;

use carbide_host_support::agent_config::MachineIdentityConfig;
use forge_dpu_agent_utils::FMDS_IPV6_GATEWAY;
use tokio::process::{Child, Command};
use tokio::time::{sleep, timeout};

use super::*;
use crate::dpu::REAL_KERNEL_NETWORKING;
use crate::instance_metadata_endpoint::InstanceMetadataRouterStateImpl;
use crate::periodic_config_fetcher::{InstanceMetadata, PublicAddresses};

/// Production address and route application must let an IPv6-only tenant reach
/// embedded FMDS after an exact-address cold start. Reapplying, replacing and
/// withdrawing reply routes must preserve IPv4 and unrelated kernel routes.
/// Start the isolated container with `--sysctl net.ipv6.conf.all.forwarding=1`
/// and run this test binary through `unshare --net`; container PID 1 is the router.
#[tokio::test]
#[ignore = "requires isolated Docker with NET_ADMIN, SYS_ADMIN, iproute2, curl and util-linux"]
async fn embedded_metadata_reaches_ipv6_tenant_after_route_reapply() {
    assert!(Path::new("/.dockerenv").exists(), "container required");
    assert_eq!(
        std::env::var("FMDS_ROUTING_TEST_ISOLATED").as_deref(),
        Ok("1")
    );
    assert_ne!(
        std::fs::read_link("/proc/self/ns/net").expect("test namespace"),
        std::fs::read_link("/proc/1/ns/net").expect("router namespace"),
        "run the test through unshare --net, separately from container PID 1"
    );
    for fixture_override in ["NO_DPU_ARMOS_NETWORK", "NO_DPU_ARMOS_INTERFACE"] {
        assert!(
            std::env::var_os(fixture_override).is_none(),
            "unset {fixture_override}"
        );
    }
    timeout(
        Duration::from_secs(60),
        REAL_KERNEL_NETWORKING.scope((), routed_metadata()),
    )
    .await
    .expect("embedded routing deadline");
}

/// Use the container's preconfigured forwarding namespace as the router. FMDS
/// and the tenant have separate namespaces; their routes stay inside the container.
async fn routed_metadata() {
    let router_pid = 1;
    assert_eq!(
        run(
            Some(router_pid),
            &["sysctl", "-n", "net.ipv6.conf.all.forwarding"]
        )
        .await,
        "1",
        "the container must start with IPv6 forwarding enabled"
    );
    let mut tenant = network_namespace().await;
    let tenant_pid = tenant.id().expect("tenant PID");
    run(
        None,
        &[
            "sh",
            "-ec",
            r#"
        ip link set lo up
        ip link add fmds-kernel type veth peer name metadata-peer netns "$1"
        ip link set fmds-kernel up
        ip link add router-tenant type veth peer name tenant-fmds
        ip link set router-tenant netns "$1"
        ip link set tenant-fmds netns "$2"
    "#,
            "--",
            &router_pid.to_string(),
            &tenant_pid.to_string(),
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
        ip link set metadata-peer up
        ip address add 192.0.2.1/32 dev metadata-peer
        ip address add 169.254.169.253/30 dev metadata-peer
        ip address add fd00:76b9::255/127 dev metadata-peer
        ip link set router-tenant up
        ip address add 2001:db8:7176:1::1/64 dev router-tenant
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
        ip link set tenant-fmds up
        ip address add 2001:db8:7176:1::2/64 dev tenant-fmds
        ip -6 route add fd00:76b9::254/128 via 2001:db8:7176:1::1 dev tenant-fmds
    "#,
        ],
    )
    .await;
    // Both veth peers must be up before DAD can finish. Reject duplicate
    // addresses explicitly instead of treating cleared tentative flags as ready.
    for namespace in [router_pid, tenant_pid] {
        run(
            Some(namespace),
            &[
                "sh",
                "-ec",
                r#"
        while :; do
            if [ -n "$(ip -6 -o address show dadfailed)" ]; then
                ip -6 address show dadfailed >&2
                exit 1
            fi
            if [ -z "$(ip -6 -o address show tentative)" ]; then
                break
            fi
            sleep 0.1
        done
    "#,
            ],
        )
        .await;
    }
    assert!(
        run(
            Some(tenant_pid),
            &["ip", "-4", "-o", "address", "show", "dev", "tenant-fmds"]
        )
        .await
        .is_empty()
    );

    #[allow(deprecated)]
    let network_config = rpc::ManagedHostNetworkConfigResponse {
        tenant_interfaces: vec![rpc::FlatInterfaceConfig {
            prefix: Some("192.0.2.0/24".to_string()),
            addresses: vec![rpc::InterfaceAddressConfig {
                address_family: rpc::AddressFamily::V6.into(),
                prefix: "2001:db8:7176:1::/64".to_string(),
                ..Default::default()
            }],
            ..Default::default()
        }],
        ..Default::default()
    };
    let prefixes = fmds_tenant_routes(&network_config).expect("tenant prefixes");
    let state = Arc::new(
        InstanceMetadataRouterStateImpl::new(
            "fm100dsvstfujf6mis0gpsoi81tadmllicv7rqo4s7gc16gi0t2478672vg"
                .parse()
                .expect("DPU ID"),
            "http://127.0.0.1:1".to_string(),
            Arc::new(ForgeClientConfig::default()),
            MachineIdentityConfig::default(),
        )
        .expect("metadata state"),
    );
    let version = "V1-T1666644937952267".parse().expect("config version");
    state.update_instance_data(Some(Arc::new(InstanceMetadata {
        public_addresses: PublicAddresses::default(),
        hostname: "embedded-local-host".to_string(),
        instance_name: None,
        sitename: None,
        instance_id: None,
        machine_id: None,
        user_data: String::new(),
        ib_devices: None,
        config_version: version,
        network_config_version: version,
        extension_service_version: version,
    })));
    state.update_network_configuration(Some(Arc::new(network_config)));
    let mut tasks = JoinSet::new();
    let cancel = CancellationToken::new();
    // An exact IPv6 override does not imply an IPv4 listener. Keep a separate
    // explicit IPv4 listener to check the unchanged IPv4 reply path below.
    crate::metadata_service::spawn_metadata_service(
        "0.0.0.0:7777".to_string(),
        create_metrics(opentelemetry::global::meter("embedded-routing-test")),
        state.clone(),
        false,
        &mut tasks,
        cancel.clone(),
    )
    .await
    .expect("embedded metadata listener");
    let mut pending = Some(PendingMetadataService {
        address: format!("[{FMDS_IPV6_ADDRESS}]:7777"),
        metrics: create_metrics(opentelemetry::global::meter("embedded-routing-test")),
        state,
    });
    assert!(
        tokio::net::TcpListener::bind((FMDS_IPV6_ADDRESS, 7777))
            .await
            .is_err(),
        "the exact endpoint must not exist before address reconciliation"
    );

    // The tenant prefixes are absent until the production reconciler installs
    // them. The second pass must observe kernel state, not repeat its writes.
    assert!(
        Route::current_routes("fmds-kernel")
            .await
            .expect("initial routes")
            .is_empty()
    );
    for reapply in [false, true] {
        let desired = || DpuNetworkInterfaces {
            desired: vec![
                "169.254.169.254/30".parse().expect("IPv4 metadata address"),
                "fd00:76b9::254/127".parse().expect("IPv6 metadata address"),
            ],
        };
        let address_plan = Interface::plan("fmds-kernel", desired())
            .await
            .expect("address plan");
        if reapply {
            assert!(
                address_plan
                    .values()
                    .flat_map(|interfaces| interfaces.values())
                    .all(|addresses| addresses.as_ref().is_none_or(Vec::is_empty))
            );
        }
        let duplicate_plan = if reapply {
            None
        } else {
            assert!(
                address_plan
                    .values()
                    .flat_map(|interfaces| interfaces.values())
                    .any(|addresses| addresses
                        .as_ref()
                        .is_some_and(|addresses| !addresses.is_empty()))
            );
            Some(address_plan.clone())
        };
        // Exercise the same address -> DAD -> exact-bind sequence as the main
        // loop, starting with neither the endpoint address nor its listener.
        apply_fmds_interface(
            "fmds-kernel",
            desired(),
            true,
            &mut pending,
            &mut tasks,
            &cancel,
        )
        .await
        .expect("apply metadata addresses and deferred listener");
        assert!(pending.is_none());
        assert_eq!(tasks.len(), 2, "reapply must not spawn another listener");
        // A stale nonempty plan must expose the kernel's EEXIST failure. A
        // fresh observation on the second pass still produces an empty plan.
        if let Some(duplicate_plan) = duplicate_plan {
            let error = Interface::apply(duplicate_plan)
                .await
                .expect_err("replaying the initial address plan must fail");
            // iproute2 versions phrase EEXIST differently. The application error
            // must retain the failed operation and address, not swallow it.
            assert!(
                error
                    .to_string()
                    .contains("failed to add address 169.254.169.254/30 on fmds-kernel"),
                "{error:#}"
            );
        }
        Interface::wait_for_ipv6_address("fmds-kernel", FMDS_IPV6_ADDRESS)
            .await
            .expect("metadata DAD completed");
        let route_plan = plan_fmds_armos_routing(
            "fmds-kernel",
            &prefixes,
            Some(Ipv6Network::new(FMDS_IPV6_GATEWAY, 127).expect("metadata peer")),
        )
        .await
        .expect("route plan");
        if reapply {
            assert!(route_plan.is_empty(), "{route_plan:?}");
        }
        Route::apply(route_plan)
            .await
            .expect("apply metadata routes");
        assert_eq!(
            Route::current_routes("fmds-kernel")
                .await
                .expect("applied routes")
                .len(),
            2
        );
        for (namespace, source, url) in [
            // This tenant has no IPv4 address; its reply needs the IPv6 route.
            (
                tenant_pid,
                "2001:db8:7176:1::2",
                "http://[fd00:76b9::254]:7777/latest/meta-data/hostname",
            ),
            // A source outside the metadata /30 also proves the retained IPv4 route.
            (
                router_pid,
                "192.0.2.1",
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
                "embedded-local-host"
            );
        }
    }

    // Model a route left by an older configuration with a different source.
    // The replacement needs a real delete/add while leaving a direct route alone.
    run(
        None,
        &[
            "ip",
            "address",
            "add",
            "fd00:76b9::256/128",
            "dev",
            "fmds-kernel",
        ],
    )
    .await;
    Interface::wait_for_ipv6_address(
        "fmds-kernel",
        "fd00:76b9::256".parse().expect("stale route source"),
    )
    .await
    .expect("stale source DAD completed");
    run(
        None,
        &[
            "ip",
            "-6",
            "route",
            "replace",
            "2001:db8:7176:1::/64",
            "via",
            "fd00:76b9::255",
            "dev",
            "fmds-kernel",
            "src",
            "fd00:76b9::256",
        ],
    )
    .await;
    run(
        None,
        &[
            "ip",
            "-6",
            "route",
            "add",
            "2001:db8:99::/64",
            "dev",
            "fmds-kernel",
            "proto",
            "static",
        ],
    )
    .await;
    let gateway = Some(Ipv6Network::new(FMDS_IPV6_GATEWAY, 127).expect("metadata peer"));
    let replacement = plan_fmds_armos_routing("fmds-kernel", &prefixes, gateway)
        .await
        .expect("replacement plan");
    assert_eq!(
        replacement.values().map(Vec::len).sum::<usize>(),
        2,
        "{replacement:?}"
    );
    Route::apply(replacement)
        .await
        .expect("replace stale IPv6 source");
    let replaced = Route::current_routes("fmds-kernel")
        .await
        .expect("replaced routes");
    assert_eq!(replaced.len(), 2);
    assert!(
        replaced
            .iter()
            .any(|route| route.dst.is_ipv6() && route.prefsrc == Some(FMDS_IPV6_ADDRESS.into()))
    );
    assert_eq!(
        run(
            Some(tenant_pid),
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
                "2001:db8:7176:1::2",
                "http://[fd00:76b9::254]:7777/latest/meta-data/hostname"
            ]
        )
        .await,
        "embedded-local-host"
    );

    // Removing the IPv6 tenant must withdraw its owned route without removing
    // IPv4 service or the unrelated direct route on this same interface.
    let ipv4_prefixes: Vec<_> = prefixes.into_iter().filter(IpNetwork::is_ipv4).collect();
    let withdrawal = plan_fmds_armos_routing("fmds-kernel", &ipv4_prefixes, gateway)
        .await
        .expect("withdrawal plan");
    assert_eq!(
        withdrawal.values().map(Vec::len).sum::<usize>(),
        1,
        "{withdrawal:?}"
    );
    Route::apply(withdrawal)
        .await
        .expect("withdraw IPv6 tenant");
    let remaining = Route::current_routes("fmds-kernel")
        .await
        .expect("retained IPv4 route");
    assert_eq!(remaining.len(), 1);
    assert!(remaining[0].dst.is_ipv4());
    let ipv6_routes = run(None, &["ip", "-6", "route", "show", "dev", "fmds-kernel"]).await;
    assert!(
        !ipv6_routes.contains("2001:db8:7176:1::/64"),
        "{ipv6_routes}"
    );
    assert!(ipv6_routes.contains("2001:db8:99::/64"), "{ipv6_routes}");
    assert_eq!(
        run(
            Some(router_pid),
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
                "192.0.2.1",
                "http://169.254.169.254:7777/latest/meta-data/hostname"
            ]
        )
        .await,
        "embedded-local-host"
    );
    cancel.cancel();
    timeout(Duration::from_secs(6), tasks.join_all())
        .await
        .expect("metadata shutdown");
    timeout(Duration::from_secs(5), tenant.kill())
        .await
        .expect("namespace stop deadline")
        .expect("stop tenant namespace");
    // All addresses belong to test veths. Namespace teardown removes them even
    // after an assertion failure; the container's loopback is never modified.
}

/// Observe namespace creation before moving links; kill-on-drop and the holder's
/// finite lifetime keep failed assertions from leaving persistent namespaces.
async fn network_namespace() -> Child {
    let mut child = Command::new("unshare")
        .args(["--net", "sleep", "90"])
        .kill_on_drop(true)
        .spawn()
        .expect("create network namespace");
    let path = format!("/proc/{}/ns/net", child.id().expect("namespace PID"));
    let original = std::fs::read_link("/proc/self/ns/net").expect("container namespace");
    timeout(Duration::from_secs(5), async {
        loop {
            assert!(
                child.try_wait().expect("namespace status").is_none(),
                "unshare failed"
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

/// Bound each setup, observation, or HTTP command and preserve stderr on failure.
async fn run(namespace: Option<u32>, arguments: &[&str]) -> String {
    let mut command = match namespace {
        Some(pid) => {
            let mut command = Command::new("nsenter");
            command
                .args(["--target", &pid.to_string(), "--net", "--"])
                .args(arguments);
            command
        }
        None => {
            let mut command = Command::new(arguments[0]);
            command.args(&arguments[1..]);
            command
        }
    };
    command.kill_on_drop(true);
    let output = timeout(Duration::from_secs(5), command.output())
        .await
        .expect("command deadline")
        .expect("execute command");
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
