// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

//! Firmware proofs reuse the compute reader's credential checks and owned gates.

use carbide_instrument::testing::capture_logs_async;

use super::*;
use crate::nv_switch_manager::NvSwitchManager;
use crate::power_shelf_manager::PowerShelfManager;

/// Existing managers inherit an unavailable capability and reject passive reads;
/// an existing firmware updater is not evidence of passive inventory support.
#[tokio::test]
async fn firmware_inventory_defaults_are_unsupported() {
    let compute = crate::mock::MockComputeTrayManager;
    let switch = crate::mock::MockNvSwitchManager::default();
    let shelf = crate::mock::MockPowerShelfManager;

    let results = [
        (
            compute.firmware_inventory_source(),
            compute.get_firmware_inventory(&[]).await,
        ),
        (
            switch.firmware_inventory_source(),
            switch.get_firmware_inventory(&[]).await,
        ),
        (
            shelf.firmware_inventory_source(),
            shelf.get_firmware_inventory(&[]).await,
        ),
    ];

    for (source, result) in results {
        assert_eq!(source, None);
        assert!(matches!(result, Err(ComponentManagerError::Unsupported(_))));
    }
}

/// Seeds the simulator's raw collection so the reader must validate collection
/// metadata before it can request members. Repeated IDs remain repeated links.
fn set_firmware_test_inventory(pool: &ComputeReadTestPool, ids: &[String]) {
    let members: Vec<_> = ids
        .iter()
        .map(|id| serde_json::json!({ "@odata.id": format!("{FIRMWARE_INVENTORY_URI}/{id}") }))
        .collect();
    pool.sim.set_resource(
        FIRMWARE_INVENTORY_URI,
        serde_json::json!({ "Members": members, "Members@odata.count": ids.len() }),
    );
}

/// Drives a gated request until observable progress, allowing nested streams to
/// yield cooperatively. A one-second test budget turns missing progress into a failure.
async fn poll_firmware_until<F: std::future::Future + ?Sized>(
    mut request: std::pin::Pin<&mut F>,
    reached: impl Fn() -> bool,
) {
    tokio::time::timeout(
        Duration::from_secs(1),
        std::future::poll_fn(|cx| {
            assert!(
                std::future::Future::poll(request.as_mut(), cx).is_pending(),
                "gated firmware request finished before release"
            );
            if reached() {
                std::task::Poll::Ready(())
            } else {
                std::task::Poll::Pending
            }
        }),
    )
    .await
    .expect("firmware request must reach its expected gates");
}

