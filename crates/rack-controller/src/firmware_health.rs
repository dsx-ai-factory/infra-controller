//! Rack-only firmware health for ingested, non-deleted trays, using site explorer's
//! stored BMC inventory. Compute, switch, and power shelf inventories are compared
//! independently by exact inventory ID and version, without issuing BMC requests.
//! Unknown versions do not create mismatches or clear existing ones. An alert clears
//! when all remaining trays report agreement, or fewer than two trays remain.
//! Component evidence is retained separately; rack aggregate health summarizes it
//! as one alert listing the number and details of outstanding mismatches.

use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::net::IpAddr;

use health_report::{HealthAlertClassification, HealthProbeAlert, HealthProbeId, HealthReport};
use model::rack::Rack;
use model::site_explorer::EndpointExplorationReport;
use state_controller::state_handler::StateHandlerError;

pub(super) const SOURCE: &str = HealthReport::RACK_FIRMWARE_SOURCE;

#[derive(Debug)]
struct TrayFirmware<'a> {
    device_id: &'a str,
    address: Option<IpAddr>,
    report: Option<&'a EndpointExplorationReport>,
}

#[derive(Clone, Copy)]
enum TrayType {
    Compute,
    Switch,
    PowerShelf,
}

impl TrayType {
    fn name(self) -> &'static str {
        match self {
            Self::Compute => "compute",
            Self::Switch => "switch",
            Self::PowerShelf => "power_shelf",
        }
    }
}

pub(super) async fn load_report(
    pool: &sqlx::PgPool,
    rack: &Rack,
) -> Result<Option<HealthReport>, StateHandlerError> {
    let mut connection = pool.acquire().await?;
    let machines = crate::get_machines_from_rack(rack, connection.as_mut()).await?;
    let compute = machines
        .iter()
        .map(|machine| (machine.id.to_string(), machine.status.bmc_info.ip))
        .collect::<Vec<_>>();
    let switch_ids = db::switch::find_ids(
        connection.as_mut(),
        model::switch::SwitchSearchFilter {
            rack_id: Some(rack.id.clone()),
            ..Default::default()
        },
    )
    .await?;
    let switches = db::switch::find_by(
        connection.as_mut(),
        db::ObjectColumnFilter::List(db::switch::IdColumn, &switch_ids),
    )
    .await?;
    let switch = switches
        .into_iter()
        .map(|switch| {
            (
                switch.id.to_string(),
                switch.bmc_info.and_then(|bmc| bmc.ip),
            )
        })
        .collect::<Vec<_>>();
    let shelf_ids = db::power_shelf::find_ids(
        connection.as_mut(),
        model::power_shelf::PowerShelfSearchFilter {
            rack_id: Some(rack.id.clone()),
            ..Default::default()
        },
    )
    .await?;
    let shelves = db::power_shelf::find_by(
        connection.as_mut(),
        db::ObjectColumnFilter::List(db::power_shelf::IdColumn, &shelf_ids),
    )
    .await?;
    let power_shelf = shelves
        .into_iter()
        .map(|shelf| (shelf.id.to_string(), shelf.bmc_info.and_then(|bmc| bmc.ip)))
        .collect::<Vec<_>>();
    let groups = [
        (TrayType::Compute, compute),
        (TrayType::Switch, switch),
        (TrayType::PowerShelf, power_shelf),
    ];
    let addresses = groups
        .iter()
        .flat_map(|(_, devices)| devices.iter().filter_map(|(_, address)| *address))
        .collect();
    let endpoints = db::explored_endpoints::find_by_ips(connection.as_mut(), addresses).await?;
    let reports = endpoints
        .into_iter()
        .filter(|endpoint| !endpoint.waiting_for_explorer_refresh)
        .map(|endpoint| (endpoint.address, endpoint.report))
        .collect::<HashMap<_, _>>();
    let previous_health_report = rack.health_reports.merges.get(SOURCE);
    let alerts = groups
        .into_iter()
        .flat_map(|(kind, devices)| {
            let trays = devices
                .iter()
                .map(|(device_id, address)| TrayFirmware {
                    device_id,
                    address: *address,
                    report: address.and_then(|address| reports.get(&address)),
                })
                .collect::<Vec<_>>();
            compare(kind, &trays, previous_health_report)
        })
        .collect::<Vec<_>>();
    Ok((!alerts.is_empty()).then(|| HealthReport {
        source: SOURCE.to_string(),
        observed_at: Some(chrono::Utc::now()),
        triggered_by: None,
        successes: Vec::new(),
        alerts,
    }))
}

