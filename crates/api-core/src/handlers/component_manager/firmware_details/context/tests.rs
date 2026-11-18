// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

use bmc_vendor::BMCVendor;
use carbide_uuid::machine::{MachineId, MachineIdSource, MachineType};
use db::test_support::postgres::wait_for_blocked_query;
use model::address_selection_strategy::AddressSelectionStrategy;
use model::expected_machine::ExpectedMachineData;
use model::expected_power_shelf::ExpectedPowerShelf;
use model::expected_switch::ExpectedSwitch;
use model::firmware::{FirmwareComponent, FirmwareComponentType, FirmwareEntry};
use model::machine::{CURRENT_STATE_MODEL_VERSION, ManagedHostState};
use model::machine_interface_address::MachineInterfaceAssociation;
use model::power_shelf::NewPowerShelf;
use model::rack::{
    FirmwareUpgradeJob, MaintenanceActivity, MaintenanceScope, RackConfig, RackState,
};
use model::rack_type::{
    RackCapabilitiesSet, RackCapabilityCompute, RackCapabilityPowerShelf, RackCapabilitySwitch,
};
use model::switch::{NewSwitch, SwitchConfig};
use model::test_support::power_shelf_config;

use super::super::{FirmwareRequestSource, host_expectations, machine};
use super::*;

type TestResult<T = ()> = Result<T, Box<dyn std::error::Error>>;

/// Produces distinct, valid host identities without relying on random fixture IDs.
fn host_id(seed: u8) -> HostMachineId {
    HostMachineId::try_from(MachineId::new(
        MachineIdSource::ProductBoardChassisSerial,
        [seed; 32],
        MachineType::Host,
    ))
    .expect("fixture host ID")
}