/// Reversed endpoint/member completion must preserve the caller's identities,
/// repeated slots, exact backend versions, and the difference between absent and empty.
#[tokio::test]
async fn firmware_inventory_preserves_members_metadata_and_endpoint_order() {
    let endpoints = [
        test_compute_tray_endpoint(1),
        test_compute_tray_endpoint(2),
        test_compute_tray_endpoint(2),
    ];
    let pool = compute_read_test_pool(&endpoints);
    set_firmware_test_inventory(
        &pool,
        &["BMC_Firmware".into(), "other".into(), "BMC_Firmware".into()],
    );
    for (index, endpoint) in endpoints[..2].iter().enumerate() {
        let host = endpoint.bmc_ip.to_string();
        for id in ["BMC_Firmware", "other"] {
            let version = if id == "BMC_Firmware" {
                Some(format!("v{index}.2.3"))
            } else if index == 0 {
                None
            } else {
                Some(String::new())
            };
            pool.firmware.members.lock().unwrap().insert(
                (host.clone(), id.into()),
                Ok(SoftwareInventory {
                    odata: libredfish::model::ODataLinks {
                        // The response URI deliberately does not identify the lookup path.
                        odata_id: format!("/reported/{index}/image-{id}"),
                        ..Default::default()
                    },
                    id: format!("image-{index}-{id}"),
                    description: if id == "other" {
                        if index == 0 {
                            None
                        } else {
                            Some(String::new())
                        }
                    } else {
                        Some(format!("Image on host {index}"))
                    },
                    version,
                    release_date: if id == "other" {
                        if index == 0 {
                            Some(String::new())
                        } else {
                            None
                        }
                    } else {
                        Some("2026-11-03".into())
                    },
                }),
            );
        }
    }
    let manager = CoreComputeTrayManager::new(pool.clone());
    let before = pool.sim.timepoint();

    assert!(
        manager
            .get_firmware_inventory(&[])
            .await
            .unwrap()
            .is_empty()
    );
    assert!(pool.sim.create_client_calls().is_empty());
    let delayed_setup = pool.setup_gates.block(&endpoints[0]);
    let delayed_member = pool
        .firmware
        .member_gates
        .block_key(format!("{}/BMC_Firmware", endpoints[0].bmc_ip));
    let mut request = Box::pin(manager.get_firmware_inventory(&endpoints));

    // Both later endpoint slots finish while the first waits for its client.
    poll_firmware_until(request.as_mut(), || {
        pool.firmware.member_calls.lock().unwrap().len() >= 6
    })
    .await;
    assert_eq!(pool.firmware.member_calls.lock().unwrap().len(), 6);
    delayed_setup.send(()).unwrap();
    // The first member then waits while that endpoint's later members finish.
    poll_firmware_until(request.as_mut(), || {
        pool.firmware.member_calls.lock().unwrap().len() >= 9
    })
    .await;
    assert_eq!(pool.firmware.member_calls.lock().unwrap().len(), 9);
    delayed_member.send(()).unwrap();
    let results = request.await.unwrap();

    assert_eq!(
        manager.firmware_inventory_source(),
        Some(FirmwareInventorySource::Redfish)
    );
    assert_eq!(results.len(), 3);
    for (slot, result) in results.iter().enumerate() {
        let host_index = if slot == 0 { 0 } else { 1 };
        assert_eq!(result.mac_address, endpoints[slot].bmc_mac);
        let expected: Vec<_> = ["BMC_Firmware", "other", "BMC_Firmware"]
            .into_iter()
            .map(|id| FirmwareInventoryEntry {
                inventory_id: Some(id.into()),
                firmware: Ok(FirmwareComponent {
                    id: Some(format!("image-{host_index}-{id}")),
                    resource_uri: Some(format!("/reported/{host_index}/image-{id}")),
                    // The client model exposes neither a component name nor an
                    // update target; IDs and resource URIs must not invent them.
                    name: None,
                    update_target: None,
                    description: match (id, host_index) {
                        ("other", 0) => None,
                        ("other", _) => Some(String::new()),
                        _ => Some(format!("Image on host {host_index}")),
                    },
                    version: match (id, host_index) {
                        ("BMC_Firmware", 0) => Some("v0.2.3".into()),
                        ("BMC_Firmware", _) => Some("v1.2.3".into()),
                        (_, 0) => None,
                        _ => Some(String::new()),
                    },
                    release_date: match (id, host_index) {
                        ("other", 0) => Some(String::new()),
                        ("other", _) => None,
                        _ => Some("2026-11-03".into()),
                    },
                }),
            })
            .collect();
        assert_eq!(result.inventory, Ok(expected));
    }
    assert_eq!(
        pool.sim
            .create_client_calls()
            .iter()
            .map(|call| call.host.as_str())
            .collect::<Vec<_>>(),
        ["192.0.2.2", "192.0.2.2", "192.0.2.1"]
    );
    assert_eq!(
        pool.sim.resource_requests(),
        vec!["/redfish/v1/UpdateService/FirmwareInventory"; 3]
    );
    assert!(pool.sim.actions_since(&before).all_hosts().is_empty());
}

