/*
 * SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
 * SPDX-License-Identifier: Apache-2.0
 */

//! Reconcile maintenance independently of attachment, HBN, and DHCP status.
//!
//! Controller requests identify logical interfaces such as eth_r0_p0. Resolve
//! these through provisioned mappings, then send the owning netdev name
//! (for example A56p0) to Xplane. Return per-interface readback and errors in
//! AstraConfigStatus while preserving any Weave attachment observations.
//! Provisioning requires these altnames to be assigned successfully. If a mapping
//! is later missing or ambiguous, report a per-interface error without changing
//! that interface's maintenance state, and resolve it again on the next poll.
//! With no configured mode, only read the flavor-initialized state; an explicit
//! operator request enables enforcement on subsequent polls.

use std::collections::{HashMap, HashSet};
use std::path::Path;

use eyre::WrapErr;
use rpc::forge::{AstraConfig, AstraConfigStatus, AstraMultiplaneInterfaceStatus};
use rtnetlink::packet_route::link::{LinkAttribute, LinkMessage, Prop};
use tokio::sync::OnceCell;
use tracing::Instrument;

use crate::xplane_client;

// DPUFlavor writes the logical-to-netdev mapping in configure-xplane-network.sh.
// The chart mounts its directory read-only. We load and cache the mapping for the
// agent lifetime; missing or invalid files retry on later polls.
const NETDEV_MAPPING_FILE: &str = "/etc/nico/xplane/logical-to-netdev-mapping";

async fn load_mapping() -> eyre::Result<Vec<LinkMessage>> {
    static MAPPING: OnceCell<Vec<LinkMessage>> = OnceCell::const_new();
    let links = MAPPING
        .get_or_try_init(|| async {
            let mapping = tokio::fs::read_to_string(NETDEV_MAPPING_FILE)
                .await
                .wrap_err_with(|| {
                    format!("reading provisioned xplane netdev mapping {NETDEV_MAPPING_FILE}")
                })?;
            decode_mapping(&mapping)
        })
        .await?;
    Ok(links.clone())
}

fn decode_mapping(mapping: &str) -> eyre::Result<Vec<LinkMessage>> {
    let mut logical_names = HashSet::new();
    let mut links = Vec::new();
    for (index, row) in mapping.lines().enumerate() {
        let fields: Vec<_> = row.split_whitespace().collect();
        let [logical, netdev] = fields.as_slice() else {
            return Err(eyre::eyre!(
                "invalid xplane netdev mapping at line {}",
                index + 1
            ));
        };
        if !logical_names.insert(*logical) {
            return Err(eyre::eyre!(
                "duplicate logical interface {logical} in xplane netdev mapping"
            ));
        }
        let mut link = LinkMessage::default();
        link.attributes
            .push(LinkAttribute::IfName((*netdev).to_string()));
        link.attributes
            .push(LinkAttribute::PropList(vec![Prop::AltIfName(
                (*logical).to_string(),
            )]));
        links.push(link);
    }
    if links.is_empty() {
        return Err(eyre::eyre!("provisioned xplane netdev mapping is empty"));
    }
    Ok(links)
}

/// Resolve eth_r<0..3>_p<0..7> to exactly one primary Linux interface name.
/// Xplane receives this netdev name as pf_name; it does not receive rail/plane names.
fn resolve_netdev(logical_ifname: &str, links: &[LinkMessage]) -> eyre::Result<String> {
    if !matches!(
        logical_ifname.as_bytes(),
        [
            b'e',
            b't',
            b'h',
            b'_',
            b'r',
            b'0'..=b'3',
            b'_',
            b'p',
            b'0'..=b'7'
        ]
    ) {
        return Err(eyre::eyre!(
            "invalid logical interface name {logical_ifname}"
        ));
    }
    // Continue after the first match so ambiguous ownership is rejected.
    let mut matched = None;
    for link in links {
        let Some(name) = link
            .attributes
            .iter()
            .find_map(|attribute| match attribute {
                LinkAttribute::IfName(name) => Some(name),
                _ => None,
            })
        else {
            continue;
        };
        // A logical name may be the primary name itself or an altname installed
        // during provisioning. In either case, return the owner's primary name.
        let matches = name == logical_ifname || link.attributes.iter().any(|attribute| match attribute {
            LinkAttribute::PropList(properties) => properties.iter().any(|property| matches!(property, Prop::AltIfName(altname) if altname == logical_ifname)),
            _ => false,
        });
        if matches {
            if matched.is_some() {
                return Err(eyre::eyre!(
                    "multiple netdevs own logical interface {logical_ifname}"
                ));
            }
            matched = Some(name.clone());
        }
    }
    matched.ok_or_else(|| eyre::eyre!("no netdev owns logical interface {logical_ifname}"))
}

