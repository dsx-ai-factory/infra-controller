// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

//! Refreshes the running NVOS observation while a switch is otherwise idle.

use carbide_uuid::switch::SwitchId;
use component_manager::firmware_inventory::{
    ComponentFirmwareInventoryResult, FirmwareInventorySource,
};
use db::ConditionalWrite;
use mac_address::MacAddress;
use model::switch::{Switch, SwitchControllerState};
use state_controller::state_handler::{
    StateHandlerContext, StateHandlerError, StateHandlerOutcome,
};

use crate::context::SwitchStateHandlerContextObjects;
use crate::maintenance::build_switch_endpoint;

pub(super) async fn refresh(
    switch_id: &SwitchId,
    state: &Switch,
    ctx: &mut StateHandlerContext<'_, SwitchStateHandlerContextObjects>,
) -> Result<StateHandlerOutcome<SwitchControllerState>, StateHandlerError> {
    let Some(component_manager) = ctx.services.component_manager.as_ref() else {
        return Ok(StateHandlerOutcome::wait(
            "NVOS version refresh requires a component manager".into(),
        ));
    };
    // Only RMS defines its NVOS member as the current /nvue_v1/system/image.
    // Check this capability before resolving endpoints or reading credentials.
    if component_manager.nv_switch.firmware_inventory_source() != Some(FirmwareInventorySource::Rms)
    {
        return Ok(StateHandlerOutcome::wait(
            "NVOS version refresh requires RMS firmware inventory".into(),
        ));
    }

    // Endpoint DB/credential reads and persistence share the state controller's
    // max_object_handling_time budget (180 seconds by default), which starts
    // before queue claim and cancels this handler on expiry.
    let endpoint = match build_switch_endpoint(
        switch_id,
        state,
        &ctx.services.db_pool,
        ctx.services.credential_manager.as_ref(),
    )
    .await
    {
        Ok(endpoint) => endpoint,
        Err(error) => {
            tracing::debug!(%switch_id, %error, "NVOS version endpoint is unavailable");
            return Ok(StateHandlerOutcome::wait(
                "NVOS version refresh is waiting for endpoint credentials and addresses".into(),
            ));
        }
    };

    // One read per controller pass. The RMS inventory reader bounds preparation
    // and RPCs together to 30 seconds. Cancellation drops the read; failures
    // retain the request and retry on the next periodic controller iteration.
    // No database transaction remains open across endpoint or backend I/O.
    let results = match component_manager
        .nv_switch
        .get_firmware_inventory(std::slice::from_ref(&endpoint))
        .await
    {
        Ok(results) => results,
        Err(error) => {
            tracing::debug!(%switch_id, %error, "NVOS firmware inventory read failed");
            return Ok(StateHandlerOutcome::wait(
                "NVOS version refresh is waiting for firmware inventory".into(),
            ));
        }
    };
    let version = match running_nvos_version(&results, endpoint.bmc_mac) {
        Ok(version) => version,
        Err(reason) => return Ok(StateHandlerOutcome::wait(reason.into())),
    };

    let mut txn = ctx.services.db_pool.begin().await?;
    match db::switch::try_complete_nvos_version_refresh(
        &mut txn,
        *switch_id,
        state.version,
        version,
    )
    .await?
    {
        ConditionalWrite::Applied(()) => Ok(StateHandlerOutcome::do_nothing().with_txn(txn)),
        ConditionalWrite::NotApplied(_) => Ok(StateHandlerOutcome::wait(
            "NVOS version refresh observation is no longer current".into(),
        )
        .with_txn(txn)),
    }
}