/// Parser diagnostics identify the violated rule and requested BMC without
/// exposing collection text. Unsupported references must never be rewritten silently.
#[tokio::test]
async fn firmware_collection_validation_reports_safe_rejection_reasons() {
    const PRIVATE: &str = "firmware-collection-private-value";
    const MEMBER: &str = "/redfish/v1/UpdateService/FirmwareInventory/BMC_Firmware";
    // Complete metadata isolates the individual URI rule in each reference case.
    let with_uri = |uri: &str| {
        serde_json::json!({
            "Members": [{"@odata.id": uri}], "Members@odata.count": 1, "Private": PRIVATE
        })
        .to_string()
    };
    let cases = [
        // Defensive decoding must not put parser text or the raw input into logs.
        (
            "invalid JSON",
            PRIVATE.to_owned(),
            Some("collection_decode_failed"),
        ),
        // Valid JSON of another type cannot stand in for an inventory object.
        (
            "nonobject",
            serde_json::json!([PRIVATE]).to_string(),
            Some("collection_not_object"),
        ),
        // An absent Members key is different from a successfully empty collection.
        (
            "missing Members",
            serde_json::json!({"Members@odata.count": 0, "Private": PRIVATE}).to_string(),
            Some("members_missing"),
        ),
        // A present non-array Members field is malformed rather than empty.
        (
            "malformed Members",
            serde_json::json!({"Members": {}, "Members@odata.count": 0, "Private": PRIVATE}).to_string(),
            Some("members_missing"),
        ),
        // The required count is completeness evidence, even when a member is present.
        (
            "missing count",
            serde_json::json!({"Members": [{"@odata.id": MEMBER}], "Private": PRIVATE}).to_string(),
            Some("member_count_invalid"),
        ),
        // A string count cannot be silently coerced into a valid cardinality.
        (
            "malformed count",
            serde_json::json!({"Members": [], "Members@odata.count": PRIVATE}).to_string(),
            Some("member_count_invalid"),
        ),
        // Fractional counts cannot be truncated to match an otherwise valid list.
        (
            "fractional count",
            serde_json::json!({"Members": [{"@odata.id": MEMBER}], "Members@odata.count": 1.5, "Private": PRIVATE}).to_string(),
            Some("member_count_invalid"),
        ),
        // A member continuation means the supplied page cannot prove completeness.
        (
            "member continuation",
            serde_json::json!({"Members": [], "Members@odata.count": 0, "Members@odata.nextLink": PRIVATE}).to_string(),
            Some("continuation_unsupported"),
        ),
        // A top-level OData continuation has the same incomplete-evidence meaning.
        (
            "top continuation",
            serde_json::json!({"Members": [], "Members@odata.count": 0, "@odata.nextLink": PRIVATE}).to_string(),
            Some("continuation_unsupported"),
        ),
        // The chosen strict policy rejects a supplied continuation even when null.
        (
            "null continuation",
            serde_json::json!({"Members": [], "Members@odata.count": 0, "Members@odata.nextLink": null, "Private": PRIVATE}).to_string(),
            Some("continuation_unsupported"),
        ),
        // Numeric member identities cannot be filtered away from a successful list.
        (
            "malformed member",
            serde_json::json!({"Members": [{"@odata.id": 7}], "Members@odata.count": 1, "Private": PRIVATE}).to_string(),
            Some("member_uri_invalid"),
        ),
        // An empty child would target the collection rather than a firmware member.
        (
            "empty child",
            with_uri("/redfish/v1/UpdateService/FirmwareInventory/"),
            Some("member_uri_invalid"),
        ),
        // Taking only the last segment would discard part of this member's path.
        (
            "nested child",
            with_uri("/redfish/v1/UpdateService/FirmwareInventory/nested/BMC"),
            Some("member_uri_invalid"),
        ),
        // A parent segment would be normalized into a different resource path.
        (
            "dot segment",
            with_uri("/redfish/v1/UpdateService/FirmwareInventory/.."),
            Some("member_uri_invalid"),
        ),
        // Queries change the requested representation instead of naming a literal child.
        (
            "query",
            with_uri("/redfish/v1/UpdateService/FirmwareInventory/BMC?view=other"),
            Some("member_uri_invalid"),
        ),
        // A fragment is not sent as part of the member's HTTP path.
        (
            "fragment",
            with_uri("/redfish/v1/UpdateService/FirmwareInventory/BMC#other"),
            Some("member_uri_invalid"),
        ),
        // Backslashes can be interpreted as path separators by URL parsing.
        (
            "backslash",
            with_uri(r"/redfish/v1/UpdateService/FirmwareInventory/BMC\other"),
            Some("member_uri_invalid"),
        ),
        // Encoded path characters need normalization the reader does not support.
        (
            "percent escape",
            with_uri("/redfish/v1/UpdateService/FirmwareInventory/%2e%2e"),
            Some("member_uri_invalid"),
        ),
        // Whitespace would need escaping or trimming, changing the literal lookup.
        (
            "whitespace",
            with_uri("/redfish/v1/UpdateService/FirmwareInventory/BMC other"),
            Some("member_uri_invalid"),
        ),
        // Control characters can be removed by URL parsing rather than kept verbatim.
        (
            "control",
            with_uri("/redfish/v1/UpdateService/FirmwareInventory/BMC\u{0000}other"),
            Some("member_uri_invalid"),
        ),
        // Existing Dell inventory names use colons within a valid child segment.
        (
            "colon",
            with_uri("/redfish/v1/UpdateService/FirmwareInventory/Disk.Bay.0:Enclosure.Internal.0-1"),
            None,
        ),
    ];
    let endpoint = test_compute_tray_endpoint(1);
    for (name, raw, reason) in cases {
        let (result, logs) = capture_logs_async(async {
            firmware_inventory_ids(&raw, endpoint.bmc_ip, endpoint.bmc_mac)
        })
        .await;

        if let Some(reason) = reason {
            assert_eq!(result, Err(FirmwareReadError::ReadFailed), "{name}");
            assert_eq!(logs.len(), 1, "{name}");
            assert_eq!(
                logs[0].message, "firmware inventory collection rejected",
                "{name}"
            );
            assert_eq!(logs[0].level, tracing::Level::WARN, "{name}");
            assert_eq!(logs[0].field("reason"), Some(reason), "{name}");
            assert_eq!(logs[0].field("bmc_ip_address"), Some("192.0.2.1"), "{name}");
            assert_eq!(
                logs[0].field("bmc_mac_address"),
                Some("00:00:00:00:00:01"),
                "{name}"
            );
        } else {
            assert_eq!(result, Ok(vec!["Disk.Bay.0:Enclosure.Internal.0-1".into()]));
            assert!(logs.is_empty());
        }
        assert!(
            logs.iter().all(|log| !log.message.contains(PRIVATE)
                && log
                    .fields
                    .iter()
                    .all(|(key, value)| !key.contains(PRIVATE) && !value.contains(PRIVATE))),
            "{name}"
        );
    }
}