/// Observe maintenance and enforce explicit requests, returning observations merged with the
/// existing attachment status. The main loop checks config presence before calling.
pub(crate) async fn build_notify_xplane_maintenance_config(
    config: &AstraConfig,
    mut status: Option<AstraConfigStatus>,
) -> Option<AstraConfigStatus> {
    tracing::info!(
        requested_interface_count = config.multiplane_interfaces.len(),
        socket_path = xplane_client::XPLANE_MAINTENANCE_SOCKET_PATH,
        "spx-maintenance: [dpu-agent] DPU received maintenance requests in AstraConfig"
    );
    if config.multiplane_interfaces.is_empty() {
        tracing::info!(
            "spx-maintenance: [dpu-agent] Skipping Xplane maintenance reconciliation: no requested interfaces"
        );
        return status;
    }

    let maintenance = notify_xplane_maintenance_reconcile_links(
        Path::new(xplane_client::XPLANE_MAINTENANCE_SOCKET_PATH),
        config,
        load_mapping().await,
    )
    .await;
    merge_xplane_status(&mut status, maintenance);
    status
}

/// Validate the complete mapping snapshot before sending any Xplane requests.
/// Each invalid mapping or failed RPC gets its own status; other valid ports
/// can still be updated.
async fn notify_xplane_maintenance_reconcile_links(
    socket_path: &Path,
    config: &AstraConfig,
    links: eyre::Result<Vec<LinkMessage>>,
) -> Vec<AstraMultiplaneInterfaceStatus> {
    tracing::info!(socket_path = %socket_path.display(), requested_interface_count = config.multiplane_interfaces.len(), discovered_interface_count = ?links.as_ref().map(Vec::len).ok(), "spx-maintenance: [dpu-agent] Validating Xplane maintenance mappings");
    // 1. Validate each request and resolve its logical name to a PF netdev.
    // Keep one status per input, including failures, so Core can show each result.
    let mut statuses: Vec<_> = config
        .multiplane_interfaces
        .iter()
        .map(|request| {
            // Echo the desired revision so Core can reject observations from an
            // older request that arrive after an operator changes the mode.
            let mut status = AstraMultiplaneInterfaceStatus {
                logical_ifname: request.logical_ifname.clone(),
                revision: request.revision.clone(),
                ..Default::default()
            };
            let result = (|| {
                if config.multiplane_interfaces.len() > 32 {
                    return Err(eyre::eyre!(
                        "at most 32 multiplane interfaces are supported"
                    ));
                }
                if request.revision.is_empty() {
                    return Err(eyre::eyre!("revision is required"));
                }
                // Preserve discovery failure details rather than reporting every
                // interface as missing when no kernel snapshot was available.
                let links = links.as_ref().map_err(|error| eyre::eyre!("{error:#}"))?;
                resolve_netdev(&request.logical_ifname, links)
            })();
            match result {
                Ok(netdev) => status.netdev = Some(netdev),
                Err(error) => status.error = Some(format!("{error:#}")),
            }
            status
        })
        .collect();

    // 2. Reject every request that shares a PF with another logical interface.
    // Otherwise concurrent requests could set conflicting modes on the same PF.
    let mut owners = HashMap::<&str, usize>::new();
    for status in &statuses {
        if let Some(netdev) = status.netdev.as_deref() {
            *owners.entry(netdev).or_default() += 1;
        }
    }
    let duplicates: Vec<String> = owners
        .into_iter()
        .filter(|(_, count)| *count > 1)
        .map(|(name, _)| name.to_string())
        .collect();
    for status in &mut statuses {
        if status
            .netdev
            .as_ref()
            .is_some_and(|name| duplicates.contains(name))
        {
            status.error =
                Some("multiple logical interfaces resolve to the same PF netdev".to_string());
        }
    }

    // 3. Open one shared gRPC channel only if there are valid requests to send.
    // Retain a connection failure so it can be reported on each eligible port.
    let client = if statuses.iter().any(|s| s.error.is_none()) {
        Some(xplane_client::connect(socket_path).await)
    } else {
        None
    };

    // 4. Read valid ports and enforce explicit modes concurrently so a slow port does not impose its
    // timeout on every other port in sequence. join_all preserves input order.
    let statuses = futures::future::join_all(statuses.into_iter().zip(&config.multiplane_interfaces).map(|(mut status, request)| {
        let client = &client;
        let span = tracing::info_span!("spx-maintenance: [dpu-agent] xplane_maintenance_interface", logical_ifname = %status.logical_ifname, netdev = ?status.netdev, revision = %status.revision, requested_maintenance_mode = ?request.cfg_maintenance_mode, logfmt.suppress = true);
        async move {
            // Validation failures are returned unchanged and never sent to Xplane.
            if status.error.is_none() {
                match client.as_ref().expect("valid requests connect to Xplane") {
                    Err(error) => status.error = Some(format!("{error:#}")),
                    Ok(client) => {
                        let desired = request.cfg_maintenance_mode;
                        // An unset configured mode only reads the current state. An explicit
                        // request sets when needed and verifies readback before reporting success.
                        match xplane_client::reconcile(client.clone(), status.netdev.as_deref().expect("resolved PF"), desired).await {
                            Ok(actual) => {
                                status.maintenance_mode = Some(actual);
                                if desired.is_some_and(|desired| actual != desired) { status.error = Some("Xplane readback does not match requested maintenance mode".to_string()); }
                            },
                            Err(error) => status.error = Some(format!("{error:#}")),
                        }
                    },
                }
            }
            // Timestamp failures too, allowing Core to order observations for
            // the same desired revision. Failed ports retry on the next poll.
            status.observed_at = Some(chrono::Utc::now().into());
            status
        }.instrument(span)
    })).await;
    // Routine polls produce one summary; failures retain per-port details in one warning.
    let failures: Vec<_> = statuses
        .iter()
        .filter(|status| status.error.is_some())
        .collect();
    if !failures.is_empty() {
        tracing::warn!(failures = ?failures, "spx-maintenance: [dpu-agent] ERROR: Xplane maintenance failures; retrying on next config poll");
    }

    // Consolidate the per-interface mapping, request and readback into one entry.
    let interfaces: Vec<_> = config
        .multiplane_interfaces
        .iter()
        .zip(&statuses)
        .map(|(request, status)| {
            // Use the logical name as the entry key so it stays first even when
            // serde_json sorts the remaining object's fields alphabetically.
            serde_json::json!({
                (status.logical_ifname.as_str()): {
                    "netdev": status.netdev,
                    "cfg_maintenance_mode": request.cfg_maintenance_mode,
                    "observed_maintenance_mode": status.maintenance_mode,
                    "revision": status.revision,
                    "error": status.error,
                }
            })
        })
        .collect();
    tracing::info!(
        interfaces = %serde_json::Value::Array(interfaces),
        interface_count = statuses.len(),
        observe_only_interface_count = config
            .multiplane_interfaces
            .iter()
            .filter(|request| request.cfg_maintenance_mode.is_none())
            .count(),
        maintenance_on_interface_count = statuses
            .iter()
            .filter(|status| status.error.is_none() && status.maintenance_mode == Some(true))
            .count(),
        maintenance_off_interface_count = statuses
            .iter()
            .filter(|status| status.error.is_none() && status.maintenance_mode == Some(false))
            .count(),
        verified_interface_count = statuses.iter().filter(|s| s.error.is_none()).count(),
        failed_interface_count = statuses.iter().filter(|s| s.error.is_some()).count(),
        "spx-maintenance: [dpu-agent] Completed Xplane maintenance reconciliation batch"
    );
    statuses
}