fn running_nvos_version(
    results: &[ComponentFirmwareInventoryResult],
    expected_mac: MacAddress,
) -> Result<&str, &'static str> {
    let [result] = results else {
        return Err("NVOS version refresh requires exactly one switch inventory");
    };
    if result.mac_address != expected_mac {
        return Err("NVOS version refresh inventory does not identify the requested switch");
    }
    let inventory = result
        .inventory
        .as_ref()
        .map_err(|_| "NVOS version refresh inventory is unavailable")?;
    let mut version = None;
    for entry in inventory {
        let firmware = entry
            .firmware
            .as_ref()
            .map_err(|_| "NVOS version refresh has an unreadable inventory member")?;
        if firmware.name.as_deref() != Some("NVOS") {
            continue;
        }
        if version.is_some() {
            return Err("NVOS version refresh has ambiguous NVOS inventory members");
        }
        version = Some(
            firmware
                .version
                .as_deref()
                .filter(|value| !value.trim().is_empty())
                .ok_or("NVOS version refresh has no nonempty NVOS version")?,
        );
    }
    version.ok_or("NVOS version refresh inventory has no NVOS member")
}

#[cfg(test)]
mod tests {
    use carbide_test_support::{Check, check_values};
    use component_manager::firmware_inventory::{
        FirmwareComponent, FirmwareInventoryEntry, FirmwareReadError,
    };

    use super::*;

    fn member(name: &str, version: Option<&str>) -> FirmwareInventoryEntry {
        FirmwareInventoryEntry {
            inventory_id: None,
            firmware: Ok(FirmwareComponent {
                id: None,
                resource_uri: None,
                name: Some(name.into()),
                update_target: None,
                description: None,
                version: version.map(str::to_owned),
                release_date: None,
            }),
        }
    }

    #[test]
    fn running_nvos_version_requires_unambiguous_evidence() {
        let mac = MacAddress::new([2, 0, 0, 0, 0, 1]);
        let observation = |members| ComponentFirmwareInventoryResult {
            mac_address: mac,
            inventory: Ok(members),
        };
        let nvos = member("NVOS", Some("nvos-25.02.4440"));
        check_values(
            [
                Check {
                    scenario: "NVOS is selected among other firmware",
                    input: vec![observation(vec![member("BMC", Some("1.2")), nvos.clone()])],
                    expect: Some("nvos-25.02.4440".to_owned()),
                },
                Check {
                    scenario: "missing endpoint result",
                    input: vec![],
                    expect: None,
                },
                Check {
                    scenario: "duplicated endpoint result",
                    input: vec![
                        observation(vec![nvos.clone()]),
                        observation(vec![nvos.clone()]),
                    ],
                    expect: None,
                },
                Check {
                    scenario: "another endpoint cannot supply the version",
                    input: vec![ComponentFirmwareInventoryResult {
                        mac_address: MacAddress::new([2, 0, 0, 0, 0, 2]),
                        inventory: Ok(vec![nvos.clone()]),
                    }],
                    expect: None,
                },
                Check {
                    scenario: "collection read failure",
                    input: vec![ComponentFirmwareInventoryResult {
                        mac_address: mac,
                        inventory: Err(FirmwareReadError::ReadFailed),
                    }],
                    expect: None,
                },
                Check {
                    scenario: "unreadable member leaves uniqueness unknown",
                    input: vec![observation(vec![
                        nvos.clone(),
                        FirmwareInventoryEntry {
                            inventory_id: None,
                            firmware: Err(FirmwareReadError::LimitExceeded),
                        },
                    ])],
                    expect: None,
                },
                Check {
                    scenario: "other firmware does not imply NVOS",
                    input: vec![observation(vec![member("BMC", Some("1.2"))])],
                    expect: None,
                },
                Check {
                    scenario: "missing NVOS version",
                    input: vec![observation(vec![member("NVOS", None)])],
                    expect: None,
                },
                Check {
                    scenario: "blank NVOS version",
                    input: vec![observation(vec![member("NVOS", Some(" \t"))])],
                    expect: None,
                },
                Check {
                    scenario: "duplicate NVOS members are ambiguous even when identical",
                    input: vec![observation(vec![nvos.clone(), nvos])],
                    expect: None,
                },
            ],
            |results| running_nvos_version(&results, mac).ok().map(str::to_owned),
        );
    }
}