/// A late invalid member or inconsistent nonempty count must reject the whole
/// collection before the public reader dispatches even its healthy prefix.
#[tokio::test]
async fn firmware_inventory_rejects_incomplete_or_malformed_raw_collections() {
    const MEMBER: &str = "/redfish/v1/UpdateService/FirmwareInventory/BMC_Firmware";
    let cases = [
        // A healthy listed member is still unreadable evidence when the count disagrees.
        (
            "nonempty count mismatch",
            serde_json::json!({"Members": [{"@odata.id": MEMBER}], "Members@odata.count": 2}),
        ),
        // The second URI's healthy suffix must not be remapped into FirmwareInventory.
        (
            "valid prefix then wrong URI",
            serde_json::json!({"Members": [{"@odata.id": MEMBER}, {"@odata.id": "/redfish/v1/Managers/BMC_Firmware"}], "Members@odata.count": 2}),
        ),
    ];
    for (name, collection) in cases {
        let endpoints = [test_compute_tray_endpoint(1)];
        let pool = compute_read_test_pool(&endpoints);
        pool.sim.set_resource(FIRMWARE_INVENTORY_URI, collection);
        let manager = CoreComputeTrayManager::new(pool.clone());
        let before = pool.sim.timepoint();

        let results = manager.get_firmware_inventory(&endpoints).await.unwrap();

        assert_eq!(
            results,
            vec![ComponentFirmwareInventoryResult {
                mac_address: endpoints[0].bmc_mac,
                inventory: Err(FirmwareReadError::ReadFailed),
            }],
            "{name}"
        );
        assert_eq!(
            pool.sim.resource_requests(),
            vec![FIRMWARE_INVENTORY_URI],
            "{name}"
        );
        assert!(
            pool.firmware.member_calls.lock().unwrap().is_empty(),
            "{name}"
        );
        assert!(
            pool.sim.actions_since(&before).all_hosts().is_empty(),
            "{name}"
        );
    }
}

/// Exact accepted boundaries prevent accidental tightening of either the member
/// limit or the lookup byte budget. Repeated IDs still occupy individual slots.
#[test]
fn firmware_collection_validation_accepts_member_and_lookup_limits() {
    let cases = [
        // The final allowed member must not be rejected by an off-by-one check.
        ("256 members", vec!["member".to_owned(); 256]),
        // Individually valid 1 KiB IDs may fill the whole 64 KiB lookup allowance.
        ("64 KiB lookup text", vec!["x".repeat(1024); 64]),
    ];
    let endpoint = test_compute_tray_endpoint(1);
    for (name, ids) in cases {
        let members: Vec<_> = ids.iter().map(|id| serde_json::json!({"@odata.id": format!("/redfish/v1/UpdateService/FirmwareInventory/{id}")})).collect();
        let raw =
            serde_json::json!({"Members": members, "Members@odata.count": ids.len()}).to_string();

        let result = firmware_inventory_ids(&raw, endpoint.bmc_ip, endpoint.bmc_mac);

        assert_eq!(result, Ok(ids), "{name}");
    }
}

/// Every retained Redfish field has an individual byte limit, independent of the
/// aggregate allowance. The public failure test already protects oversized versions.
#[test]
fn firmware_component_rejects_oversized_metadata_fields() {
    /// Changes one field so each row isolates that field's byte cap.
    type SetOversizedFirmwareField = fn(&mut SoftwareInventory);
    let cases: [(&str, SetOversizedFirmwareField); 4] = [
        // Component identity must not bypass the field bound or be shortened.
        ("id", |member| member.id = "i".repeat(1025)),
        // A response URI is independently bounded even when the lookup ID is short.
        ("resource URI", |member| {
            member.odata.odata_id = "u".repeat(1025)
        }),
        // Descriptive text contributes to the same per-field safety limit.
        ("description", |member| {
            member.description = Some("d".repeat(1025))
        }),
        // Release dates remain strings, so they require the same byte bound.
        ("release date", |member| {
            member.release_date = Some("t".repeat(1025))
        }),
    ];
    for (name, oversize) in cases {
        let mut member = SoftwareInventory {
            odata: Default::default(),
            id: "member".into(),
            description: None,
            version: Some("1.2.3".into()),
            release_date: None,
        };
        oversize(&mut member);

        let result = firmware_component(member);

        assert_eq!(result, Err(FirmwareReadError::LimitExceeded), "{name}");
    }
}