/// Compare exact versions for matching inventory IDs within one tray type.
/// Missing reports or versions cannot prove recovery of an existing alert.
fn compare(
    kind: TrayType,
    trays: &[TrayFirmware<'_>],
    previous_health_report: Option<&HealthReport>,
) -> Vec<HealthProbeAlert> {
    if trays.len() < 2 {
        return Vec::new();
    }
    let inventories = trays
        .iter()
        .map(|tray| {
            tray.report
                .filter(|report| report.last_exploration_error.is_none())
                .into_iter()
                .flat_map(|report| &report.service)
                .flat_map(|service| &service.inventories)
                .filter_map(|entry| {
                    entry
                        .version
                        .as_deref()
                        .filter(|version| !version.trim().is_empty())
                        .map(|version| (entry.id.as_str(), version))
                })
                .collect::<BTreeMap<_, _>>()
        })
        .collect::<Vec<_>>();
    let prefix = format!("{}/", kind.name());
    let previous_alerts = previous_health_report
        .into_iter()
        .flat_map(|report| &report.alerts)
        .filter_map(|alert| {
            alert
                .target
                .as_deref()?
                .strip_prefix(&prefix)
                .map(|id| (id, alert))
        })
        .collect::<BTreeMap<_, _>>();
    let ids = inventories
        .iter()
        .flat_map(|inventory| inventory.keys().copied())
        .chain(previous_alerts.keys().copied())
        .collect::<BTreeSet<_>>();
    ids.into_iter()
        .filter_map(|id| {
            let mut versions = BTreeMap::<&str, Vec<String>>::new();
            for (inventory, tray) in inventories.iter().zip(trays) {
                if let Some(version) = inventory.get(id) {
                    let device = match tray.address {
                        Some(address) => format!("{} (bmc ip: {address})", tray.device_id),
                        None => tray.device_id.to_string(),
                    };
                    versions.entry(version).or_default().push(device);
                }
            }
            if versions.len() > 1 {
                let details = versions
                    .into_iter()
                    .map(|(version, mut devices)| {
                        devices.sort();
                        format!("{version}: [{}]", devices.join(", "))
                    })
                    .collect::<Vec<_>>()
                    .join("; ");
                Some(HealthProbeAlert {
                    id: HealthProbeId::rack_firmware_mismatch(),
                    target: Some(format!("{prefix}{id}")),
                    in_alert_since: None,
                    message: format!(
                        "{} trays report different firmware for {id}: {details}",
                        kind.name()
                    ),
                    tenant_message: None,
                    classifications: vec![HealthAlertClassification::hardware()],
                })
            } else if inventories
                .iter()
                .any(|inventory| !inventory.contains_key(id))
            {
                previous_alerts.get(id).map(|alert| (*alert).clone())
            } else {
                None
            }
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use carbide_test_support::{Check, check_values};
    use model::site_explorer::{Inventory, Service};

    use super::*;

    fn tray(report: Option<&EndpointExplorationReport>) -> TrayFirmware<'_> {
        TrayFirmware {
            device_id: "test-tray",
            address: None,
            report,
        }
    }

    fn report(entries: &[(&str, Option<&str>)]) -> EndpointExplorationReport {
        EndpointExplorationReport {
            service: vec![Service {
                id: "UpdateService".to_string(),
                inventories: entries
                    .iter()
                    .map(|(id, version)| Inventory {
                        id: (*id).to_string(),
                        version: version.map(str::to_string),
                        ..Default::default()
                    })
                    .collect(),
            }],
            ..Default::default()
        }
    }

    #[test]
    fn compares_matching_inventory_versions() {
        check_values(
            [
                Check {
                    scenario: "equal versions regardless of inventory order",
                    input: vec![
                        report(&[("BMC", Some("1")), ("BIOS", Some("2"))]),
                        report(&[("BIOS", Some("2")), ("BMC", Some("1"))]),
                    ],
                    expect: vec![],
                },
                Check {
                    scenario: "one mismatching component",
                    input: vec![
                        report(&[("BMC", Some("1")), ("BIOS", Some("2"))]),
                        report(&[("BMC", Some("3")), ("BIOS", Some("2"))]),
                    ],
                    expect: vec!["compute/BMC".to_string()],
                },
                Check {
                    scenario: "missing version is not a mismatch",
                    input: vec![report(&[("BMC", Some("1"))]), report(&[("BMC", None)])],
                    expect: vec![],
                },
                Check {
                    scenario: "different inventory IDs are not compared",
                    input: vec![
                        report(&[("BMC", Some("1"))]),
                        report(&[("BIOS", Some("2"))]),
                    ],
                    expect: vec![],
                },
                Check {
                    scenario: "single tray",
                    input: vec![report(&[("BMC", Some("1"))])],
                    expect: vec![],
                },
            ],
            |reports| {
                let trays = reports
                    .iter()
                    .map(|report| tray(Some(report)))
                    .collect::<Vec<_>>();
                compare(TrayType::Compute, &trays, None)
                    .into_iter()
                    .map(|alert| alert.target.unwrap())
                    .collect::<Vec<_>>()
            },
        );
    }

    #[test]
    fn recovery_requires_versions_from_every_remaining_tray() {
        let first = report(&[("BMC", Some("1"))]);
        let second = report(&[("BMC", Some("2"))]);
        let alerts = compare(
            TrayType::Switch,
            &[tray(Some(&first)), tray(Some(&second))],
            None,
        );
        assert_eq!(alerts[0].id.as_str(), "RackFirmwareMismatch");
        assert_eq!(
            alerts[0].classifications,
            vec![HealthAlertClassification::hardware()]
        );
        let mut previous = HealthReport::empty(SOURCE.to_string());
        previous.alerts = alerts;
        let missing_version = report(&[("BMC", Some(""))]);
        let failed = EndpointExplorationReport {
            last_exploration_error: Some(model::site_explorer::EndpointExplorationError::Other {
                details: "exploration failed".to_string(),
            }),
            ..first.clone()
        };
        check_values(
            [
                Check {
                    scenario: "missing endpoint preserves alert",
                    input: vec![tray(Some(&first)), tray(None)],
                    expect: previous.alerts.clone(),
                },
                Check {
                    scenario: "empty version preserves alert",
                    input: vec![tray(Some(&first)), tray(Some(&missing_version))],
                    expect: previous.alerts.clone(),
                },
                Check {
                    scenario: "agreement clears alert",
                    input: vec![tray(Some(&first)), tray(Some(&first))],
                    expect: vec![],
                },
                Check {
                    scenario: "failed exploration cannot confirm agreement",
                    input: vec![tray(Some(&first)), tray(Some(&failed))],
                    expect: previous.alerts.clone(),
                },
                Check {
                    scenario: "removed tray clears mismatch",
                    input: vec![tray(Some(&first))],
                    expect: vec![],
                },
            ],
            |trays| compare(TrayType::Switch, &trays, Some(&previous)),
        );
        assert!(
            compare(
                TrayType::PowerShelf,
                &[tray(Some(&first)), tray(None)],
                Some(&previous)
            )
            .is_empty()
        );
    }
}