/// Add maintenance observations without changing attachment observations or
/// their presence flag. With no observations, preserve the existing status.
fn merge_xplane_status(
    status: &mut Option<AstraConfigStatus>,
    maintenance: Vec<AstraMultiplaneInterfaceStatus>,
) {
    if !maintenance.is_empty() {
        // A maintenance-only report must not tell Core to clear attachment state.
        let status = status.get_or_insert_with(|| AstraConfigStatus {
            is_attachment_status_present: Some(false),
            ..Default::default()
        });
        status.is_maintenance_status_present = Some(true);
        status.multiplane_interfaces_status = maintenance;
    }
}

#[cfg(test)]
mod tests {
    // Mapping tests use synthetic kernel messages. The RPC test explicitly starts
    // a mock on a temporary socket, leaving the real Xplane socket untouched.
    use std::sync::Arc;
    use std::time::Duration;

    use rpc::forge::{AstraAttachmentStatus, AstraMultiplaneInterface};

    use super::*;
    use crate::xplane_mock_server::{self, MockXplaneMaintenance};

    #[test]
    fn xplane_status_merge_preserves_attachment_observations() {
        // Cover absent reports, maintenance-only reports, and combined reports.
        let maintenance = || {
            vec![AstraMultiplaneInterfaceStatus {
                logical_ifname: "eth_r0_p0".to_string(),
                maintenance_mode: Some(false),
                ..Default::default()
            }]
        };
        let mut absent = None;
        merge_xplane_status(&mut absent, Vec::new());
        assert!(absent.is_none());
        merge_xplane_status(&mut absent, maintenance());
        let only_maintenance = absent.unwrap();
        assert_eq!(only_maintenance.is_attachment_status_present, Some(false));
        assert_eq!(only_maintenance.is_maintenance_status_present, Some(true));
        assert_eq!(only_maintenance.multiplane_interfaces_status.len(), 1);

        let mut combined = Some(AstraConfigStatus {
            is_attachment_status_present: Some(true),
            is_maintenance_status_present: Some(false),
            astra_attachments_status: vec![AstraAttachmentStatus {
                mac_address: "00:11:22:33:44:55".to_string(),
                ..Default::default()
            }],
            ..Default::default()
        });
        merge_xplane_status(&mut combined, maintenance());
        let combined = combined.unwrap();
        assert_eq!(combined.is_attachment_status_present, Some(true));
        assert_eq!(combined.is_maintenance_status_present, Some(true));
        assert_eq!(
            combined.astra_attachments_status[0].mac_address,
            "00:11:22:33:44:55"
        );
        assert_eq!(combined.multiplane_interfaces_status.len(), 1);
    }