/// Setup/list failures make the collection unavailable. A failed or oversized
/// member keeps its slot while its healthy peer remains available, without raw errors.
#[tokio::test]
async fn firmware_inventory_reports_failures_without_losing_healthy_members() {
    /// Each case injects one failure at the collection or member boundary.
    struct Case {
        name: &'static str,
        configure: fn(&ComputeReadTestPool),
        whole_inventory: bool,
        error: FirmwareReadError,
        collection_reason: Option<&'static str>,
    }
    let cases = [
        // Client creation fails before the collection can be observed.
        Case {
            name: "client",
            configure: |pool| pool.sim.set_create_client_error("secret-client-detail"),
            whole_inventory: true,
            error: FirmwareReadError::ReadFailed,
            collection_reason: None,
        },
        // A failed list cannot be presented as an empty successful inventory.
        Case {
            name: "list",
            configure: |pool| pool.sim.fail_resource(FIRMWARE_INVENTORY_URI),
            whole_inventory: true,
            error: FirmwareReadError::ReadFailed,
            collection_reason: None,
        },
        // Cardinality is checked before issuing any follow-up member requests.
        Case {
            name: "member count",
            configure: |pool| set_firmware_test_inventory(pool, &vec!["member".into(); 257]),
            whole_inventory: true,
            error: FirmwareReadError::LimitExceeded,
            collection_reason: Some("member_count_limit"),
        },
        // An overlong lookup ID cannot be clipped or echoed in an error slot.
        Case {
            name: "lookup text",
            configure: |pool| set_firmware_test_inventory(pool, &["x".repeat(1025)]),
            whole_inventory: true,
            error: FirmwareReadError::LimitExceeded,
            collection_reason: Some("member_id_limit"),
        },
        // Individually valid IDs can still exceed the retained collection budget.
        Case {
            name: "total lookup text",
            configure: |pool| set_firmware_test_inventory(pool, &vec!["x".repeat(1024); 65]),
            whole_inventory: true,
            error: FirmwareReadError::LimitExceeded,
            collection_reason: Some("lookup_text_limit"),
        },
        // One unreadable member must not make the other component disappear.
        Case {
            name: "member",
            configure: |pool| {
                pool.firmware.members.lock().unwrap().insert(
                    ("192.0.2.1".into(), "broken".into()),
                    Err("secret-member-detail".into()),
                );
            },
            whole_inventory: false,
            error: FirmwareReadError::ReadFailed,
            collection_reason: None,
        },
        // Truncating a version would fabricate a different firmware version.
        Case {
            name: "member text",
            configure: |pool| {
                pool.firmware.members.lock().unwrap().insert(
                    ("192.0.2.1".into(), "broken".into()),
                    Ok(SoftwareInventory {
                        odata: Default::default(),
                        id: "broken".into(),
                        description: None,
                        version: Some("x".repeat(1025)),
                        release_date: None,
                    }),
                );
            },
            whole_inventory: false,
            error: FirmwareReadError::LimitExceeded,
            collection_reason: None,
        },
    ];
    for case in cases {
        let endpoints = [test_compute_tray_endpoint(1), test_compute_tray_endpoint(2)];
        let pool = compute_read_test_pool(&endpoints);
        set_firmware_test_inventory(&pool, &["BMC_Firmware".into(), "broken".into()]);
        let before = pool.sim.timepoint();
        let manager = CoreComputeTrayManager::new(pool.clone());
        let delayed_first = pool.setup_gates.block(&endpoints[0]);

        let (results, logs) = capture_logs_async(async {
            let mut request = Box::pin(manager.get_firmware_inventory(&endpoints));

            // Capture manual polling too: the healthy endpoint completes before
            // simulator changes inject an error into the still-blocked first one.
            poll_firmware_until(request.as_mut(), || {
                pool.firmware.member_calls.lock().unwrap().len() >= 2
            })
            .await;
            assert_eq!(pool.firmware.member_calls.lock().unwrap().len(), 2);
            (case.configure)(&pool);
            delayed_first.send(()).unwrap();
            request.await.unwrap()
        })
        .await;

        assert_eq!(results.len(), endpoints.len(), "{}", case.name);
        assert_eq!(results[0].mac_address, endpoints[0].bmc_mac);
        if case.whole_inventory {
            assert_eq!(results[0].inventory, Err(case.error), "{}", case.name);
            assert_eq!(
                *pool.firmware.member_calls.lock().unwrap(),
                vec![
                    ("192.0.2.2".into(), "BMC_Firmware".into()),
                    ("192.0.2.2".into(), "broken".into())
                ],
                "{}",
                case.name
            );
        } else {
            let entries = results[0].inventory.as_ref().unwrap();
            assert_eq!(entries.len(), 2, "{}", case.name);
            assert_eq!(entries[0].inventory_id.as_deref(), Some("BMC_Firmware"));
            assert!(entries[0].firmware.is_ok());
            assert_eq!(entries[1].inventory_id.as_deref(), Some("broken"));
            assert_eq!(entries[1].firmware, Err(case.error), "{}", case.name);
        }
        assert_eq!(results[1].mac_address, endpoints[1].bmc_mac);
        let healthy = results[1].inventory.as_ref().unwrap();
        assert_eq!(
            healthy
                .iter()
                .map(|entry| entry.inventory_id.as_deref())
                .collect::<Vec<_>>(),
            [Some("BMC_Firmware"), Some("broken")]
        );
        assert!(
            healthy.iter().all(|entry| entry.firmware.is_ok()),
            "{}",
            case.name
        );
        assert!(
            pool.sim.actions_since(&before).all_hosts().is_empty(),
            "{}",
            case.name
        );
        let warnings: Vec<_> = logs
            .iter()
            .filter(|log| {
                log.level == tracing::Level::WARN && log.target.ends_with("::core_compute_manager")
            })
            .collect();
        if let Some(reason) = case.collection_reason {
            assert_eq!(warnings.len(), 1, "{}", case.name);
            assert_eq!(
                warnings[0].message, "firmware inventory collection rejected",
                "{}",
                case.name
            );
            assert_eq!(warnings[0].field("reason"), Some(reason), "{}", case.name);
            assert_eq!(
                warnings[0].field("bmc_ip_address"),
                Some("192.0.2.1"),
                "{}",
                case.name
            );
            assert_eq!(
                warnings[0].field("bmc_mac_address"),
                Some("00:00:00:00:00:01"),
                "{}",
                case.name
            );
        } else {
            assert!(
                warnings.is_empty(),
                "{}: unexpected reader warning",
                case.name
            );
        }
        for private in ["secret-client-detail", "secret-member-detail"] {
            assert!(
                logs.iter().all(|log| !log.message.contains(private)
                    && log
                        .fields
                        .iter()
                        .all(|(key, value)| !key.contains(private) && !value.contains(private))),
                "{}",
                case.name
            );
        }
    }
}