/// Keeps an explicit BMC filter so the fixture detects lost request selectors.
fn activities(object_id: &str) -> Vec<MaintenanceActivity> {
    vec![MaintenanceActivity::FirmwareUpgrade {
        firmware_version: Some(format!(r#"{{"Id":"{object_id}"}}"#)),
        components: vec!["BMC".into()],
        force_update: false,
    }]
}

/// Uses full-rack scope with an unknown request time.
fn rack_config(object_id: &str) -> RackConfig {
    RackConfig {
        maintenance_requested: Some(MaintenanceScope {
            activities: activities(object_id),
            ..Default::default()
        }),
        ..Default::default()
    }
}

/// Supplies a runtime default independently of the rack job's frozen target.
fn host_override(model: &str, version: &str) -> HostFirmwareConfig {
    HostFirmwareConfig {
        vendor: BMCVendor::Dell,
        model: model.into(),
        ordering: vec![FirmwareComponentType::Bmc],
        components: HashMap::from([(
            FirmwareComponentType::Bmc,
            FirmwareComponent {
                known_firmware: vec![FirmwareEntry {
                    version: version.into(),
                    default: true,
                    ..Default::default()
                }],
                ..Default::default()
            },
        )]),
        ..Default::default()
    }
}

/// Three hosts, two switches and one shelf include missing dependencies without
/// losing the healthy peers. The third host and its rack stay outside selections.
struct FirmwareContextFixture {
    rack_id: RackId,
    profile_id: RackProfileId,
    hosts: [HostMachineId; 3],
    host_ip: IpAddr,
    missing_report_ip: IpAddr,
    missing_rack_id: RackId,
    unconfigured_rack_id: RackId,
    unconfigured_profile_id: RackProfileId,
    unrelated_rack_id: RackId,
    unrelated_profile_id: RackProfileId,
    switch_id: SwitchId,
    endpointless_switch_id: SwitchId,
    shelf_id: PowerShelfId,
}

/// Uses the UNDERLAY segment seeded by `create_test_env`, retaining the chosen
/// owner and interface type so alias assertions exercise the real bulk reader.
async fn create_context_interface(
    txn: &mut sqlx::PgConnection,
    seed: u8,
    association: MachineInterfaceAssociation,
    interface_type: InterfaceType,
) -> TestResult<IpAddr> {
    let segment = db::network_segment::find_by_name(&mut *txn, "UNDERLAY").await?;
    let row = db::machine_interface::create_with_type(
        &mut *txn,
        &[segment],
        &MacAddress::new([2, 0, 0, 0, 0, seed]),
        false,
        AddressSelectionStrategy::NextAvailableIp,
        interface_type,
        None,
    )
    .await?;
    db::machine_interface::associate_interface_with_machine(&row.id, association, txn).await?;
    Ok(*row.addresses.first().expect("allocated interface address"))
}

/// Persists pending records and profile counts matching the fixture inventory.
async fn seed_firmware_context(
    pool: &sqlx::PgPool,
    config: &mut CarbideConfig,
) -> TestResult<FirmwareContextFixture> {
    let rack_id = RackId::new("firmware-context");
    let profile_id = RackProfileId::new("context-profile");
    let missing_rack_id = RackId::new("firmware-context-absent");
    let unconfigured_rack_id = RackId::new("firmware-context-unconfigured");
    let unconfigured_profile_id = RackProfileId::new("unconfigured-profile");
    let unrelated_rack_id = RackId::new("firmware-context-unrelated");
    let unrelated_profile_id = RackProfileId::new("unrelated-profile");
    config.rack_profiles.rack_profiles = HashMap::from([
        (
            profile_id.to_string(),
            RackProfile {
                rack_capabilities: RackCapabilitiesSet {
                    compute: RackCapabilityCompute {
                        count: 1,
                        ..Default::default()
                    },
                    switch: RackCapabilitySwitch {
                        count: 1,
                        ..Default::default()
                    },
                    power_shelf: RackCapabilityPowerShelf {
                        count: 1,
                        ..Default::default()
                    },
                },
                ..Default::default()
            },
        ),
        (
            unrelated_profile_id.to_string(),
            RackProfile {
                rack_capabilities: RackCapabilitiesSet {
                    compute: RackCapabilityCompute {
                        count: 1,
                        ..Default::default()
                    },
                    ..Default::default()
                },
                attributes: HashMap::from([("fixture".into(), "unselected-owner".into())]),
                ..Default::default()
            },
        ),
    ]);
    let hosts = [host_id(201), host_id(202), host_id(204)];
    let switch_id = SwitchId::from(uuid::Uuid::from_u128(201));
    let endpointless_switch_id = SwitchId::from(uuid::Uuid::from_u128(203));
    let shelf_id = PowerShelfId::from(uuid::Uuid::from_u128(201));
    let mut txn = pool.begin().await?;
    let rack = db::rack::create(
        &mut txn,
        &rack_id,
        Some(&profile_id),
        &rack_config("old"),
        None,
    )
    .await?;
    // A Ready rack with a saved request has pending intent before admission.
    assert!(matches!(
        db::rack::try_update_controller_state(
            &mut txn,
            &rack_id,
            rack.controller_state.version,
            rack.controller_state.version.increment(),
            &RackState::Ready,
        )
        .await?,
        db::ConditionalWrite::Applied(())
    ));
    for (owner, profile, target) in [
        (
            &unconfigured_rack_id,
            &unconfigured_profile_id,
            "unconfigured",
        ),
        (&unrelated_rack_id, &unrelated_profile_id, "unrelated"),
    ] {
        db::rack::create(&mut txn, owner, Some(profile), &rack_config(target), None).await?;
    }
    for (host, owner) in [
        (hosts[0], &rack_id),
        (hosts[1], &missing_rack_id),
        (hosts[2], &unrelated_rack_id),
    ] {
        db::machine::create(
            &mut txn,
            None,
            host.as_machine_id(),
            ManagedHostState::Ready,
            Some(&ExpectedMachineData {
                rack_id: Some(owner.clone()),
                ..Default::default()
            }),
            CURRENT_STATE_MODEL_VERSION,
        )
        .await?;
    }
    let host_ip = create_context_interface(
        &mut txn,
        11,
        MachineInterfaceAssociation::Machine(hosts[0].into()),
        InterfaceType::Bmc,
    )
    .await?;
    create_context_interface(
        &mut txn,
        12,
        MachineInterfaceAssociation::Machine(hosts[0].into()),
        InterfaceType::Bmc,
    )
    .await?;
    create_context_interface(
        &mut txn,
        13,
        MachineInterfaceAssociation::Machine(hosts[0].into()),
        InterfaceType::Data,
    )
    .await?;
    let missing_report_ip = create_context_interface(
        &mut txn,
        14,
        MachineInterfaceAssociation::Machine(hosts[1].into()),
        InterfaceType::Bmc,
    )
    .await?;
    // An unselected owner's BMC must never enter the requested alias map.
    create_context_interface(
        &mut txn,
        15,
        MachineInterfaceAssociation::Machine(hosts[2].into()),
        InterfaceType::Bmc,
    )
    .await?;
    // Creation timestamps share the transaction clock. Give the selected BMC
    // a distinct earlier timestamp so its report lookup is unambiguous.
    sqlx::query("UPDATE machine_interfaces SET created = created - interval '1 second' WHERE mac_address = $1")
        .bind(MacAddress::new([2, 0, 0, 0, 0, 11]))
        .execute(&mut *txn)
        .await?;
    db::explored_endpoints::insert(
        host_ip,
        &EndpointExplorationReport {
            vendor: Some(BMCVendor::Dell),
            model: Some("context-model".into()),
            ..Default::default()
        },
        false,
        &mut txn,
    )
    .await?;
    db::host_machine_update::trigger_host_reprovisioning_request(
        &mut txn,
        "rack-firmware-context",
        &hosts[0],
    )
    .await?;
    // The second host retains its request despite having no report or rack row.
    db::host_machine_update::trigger_host_reprovisioning_request(
        &mut txn,
        "operator-without-report",
        &hosts[1],
    )
    .await?;
    db::host_firmware_config::upsert(&mut txn, &host_override("context-model", "old")).await?;
    db::rack::update_firmware_upgrade_job(
        &mut txn,
        &rack_id,
        Some(&FirmwareUpgradeJob {
            firmware_id: Some("frozen-object".into()),
            ..Default::default()
        }),
    )
    .await?;
    let switch_mac = MacAddress::new([2, 0, 0, 0, 0, 21]);
    db::expected_switch::create(
        &mut txn,
        ExpectedSwitch {
            bmc_mac_address: switch_mac,
            ..Default::default()
        },
    )
    .await?;
    for (id, bmc_mac, owner) in [
        (switch_id, Some(switch_mac), &rack_id),
        (endpointless_switch_id, None, &unconfigured_rack_id),
    ] {
        db::switch::create(
            &mut txn,
            &NewSwitch {
                id,
                config: SwitchConfig {
                    name: id.to_string(),
                    enable_nmxc: false,
                    fabric_manager_config: None,
                },
                bmc_mac_address: bmc_mac,
                metadata: None,
                rack_id: Some(owner.clone()),
                slot_number: None,
                tray_index: None,
            },
        )
        .await?;
        db::switch::set_switch_reprovisioning_requested(
            &mut txn,
            id,
            &format!("rack-{owner}"),
            activities("old"),
        )
        .await?;
    }
    create_context_interface(
        &mut txn,
        21,
        MachineInterfaceAssociation::Switch(switch_id),
        InterfaceType::Bmc,
    )
    .await?;
    let shelf_mac = MacAddress::new([2, 0, 0, 0, 0, 31]);
    db::expected_power_shelf::create(
        &mut txn,
        ExpectedPowerShelf {
            bmc_mac_address: shelf_mac,
            ..Default::default()
        },
    )
    .await?;
    db::power_shelf::create(
        &mut txn,
        &NewPowerShelf {
            id: shelf_id,
            config: power_shelf_config("context-shelf"),
            bmc_mac_address: Some(shelf_mac),
            metadata: None,
            rack_id: Some(rack_id.clone()),
        },
    )
    .await?;
    create_context_interface(
        &mut txn,
        31,
        MachineInterfaceAssociation::PowerShelf(shelf_id),
        InterfaceType::Bmc,
    )
    .await?;
    db::power_shelf::set_power_shelf_reprovisioning_requested(
        &mut txn,
        shelf_id,
        "rack-firmware-context",
        activities("old"),
    )
    .await?;
    txn.commit().await?;
    Ok(FirmwareContextFixture {
        rack_id,
        profile_id,
        hosts,
        host_ip,
        missing_report_ip,
        missing_rack_id,
        unconfigured_rack_id,
        unconfigured_profile_id,
        unrelated_rack_id,
        unrelated_profile_id,
        switch_id,
        endpointless_switch_id,
        shelf_id,
    })
}

/// Requires a populated host slot so assertions cannot pass on a missing member.
fn loaded_host(context: &FirmwareReadContext, index: usize) -> &Arc<HostMachine> {
    let FirmwareReadEntry::Machine {
        machine: Some(host),
        ..
    } = &context.entries[index]
    else {
        panic!("expected managed host slot")
    };
    host
}

/// Exercises the retained projector with the catalog and report loaded together.
fn configured_version(context: &FirmwareReadContext, fixture: &FirmwareContextFixture) -> String {
    host_expectations(
        context.host_catalog.as_ref().expect("host catalog"),
        &context.host_reports[&fixture.host_ip],
    )
    .expect("runtime expectation")
    .versions
    .versions[&FirmwareComponentType::Bmc]
        .clone()
}

/// The closed pool exposes database access. Empty input and the 65th slot finish
/// admission first; a caller deadline also precedes a ready acquisition failure.
#[tokio::test]
async fn admission_finishes_before_database_access() -> TestResult {
    let pool = sqlx::PgPool::connect_lazy("postgresql://localhost/unused")?;
    pool.close().await;
    let config = crate::test_support::default_config::get();
    let empty = load(&pool, &config, FirmwareTargets::Machines(&[]), None).await?;
    assert!(empty.entries.is_empty());
    assert!(empty.host_catalog.is_none());

    let ids = [host_id(203); 65];
    let result = load(&pool, &config, FirmwareTargets::Machines(&ids), None).await;
    assert!(matches!(result, Err(CarbideError::InvalidArgument(_))));

    let ids = [SwitchId::from(uuid::Uuid::from_u128(201))];
    let result = load(&pool, &config, FirmwareTargets::Switches(&ids), None).await;
    assert!(matches!(result, Err(CarbideError::DBError(_))));

    let result = load(
        &pool,
        &config,
        FirmwareTargets::Switches(&ids),
        Some(Instant::now()),
    )
    .await;
    assert!(matches!(result, Err(CarbideError::Internal { message })
        if message == "firmware details preparation deadline exceeded"));
    Ok(())
}

/// Real bulk readers retain slots and saved work when dependencies are absent.
/// The runtime-only model also remains available with a missing metadata directory.
#[crate::sqlx_test]
async fn context_preserves_members_aliases_sources_and_input_slots(
    pool: sqlx::PgPool,
) -> TestResult {
    let env = crate::tests::create_test_env(pool.clone()).await;
    let mut config = env.api.runtime_config.as_ref().clone();
    let directory = tempfile::tempdir()?;
    config.firmware_global.firmware_directory = directory.path().join("absent");
    // This model comes only from runtime overrides, independently of local metadata.
    config.host_models.clear();
    let fixture = seed_firmware_context(&pool, &mut config).await?;
    let missing_host = host_id(203);
    let hosts = [
        fixture.hosts[1],
        missing_host,
        fixture.hosts[0],
        fixture.hosts[1],
    ];
    let switches = [
        SwitchId::from(uuid::Uuid::from_u128(202)),
        fixture.endpointless_switch_id,
        fixture.switch_id,
        fixture.switch_id,
    ];
    let shelves = [
        PowerShelfId::from(uuid::Uuid::from_u128(202)),
        fixture.shelf_id,
        fixture.shelf_id,
    ];
    let primary_racks = [fixture.rack_id.clone()];
    let switch_racks = [
        fixture.rack_id.clone(),
        fixture.unconfigured_rack_id.clone(),
    ];
    struct Case<'a> {
        scenario: &'static str,
        targets: FirmwareTargets<'a>,
        expected_ids: Vec<String>,
        expected_racks: &'a [RackId],
        check: fn(&str, &FirmwareReadContext, &FirmwareContextFixture),
    }
    let cases = [
        Case {
            scenario: "host slots and absent rack/report",
            targets: FirmwareTargets::Machines(&hosts),
            expected_ids: hosts.iter().map(ToString::to_string).collect(),
            expected_racks: &primary_racks,
            check: |scenario, context, fixture| {
                assert!(
                    matches!(
                        &context.entries[1],
                        FirmwareReadEntry::Machine { machine: None, .. }
                    ),
                    "{scenario}"
                );
                assert!(
                    Arc::ptr_eq(loaded_host(context, 0), loaded_host(context, 3)),
                    "{scenario}"
                );
                assert_eq!(context.bmc_aliases.len(), 2, "{scenario}");
                assert!(
                    !context.bmc_aliases.contains_key(&fixture.hosts[2]),
                    "{scenario}"
                );
                assert_eq!(
                    context.bmc_aliases[&fixture.hosts[0]],
                    [11, 12].map(|seed| MacAddress::new([2, 0, 0, 0, 0, seed])),
                    "{scenario}"
                );
                assert_eq!(
                    context.bmc_aliases[&fixture.hosts[1]],
                    [MacAddress::new([2, 0, 0, 0, 0, 14])],
                    "{scenario}"
                );

                let incomplete = loaded_host(context, 0);
                assert_eq!(incomplete.id, fixture.hosts[1], "{scenario}");
                assert_eq!(
                    incomplete.rack_id.as_ref(),
                    Some(&fixture.missing_rack_id),
                    "{scenario}"
                );
                assert_eq!(
                    incomplete.status.bmc_info.ip,
                    Some(fixture.missing_report_ip),
                    "{scenario}"
                );
                assert_eq!(
                    incomplete.state.value,
                    ManagedHostState::Ready,
                    "{scenario}"
                );
                assert_eq!(
                    incomplete
                        .host_reprovision_requested
                        .as_ref()
                        .expect("saved request")
                        .initiator,
                    "operator-without-report",
                    "{scenario}"
                );
                assert!(
                    !context.racks.contains_key(&fixture.missing_rack_id),
                    "{scenario}"
                );
                assert!(
                    !context
                        .host_reports
                        .contains_key(&fixture.missing_report_ip),
                    "{scenario}"
                );
                assert_eq!(context.host_reports.len(), 1, "{scenario}");
                assert_eq!(configured_version(context, fixture), "old", "{scenario}");

                let actions = machine(
                    loaded_host(context, 2),
                    Some(&context.racks[&fixture.rack_id]),
                );
                assert_eq!(
                    actions
                        .outstanding_requests
                        .iter()
                        .map(|request| &request.source)
                        .collect::<Vec<_>>(),
                    [
                        &FirmwareRequestSource::RackMaintenance,
                        &FirmwareRequestSource::HostReprovision
                    ],
                    "{scenario}"
                );
            },
        },
        Case {
            scenario: "switch slots and absent endpoint/profile",
            targets: FirmwareTargets::Switches(&switches),
            expected_ids: switches.iter().map(ToString::to_string).collect(),
            expected_racks: &switch_racks,
            check: |scenario, context, fixture| {
                let [
                    FirmwareReadEntry::Switch {
                        switch: None,
                        endpoint: None,
                        ..
                    },
                    FirmwareReadEntry::Switch {
                        switch: Some(incomplete),
                        endpoint: None,
                        ..
                    },
                    FirmwareReadEntry::Switch {
                        switch: Some(left),
                        endpoint: Some(left_endpoint),
                        ..
                    },
                    FirmwareReadEntry::Switch {
                        switch: Some(right),
                        endpoint: Some(right_endpoint),
                        ..
                    },
                ] = context.entries.as_slice()
                else {
                    panic!("{scenario}: expected missing, incomplete and repeated healthy switches")
                };
                assert_eq!(incomplete.id, fixture.endpointless_switch_id, "{scenario}");
                assert_eq!(
                    incomplete.rack_id.as_ref(),
                    Some(&fixture.unconfigured_rack_id),
                    "{scenario}"
                );
                assert_eq!(
                    context.racks[&fixture.unconfigured_rack_id]
                        .rack_profile_id
                        .as_ref(),
                    Some(&fixture.unconfigured_profile_id),
                    "{scenario}"
                );
                assert!(
                    !context
                        .profiles
                        .contains_key(&fixture.unconfigured_profile_id),
                    "{scenario}"
                );
                for switch in [incomplete, left] {
                    assert_eq!(
                        switch
                            .switch_reprovisioning_requested
                            .as_ref()
                            .expect("saved request")
                            .activities,
                        activities("old"),
                        "{scenario}"
                    );
                }
                assert_eq!(left.id, fixture.switch_id, "{scenario}");
                assert_eq!(left_endpoint.switch_id, fixture.switch_id, "{scenario}");
                assert!(Arc::ptr_eq(left, right), "{scenario}");
                assert!(Arc::ptr_eq(left_endpoint, right_endpoint), "{scenario}");
                assert!(context.host_catalog.is_none(), "{scenario}");
                assert!(context.host_reports.is_empty(), "{scenario}");
                assert!(context.bmc_aliases.is_empty(), "{scenario}");
            },
        },
        Case {
            scenario: "shelf slots",
            targets: FirmwareTargets::Shelves(&shelves),
            expected_ids: shelves.iter().map(ToString::to_string).collect(),
            expected_racks: &primary_racks,
            check: |scenario, context, fixture| {
                let [
                    FirmwareReadEntry::Shelf {
                        shelf: None,
                        endpoint: None,
                        ..
                    },
                    FirmwareReadEntry::Shelf {
                        shelf: Some(left),
                        endpoint: Some(left_endpoint),
                        ..
                    },
                    FirmwareReadEntry::Shelf {
                        shelf: Some(right),
                        endpoint: Some(right_endpoint),
                        ..
                    },
                ] = context.entries.as_slice()
                else {
                    panic!("{scenario}: expected missing and repeated healthy shelves")
                };
                assert_eq!(left.id, fixture.shelf_id, "{scenario}");
                assert_eq!(left_endpoint.power_shelf_id, fixture.shelf_id, "{scenario}");
                assert_eq!(
                    left.power_shelf_reprovisioning_requested
                        .as_ref()
                        .expect("saved request")
                        .activities,
                    activities("old"),
                    "{scenario}"
                );
                assert!(Arc::ptr_eq(left, right), "{scenario}");
                assert!(Arc::ptr_eq(left_endpoint, right_endpoint), "{scenario}");
                assert!(context.host_catalog.is_none(), "{scenario}");
                assert!(context.host_reports.is_empty(), "{scenario}");
                assert!(context.bmc_aliases.is_empty(), "{scenario}");
            },
        },
    ];
    for case in cases {
        let context = load(&pool, &config, case.targets, None).await?;

        let ids = context
            .entries
            .iter()
            .map(|entry| match entry {
                FirmwareReadEntry::Machine { machine_id, .. } => machine_id.to_string(),
                FirmwareReadEntry::Switch { switch_id, .. } => switch_id.to_string(),
                FirmwareReadEntry::Shelf { power_shelf_id, .. } => power_shelf_id.to_string(),
            })
            .collect::<Vec<_>>();
        assert_eq!(ids, case.expected_ids, "{}", case.scenario);
        assert_eq!(
            context.racks.keys().collect::<HashSet<_>>(),
            case.expected_racks.iter().collect::<HashSet<_>>(),
            "{}",
            case.scenario
        );
        assert_eq!(
            context.profiles.keys().collect::<HashSet<_>>(),
            HashSet::from([&fixture.profile_id]),
            "{}",
            case.scenario
        );
        assert!(
            !context.racks.contains_key(&fixture.unrelated_rack_id),
            "{}",
            case.scenario
        );
        assert!(
            !context.profiles.contains_key(&fixture.unrelated_profile_id),
            "{}",
            case.scenario
        );
        assert_eq!(
            context.profiles[&fixture.profile_id],
            config.rack_profiles.rack_profiles[fixture.profile_id.as_str()],
            "{}",
            case.scenario
        );
        let rack = &context.racks[&fixture.rack_id];
        assert_eq!(
            rack.controller_state.value,
            RackState::Ready,
            "{}",
            case.scenario
        );
        assert_eq!(
            rack.config
                .maintenance_requested
                .as_ref()
                .expect("scope")
                .activities,
            activities("old"),
            "{}",
            case.scenario
        );
        assert_eq!(
            rack.firmware_upgrade_job
                .as_ref()
                .expect("job")
                .firmware_id
                .as_deref(),
            Some("frozen-object"),
            "{}",
            case.scenario
        );
        (case.check)(case.scenario, &context, &fixture);
    }

    // Empty dependent lookup sets must not widen into an inventory-wide read.
    let missing = [missing_host];
    let context = load(&pool, &config, FirmwareTargets::Machines(&missing), None).await?;

    assert!(matches!(context.entries.as_slice(),
        [FirmwareReadEntry::Machine { machine_id, machine: None }] if *machine_id == missing_host));
    assert!(context.racks.is_empty());
    assert!(context.profiles.is_empty());
    assert!(context.bmc_aliases.is_empty());
    assert!(context.host_reports.is_empty());
    assert!(context.host_catalog.is_some());

    // The admitted boundary counts and preserves every original repeated slot.
    let full = [fixture.hosts[0]; 64];
    let context = load(&pool, &config, FirmwareTargets::Machines(&full), None).await?;

    assert_eq!(context.entries.len(), 64);
    for entry in &context.entries {
        let FirmwareReadEntry::Machine {
            machine_id,
            machine: Some(host),
        } = entry
        else {
            panic!("every admitted slot must retain its managed host")
        };
        assert_eq!(*machine_id, fixture.hosts[0]);
        assert_eq!(host.id, fixture.hosts[0]);
        assert!(Arc::ptr_eq(loaded_host(&context, 0), host));
    }
    Ok(())
}

/// Keeps one observable session and an acquisition timeout beyond the loader budget.
async fn single_connection_pool(pool: &sqlx::PgPool) -> Result<sqlx::PgPool, sqlx::Error> {
    sqlx::postgres::PgPoolOptions::new()
        .max_connections(1)
        .acquire_timeout(Duration::from_secs(30))
        .connect_with(pool.connect_options().as_ref().clone())
        .await
}

/// The host read establishes the snapshot before report selection blocks. After
/// an atomic writer commits, report/rack/override reads must still agree on the
/// old model and default; a fresh load must see the complete replacement.
#[crate::sqlx_test]
async fn context_snapshot_is_consistent_and_releases_its_healthy_session(
    pool: sqlx::PgPool,
) -> TestResult {
    let env = crate::tests::create_test_env(pool.clone()).await;
    let mut config = env.api.runtime_config.as_ref().clone();
    // No local metadata participates in this transaction-consistency proof.
    config.firmware_global.firmware_directory = Default::default();
    config.host_models.clear();
    let fixture = seed_firmware_context(&pool, &mut config).await?;
    let reader = single_connection_pool(&pool).await?;
    let original_pid: i32 = sqlx::query_scalar("SELECT pg_backend_pid()")
        .fetch_one(&reader)
        .await?;
    let mut writer = pool.begin().await?;
    sqlx::query("LOCK TABLE explored_endpoints IN ACCESS EXCLUSIVE MODE")
        .execute(&mut *writer)
        .await?;
    let blocker_pid: i32 = sqlx::query_scalar("SELECT pg_backend_pid()")
        .fetch_one(&mut *writer)
        .await?;
    let ids = [fixture.hosts[0]];
    let mut read = Box::pin(load(
        &reader,
        &config,
        FirmwareTargets::Machines(&ids),
        None,
    ));
    let blocked_pid = tokio::select! {
        result = tokio::time::timeout(
            Duration::from_secs(10),
            wait_for_blocked_query(&pool, blocker_pid, "FROM explored_endpoints"),
        ) => result.expect("report read must block before preparation completes"),
        result = &mut read => match result {
            Ok(_) => panic!("snapshot finished before its report read blocked"),
            Err(error) => panic!("snapshot failed before its report read blocked: {error:?}"),
        },
    };
    assert_eq!(blocked_pid, original_pid);

    // The writer owns the table lock. Replace both selection inputs so a mixed
    // report/catalog snapshot cannot retain a matching model by accident.
    let report_version = db::explored_endpoints::find_by_ips(&mut *writer, vec![fixture.host_ip])
        .await?
        .pop()
        .expect("seeded report")
        .report_version;
    assert!(matches!(
        db::explored_endpoints::try_update(
            fixture.host_ip,
            report_version,
            &EndpointExplorationReport {
                vendor: Some(BMCVendor::Dell),
                model: Some("replacement-model".into()),
                ..Default::default()
            },
            false,
            &mut writer,
        )
        .await?,
        db::ConditionalWrite::Applied(())
    ));
    db::host_machine_update::trigger_host_reprovisioning_request(
        &mut writer,
        "replacement",
        &fixture.hosts[0],
    )
    .await?;
    db::rack::update(&mut writer, &fixture.rack_id, &rack_config("new")).await?;
    db::host_firmware_config::delete(&mut writer, "Dell", "context-model").await?;
    db::host_firmware_config::upsert(&mut writer, &host_override("replacement-model", "new"))
        .await?;
    writer.commit().await?;
    let old = read.await?;

    assert_eq!(
        loaded_host(&old, 0)
            .host_reprovision_requested
            .as_ref()
            .expect("request")
            .initiator,
        "rack-firmware-context"
    );
    assert_eq!(
        old.racks[&fixture.rack_id]
            .config
            .maintenance_requested
            .as_ref()
            .expect("scope")
            .activities,
        activities("old")
    );
    assert_eq!(
        old.host_reports[&fixture.host_ip].model.as_deref(),
        Some("context-model")
    );
    assert_eq!(configured_version(&old, &fixture), "old");
    // Reuse the exact session immediately at the loader's real return boundary.
    let reused_pid: i32 = sqlx::query_scalar("SELECT pg_backend_pid()")
        .fetch_one(&reader)
        .await?;
    assert_eq!(reused_pid, original_pid);
    let fresh = load(&reader, &config, FirmwareTargets::Machines(&ids), None).await?;

    assert_eq!(
        loaded_host(&fresh, 0)
            .host_reprovision_requested
            .as_ref()
            .expect("request")
            .initiator,
        "replacement"
    );
    assert_eq!(
        fresh.racks[&fixture.rack_id]
            .config
            .maintenance_requested
            .as_ref()
            .expect("scope")
            .activities,
        activities("new")
    );
    assert_eq!(
        fresh.host_reports[&fixture.host_ip].model.as_deref(),
        Some("replacement-model")
    );
    assert_eq!(configured_version(&fresh, &fixture), "new");
    reader.close().await;
    Ok(())
}

/// A held slot isolates the earlier caller deadline and the default DB budget.
/// Both must wait until their own boundary, then expire before the pool timeout.
#[crate::sqlx_test]
async fn preparation_deadlines_bound_pool_acquisition(pool: sqlx::PgPool) -> TestResult {
    let reader = single_connection_pool(&pool).await?;
    let held = reader.acquire().await?;
    let config = crate::test_support::default_config::get();
    let ids = [SwitchId::from(uuid::Uuid::from_u128(201))];
    for (scenario, caller_after, expected_budget) in [
        (
            "earlier caller",
            Some(Duration::from_millis(25)),
            Duration::from_millis(25),
        ),
        ("default DB budget", None, Duration::from_secs(5)),
    ] {
        tokio::time::pause();
        let caller_deadline = caller_after.map(|duration| Instant::now() + duration);
        let mut read = Box::pin(load(
            &reader,
            &config,
            FirmwareTargets::Switches(&ids),
            caller_deadline,
        ));
        assert!(
            futures::poll!(&mut read).is_pending(),
            "{scenario}: initial acquisition"
        );

        tokio::time::advance(expected_budget - Duration::from_millis(1)).await;
        let before = futures::poll!(&mut read);
        let pending_before = before.is_pending();
        // Tokio rounds timer wakes to milliseconds. Check either side of the
        // literal policy boundary while acquisition cannot perform SQL.
        let result = if pending_before {
            tokio::time::advance(Duration::from_millis(2)).await;
            futures::poll!(&mut read)
        } else {
            before
        };
        drop(read);
        tokio::time::resume();

        assert!(pending_before, "{scenario}: expired before its budget");
        assert!(
            matches!(result,
            std::task::Poll::Ready(Err(CarbideError::Internal { message }))
                if message == "firmware details preparation deadline exceeded"),
            "{scenario}"
        );
    }
    drop(held);
    reader.close().await;
    Ok(())
}