    #[tokio::test]
    async fn empty_maintenance_interfaces_skip_reconciliation() {
        // An empty request must neither discover netdevs nor alter existing status.
        for status in [
            None,
            Some(AstraConfigStatus {
                is_attachment_status_present: Some(true),
                astra_attachments_status: vec![AstraAttachmentStatus {
                    mac_address: "00:11:22:33:44:55".to_string(),
                    ..Default::default()
                }],
                ..Default::default()
            }),
        ] {
            let result =
                build_notify_xplane_maintenance_config(&AstraConfig::default(), status.clone())
                    .await;
            assert_eq!(result, status);
        }
    }

    /// Build the same name/altname attributes returned by kernel discovery.
    fn link(name: &str, altnames: &[&str]) -> LinkMessage {
        let mut link = LinkMessage::default();
        link.attributes
            .push(LinkAttribute::IfName(name.to_string()));
        link.attributes.push(LinkAttribute::PropList(
            altnames
                .iter()
                .map(|name| Prop::AltIfName(name.to_string()))
                .collect(),
        ));
        link
    }

    #[test]
    fn logical_interface_resolves_to_primary_netdev() {
        // A name resolves only when its format is valid and ownership is unique.
        for (scenario, name, links, expected) in [
            (
                "altname",
                "eth_r0_p0",
                vec![link("A56p0", &["eth_r0_p0"])],
                Some("A56p0"),
            ),
            (
                "primary name",
                "eth_r0_p0",
                vec![link("eth_r0_p0", &[])],
                Some("eth_r0_p0"),
            ),
            (
                "missing",
                "eth_r0_p1",
                vec![link("A56p0", &["eth_r0_p0"])],
                None,
            ),
            (
                "ambiguous",
                "eth_r0_p0",
                vec![link("A56p0", &["eth_r0_p0"]), link("A53p0", &["eth_r0_p0"])],
                None,
            ),
            (
                "invalid logical name",
                "eth_r0_p8",
                vec![link("A56p0", &["eth_r0_p8"])],
                None,
            ),
        ] {
            assert_eq!(
                resolve_netdev(name, &links).ok().as_deref(),
                expected,
                "{scenario}"
            );
        }
    }