/// Every retained metadata field and lookup ID consumes the shared byte budget.
/// Listing order decides which records fit, even when an early repeated read is slow.
#[tokio::test]
async fn firmware_inventory_bounds_total_metadata_and_endpoint_count() {
    let endpoints = [test_compute_tray_endpoint(1)];
    let pool = compute_read_test_pool(&endpoints);
    let id = "i".repeat(1024);
    set_firmware_test_inventory(&pool, &vec![id.clone(); 14]);
    pool.firmware.members.lock().unwrap().insert(
        (endpoints[0].bmc_ip.to_string(), id.clone()),
        Ok(SoftwareInventory {
            odata: libredfish::model::ODataLinks {
                odata_id: "u".repeat(1024),
                ..Default::default()
            },
            id: "r".repeat(1024),
            description: Some("d".repeat(1024)),
            version: Some("v".repeat(1024)),
            release_date: Some("t".repeat(1024)),
        }),
    );
    let delayed_first = pool
        .firmware
        .member_gates
        .block_key(format!("{}/{id}", endpoints[0].bmc_ip));
    let manager = CoreComputeTrayManager::new(pool.clone());
    let before = pool.sim.timepoint();
    let mut request = Box::pin(manager.get_firmware_inventory(&endpoints));

    // The first occurrence waits while the other thirteen finish. Completion-order
    // accounting would give its allowance to a later occurrence instead.
    poll_firmware_until(request.as_mut(), || {
        pool.firmware.member_calls.lock().unwrap().len() >= 14
    })
    .await;
    assert_eq!(pool.firmware.member_calls.lock().unwrap().len(), 14);
    delayed_first.send(()).unwrap();
    let results = request.await.unwrap();

    let entries = results[0].inventory.as_ref().unwrap();
    assert_eq!(entries.len(), 14);
    assert!(
        entries
            .iter()
            .all(|entry| entry.inventory_id.as_deref() == Some(id.as_str()))
    );
    // Fourteen 1 KiB lookup IDs plus ten 5 KiB records exactly fill 65,536 bytes.
    // The five fields exposed by the Redfish model carry text; name and update
    // target remain absent. Omitting lookup bytes or any field changes this cutoff.
    assert!(entries[..10].iter().all(|entry| entry.firmware.is_ok()));
    assert!(
        entries[10..]
            .iter()
            .all(|entry| entry.firmware == Err(FirmwareReadError::LimitExceeded))
    );

    assert!(pool.sim.actions_since(&before).all_hosts().is_empty());

    // A fresh pool keeps the endpoint-boundary proof independent of byte accounting.
    let pool = compute_read_test_pool(&endpoints);
    set_firmware_test_inventory(&pool, &[]);
    let manager = CoreComputeTrayManager::new(pool.clone());
    let before = pool.sim.timepoint();

    let accepted = manager
        .get_firmware_inventory(&vec![endpoints[0].clone(); 64])
        .await
        .unwrap();
    let rejected = manager
        .get_firmware_inventory(&vec![endpoints[0].clone(); 65])
        .await;

    assert_eq!(accepted.len(), 64);
    assert!(
        accepted
            .iter()
            .all(|result| result.mac_address == endpoints[0].bmc_mac
                && result.inventory == Ok(vec![]))
    );
    assert!(matches!(
        rejected,
        Err(ComponentManagerError::InvalidArgument(_))
    ));
    assert_eq!(pool.sim.create_client_calls().len(), 64);
    assert_eq!(pool.sim.resource_requests().len(), 64);
    assert!(pool.firmware.member_calls.lock().unwrap().is_empty());
    assert!(pool.sim.actions_since(&before).all_hosts().is_empty());
}

/// Endpoint admission lasts through member reads: at most four BMCs with four
/// members each run. Cancellation drops active waits and leaves queued work unstarted.
#[tokio::test(start_paused = true)]
async fn firmware_inventory_cancellation_bounds_setup_listing_and_member_work() {
    /// Selects which owned future must be dropped with the caller.
    #[derive(Clone, Copy, Debug)]
    enum Phase {
        Setup,
        Listing,
        Members,
    }

    for phase in [Phase::Setup, Phase::Listing, Phase::Members] {
        let endpoints: Vec<_> = (1..=5).map(test_compute_tray_endpoint).collect();
        let pool = compute_read_test_pool(&endpoints);
        let members: Vec<_> = (0..5).map(|i| format!("member-{i}")).collect();
        set_firmware_test_inventory(&pool, &members);
        let (gates, keys) = match phase {
            Phase::Setup => (
                &pool.setup_gates,
                endpoints
                    .iter()
                    .map(|endpoint| endpoint.bmc_ip.to_string())
                    .collect::<Vec<_>>(),
            ),
            Phase::Listing => (
                &pool.firmware.list_gates,
                endpoints
                    .iter()
                    .map(|endpoint| endpoint.bmc_ip.to_string())
                    .collect(),
            ),
            Phase::Members => (
                &pool.firmware.member_gates,
                endpoints
                    .iter()
                    .flat_map(|endpoint| {
                        members
                            .iter()
                            .map(move |id| format!("{}/{id}", endpoint.bmc_ip))
                    })
                    .collect(),
            ),
        };
        let senders: Vec<_> = keys.into_iter().map(|key| gates.block_key(key)).collect();
        let manager = CoreComputeTrayManager::new(pool.clone());
        let before = pool.sim.timepoint();
        let mut request = Box::pin(manager.get_firmware_inventory(&endpoints));
        let active = if matches!(phase, Phase::Members) {
            16
        } else {
            4
        };

        poll_firmware_until(request.as_mut(), || {
            gates.started.load(Ordering::Relaxed) >= active
        })
        .await;
        // Another poll after admission must not release the endpoint slot early
        // or admit a fifth member while all four reads are blocked.
        tokio::task::yield_now().await;
        assert!(futures_util::poll!(request.as_mut()).is_pending());
        assert_eq!(gates.started.load(Ordering::Relaxed), active, "{phase:?}");
        if matches!(phase, Phase::Members) {
            assert_eq!(
                pool.sim
                    .create_client_calls()
                    .iter()
                    .map(|call| call.host.as_str())
                    .collect::<Vec<_>>(),
                ["192.0.2.1", "192.0.2.2", "192.0.2.3", "192.0.2.4"]
            );
            assert_eq!(pool.firmware.member_calls.lock().unwrap().len(), 16);
        }
        drop(request);
        tokio::task::yield_now().await;

        if matches!(phase, Phase::Members) {
            // Gates are in host/member order. The fifth member on each active
            // BMC and every member on the fifth BMC were never admitted.
            for (index, sender) in senders.iter().enumerate() {
                assert_eq!(
                    sender.is_closed(),
                    index / 5 < 4 && index % 5 < 4,
                    "member gate {index}"
                );
            }
        } else {
            assert!(
                senders[..4].iter().all(oneshot::Sender::is_closed),
                "{phase:?}"
            );
            assert!(!senders[4].is_closed(), "{phase:?}: queued work started");
        }
        assert_eq!(gates.started.load(Ordering::Relaxed), active, "{phase:?}");
        assert!(
            pool.sim.actions_since(&before).all_hosts().is_empty(),
            "{phase:?}"
        );
    }
}