    #[tokio::test]
    async fn xplane_reconciliation_reports_readback_retries_and_isolates_failures() {
        let directory = tempfile::tempdir().unwrap();
        let socket = directory.path().join("xplane.sock");
        let service = Arc::new(MockXplaneMaintenance::default());
        // Inject independent failures in one batch: rejected Set, mismatching
        // readback, and a stalled Get. Other ports must still finish normally.
        service
            .fail_set_for
            .lock()
            .await
            .insert("A56p1".to_string());
        service
            .readback_override
            .lock()
            .await
            .insert("A53p2".to_string(), false);
        // An unconfigured port reports either observed mode without issuing Set.
        service
            .readback_override
            .lock()
            .await
            .insert("A56p2".to_string(), false);
        service
            .stall_get_for
            .lock()
            .await
            .insert("A53p3".to_string());
        let server_path = socket.clone();
        let server_service = service.clone();
        let server = tokio::spawn(async move {
            xplane_mock_server::serve_with_service(&server_path, server_service).await
        });
        tokio::time::timeout(Duration::from_secs(5), async {
            loop {
                if xplane_client::connect(&socket).await.is_ok() {
                    break;
                }
                assert!(!server.is_finished(), "mock server stopped");
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        })
        .await
        .unwrap();
        let request = |name: &str, mode| AstraMultiplaneInterface {
            logical_ifname: name.to_string(),
            cfg_maintenance_mode: mode,
            revision: "V1-T1791792000000000".to_string(),
            ..Default::default()
        };
        let mut config = AstraConfig {
            multiplane_interfaces: vec![
                request("eth_r0_p0", Some(false)),
                request("eth_r0_p1", Some(true)),
                request("eth_r0_p2", None),
                request("eth_r0_p3", Some(true)),
                request("eth_r1_p0", Some(true)),
                request("eth_r1_p1", Some(true)),
                request("eth_r1_p2", Some(true)),
                request("eth_r1_p3", Some(true)),
            ],
            ..Default::default()
        };
        let links = vec![
            link("A56p0", &["eth_r0_p0"]),
            link("A56p1", &["eth_r0_p1"]),
            link("A56p2", &["eth_r0_p2"]),
            link("A53p0", &["eth_r1_p0", "eth_r1_p1"]),
            link("A53p2", &["eth_r1_p2"]),
            link("A53p3", &["eth_r1_p3"]),
        ];
        // Synthetic links also include a missing owner and two logical names
        // sharing one PF; neither mapping error should reach the mock.
        let statuses = tokio::time::timeout(
            Duration::from_secs(5),
            notify_xplane_maintenance_reconcile_links(&socket, &config, Ok(links.clone())),
        )
        .await
        .unwrap();
        assert_eq!(statuses.len(), config.multiplane_interfaces.len());
        assert_eq!(statuses[0].netdev.as_deref(), Some("A56p0"));
        assert_eq!(statuses[0].maintenance_mode, Some(false));
        assert!(statuses[0].error.is_none());
        assert_eq!(statuses[2].maintenance_mode, Some(false));
        assert!(statuses[2].error.is_none());
        for (_, status) in statuses
            .iter()
            .enumerate()
            .skip(1)
            .filter(|(index, _)| *index != 2)
        {
            assert!(
                status.error.is_some(),
                "{} must report its failure",
                status.logical_ifname
            );
        }
        assert_eq!(
            statuses[6].maintenance_mode,
            Some(false),
            "report actual state when readback disagrees"
        );
        assert!(
            statuses[7].maintenance_mode.is_none(),
            "timeout cannot claim application"
        );
        let mut calls = service.set_calls.lock().await.clone();
        calls.sort();
        assert_eq!(
            calls,
            vec![
                ("A53p2".to_string(), true),
                ("A56p0".to_string(), false),
                ("A56p1".to_string(), true)
            ]
        );

        service
            .readback_override
            .lock()
            .await
            .insert("A56p2".to_string(), true);
        let read_only = AstraConfig {
            multiplane_interfaces: vec![request("eth_r0_p2", None)],
            ..Default::default()
        };
        let observed = notify_xplane_maintenance_reconcile_links(
            &socket,
            &read_only,
            Ok(vec![link("A56p2", &["eth_r0_p2"])]),
        )
        .await;
        assert_eq!(observed[0].maintenance_mode, Some(true));
        assert!(observed[0].error.is_none());
        assert!(
            service
                .set_calls
                .lock()
                .await
                .iter()
                .all(|(pf, _)| pf != "A56p2")
        );

        // Same desired revision retries a failed port, while verified ports
        // avoid redundant sets. No local success cache hides server drift.
        service.fail_set_for.lock().await.clear();
        config.multiplane_interfaces.truncate(2);
        let retry = notify_xplane_maintenance_reconcile_links(&socket, &config, Ok(links)).await;
        assert!(retry.iter().all(|s| s.error.is_none()));
        assert_eq!(retry[1].maintenance_mode, Some(true));
        let calls = service.set_calls.lock().await;
        assert_eq!(calls.iter().filter(|(name, _)| name == "A56p0").count(), 1);
        assert_eq!(calls.iter().filter(|(name, _)| name == "A56p1").count(), 2);
        drop(calls);

        // If the service is unavailable, every valid request must report an
        // error without claiming that its maintenance mode was applied.
        let missing_socket = directory.path().join("missing.sock");
        let failures = notify_xplane_maintenance_reconcile_links(
            &missing_socket,
            &config,
            Ok(vec![
                link("A56p0", &["eth_r0_p0"]),
                link("A56p1", &["eth_r0_p1"]),
            ]),
        )
        .await;
        assert!(
            failures
                .iter()
                .all(|s| s.error.is_some() && s.maintenance_mode.is_none())
        );
        server.abort();
        assert!(server.await.unwrap_err().is_cancelled());
    }
}