/// Setup and queueing consume the original budget; it cannot restart at listing
/// or member reads. Completed evidence survives while late success/error is rejected.
#[tokio::test(start_paused = true)]
async fn firmware_inventory_deadline_preserves_peers_and_rejects_late_results() {
    // Both ready outcomes must lose to expiry, rather than replace it with a
    // successful member or an ordinary read error.
    for late_failure in [false, true] {
        let endpoints: Vec<_> = (1..=5).map(test_compute_tray_endpoint).collect();
        let pool = compute_read_test_pool(&endpoints);
        let ids: Vec<_> = [
            "BMC_Firmware",
            "late",
            "blocked-1",
            "blocked-2",
            "blocked-3",
            "queued",
        ]
        .into_iter()
        .map(str::to_owned)
        .collect();
        set_firmware_test_inventory(&pool, &ids);
        if late_failure {
            pool.firmware.members.lock().unwrap().insert(
                (endpoints[0].bmc_ip.to_string(), "late".into()),
                Err("secret-late-error".into()),
            );
        }
        let mut setup_senders: Vec<_> = endpoints
            .iter()
            .map(|endpoint| Some(pool.setup_gates.block(endpoint)))
            .collect();
        let delayed_listing = pool.firmware.list_gates.block(&endpoints[1]);
        let mut member_senders: Vec<_> = ids[1..]
            .iter()
            .map(|id| {
                pool.firmware
                    .member_gates
                    .block_key(format!("{}/{id}", endpoints[0].bmc_ip))
            })
            .collect();
        let manager = CoreComputeTrayManager::new(pool.clone());
        let before = pool.sim.timepoint();
        let started = Instant::now();

        let (results, logs) = capture_logs_async(async {
            let mut request = Box::pin(manager.get_firmware_inventory(&endpoints));

            poll_firmware_until(request.as_mut(), || {
                pool.setup_gates.started.load(Ordering::Relaxed) >= 4
            })
            .await;
            assert_eq!(pool.setup_gates.started.load(Ordering::Relaxed), 4);
            assert!(pool.sim.create_client_calls().is_empty());
            // Five seconds spent setting up clients must count against later reads.
            tokio::time::advance(Duration::from_secs(5)).await;
            setup_senders[0].take().unwrap().send(()).unwrap();
            setup_senders[1].take().unwrap().send(()).unwrap();
            poll_firmware_until(request.as_mut(), || {
                pool.firmware.member_gates.started.load(Ordering::Relaxed) >= 4
                    && pool.firmware.list_calls.load(Ordering::Relaxed) >= 2
            })
            .await;
            assert_eq!(
                pool.firmware.member_gates.started.load(Ordering::Relaxed),
                4
            );
            assert_eq!(pool.firmware.list_calls.load(Ordering::Relaxed), 2);
            assert_eq!(pool.sim.create_client_calls().len(), 2);

            // The original deadline is neither shortened nor reset after setup.
            tokio::time::advance(Duration::from_millis(24_999)).await;
            assert!(futures_util::poll!(request.as_mut()).is_pending());
            tokio::time::advance(Duration::from_millis(1)).await;
            member_senders.remove(0).send(()).unwrap();
            delayed_listing.send(()).unwrap();
            setup_senders[2].take().unwrap().send(()).unwrap();
            tokio::time::timeout(Duration::from_secs(1), request)
                .await
                .expect("expired firmware request must finish without held gates")
                .unwrap()
        })
        .await;

        assert_eq!(Instant::now() - started, Duration::from_secs(30));
        assert_eq!(results.len(), 5);
        assert_eq!(
            results
                .iter()
                .map(|result| result.mac_address)
                .collect::<Vec<_>>(),
            endpoints
                .iter()
                .map(|endpoint| endpoint.bmc_mac)
                .collect::<Vec<_>>()
        );
        let entries = results[0].inventory.as_ref().unwrap();
        assert_eq!(
            entries
                .iter()
                .map(|entry| entry.inventory_id.as_ref())
                .collect::<Vec<_>>(),
            ids.iter().map(Some).collect::<Vec<_>>()
        );
        assert!(entries[0].firmware.is_ok());
        assert!(
            entries[1..]
                .iter()
                .all(|entry| entry.firmware == Err(FirmwareReadError::DeadlineExceeded))
        );
        assert!(
            results[1..]
                .iter()
                .all(|result| result.inventory == Err(FirmwareReadError::DeadlineExceeded))
        );
        assert!(member_senders[..3].iter().all(oneshot::Sender::is_closed));
        assert!(
            !member_senders[3].is_closed(),
            "queued member started after expiry"
        );
        assert!(setup_senders[3].as_ref().unwrap().is_closed());
        assert!(
            !setup_senders[4].as_ref().unwrap().is_closed(),
            "queued endpoint started after expiry"
        );
        assert_eq!(pool.setup_gates.started.load(Ordering::Relaxed), 4);
        assert_eq!(pool.sim.create_client_calls().len(), 3);
        assert_eq!(
            pool.firmware.list_calls.load(Ordering::Relaxed),
            2,
            "late setup admitted a collection read"
        );
        let calls = pool.firmware.member_calls.lock().unwrap();
        assert_eq!(calls.len(), 5);
        assert!(
            calls
                .iter()
                .all(|(host, id)| host == "192.0.2.1" && id != "queued")
        );
        assert!(pool.sim.actions_since(&before).all_hosts().is_empty());
        assert!(
            logs.iter().all(|log| log.level != tracing::Level::WARN
                || !log.target.ends_with("::core_compute_manager")),
            "deadline path duplicated an I/O diagnostic"
        );
        assert!(logs.iter().all(|log| {
            !log.message.contains("secret-late-error")
                && log.fields.iter().all(|(key, value)| {
                    !key.contains("secret-late-error") && !value.contains("secret-late-error")
                })
        }));
    }
}
