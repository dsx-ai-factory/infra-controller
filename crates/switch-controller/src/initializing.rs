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

//! Handler for SwitchControllerState::Initializing.

use carbide_uuid::switch::SwitchId;
use model::machine_interface_address::MachineInterfaceAssociation;
use model::switch::{
    ConfigureCertificateState, ConfiguringState, InitializingState, Switch, SwitchControllerState,
};
use state_controller::state_handler::{
    StateHandlerContext, StateHandlerError, StateHandlerOutcome,
};

use crate::context::SwitchStateHandlerContextObjects;

/// Handles the Initializing state for a switch.
pub async fn handle_initializing(
    switch_id: &SwitchId,
    state: &mut Switch,
    ctx: &mut StateHandlerContext<'_, SwitchStateHandlerContextObjects>,
) -> Result<StateHandlerOutcome<SwitchControllerState>, StateHandlerError> {
    let initializing_state = match &state.controller_state.value {
        SwitchControllerState::Initializing { initializing_state } => initializing_state,
        _ => unreachable!("handle_initializing called with non-Initializing state"),
    };

    match initializing_state {
        InitializingState::WaitForOsMachineInterface => {
            handle_wait_for_os_machine_interface(switch_id, state, ctx).await
        }
    }
}

async fn handle_wait_for_os_machine_interface(
    switch_id: &SwitchId,
    state: &mut Switch,
    ctx: &mut StateHandlerContext<'_, SwitchStateHandlerContextObjects>,
) -> Result<StateHandlerOutcome<SwitchControllerState>, StateHandlerError> {
    let Some(bmc_mac_address) = state.bmc_mac_address else {
        return Ok(StateHandlerOutcome::transition(
            SwitchControllerState::Error {
                cause: "No BMC MAC address on switch".to_string(),
            },
        ));
    };

    let mut txn = ctx.services.db_pool.begin().await?;

    let expected_switch =
        db::expected_switch::find_by_bmc_mac_address(&mut txn, bmc_mac_address).await?;

    let expected_switch = match expected_switch {
        Some(es) => es,
        None => {
            tracing::info!(
                switch_id = %switch_id,
                bmc_mac_address = %bmc_mac_address,
                "No expected switch found; transitioning to error",
            );
            return Ok(StateHandlerOutcome::transition(
                SwitchControllerState::Error {
                    cause: format!("No expected switch found for BMC MAC {}", bmc_mac_address),
                },
            ));
        }
    };

    let nvos_mac_addresses = expected_switch.nvos_mac_addresses();
    if nvos_mac_addresses.is_empty() {
        tracing::warn!(
            switch_id = %switch_id,
            bmc_mac_address = %bmc_mac_address,
            expected_bmc_mac_address = %expected_switch.bmc_mac_address,
            "Switch has no NVOS MAC addresses on expected switch",
        );
        return Ok(StateHandlerOutcome::transition(
            SwitchControllerState::Error {
                cause: format!(
                    "No NVOS MAC addresses on expected switch for serial {}, BMC MAC {}",
                    bmc_mac_address, expected_switch.bmc_mac_address
                ),
            },
        ));
    }

    // Endpoint selection and configuration target the operator-declared primary
    // NVOS port, so associating only a non-primary port is not enough to
    // advance. A set with ports but no resolvable primary cannot be configured.
    let Some(primary_mac_address) = expected_switch.primary_nvos_mac_address() else {
        tracing::warn!(
            switch_id = %switch_id,
            bmc_mac_address = %bmc_mac_address,
            expected_bmc_mac_address = %expected_switch.bmc_mac_address,
            "Switch has NVOS ports but none resolves as primary",
        );
        return Ok(StateHandlerOutcome::transition(
            SwitchControllerState::Error {
                cause: format!(
                    "No primary NVOS port on expected switch for BMC MAC {}",
                    expected_switch.bmc_mac_address
                ),
            },
        ));
    };

    let mut associated_count = 0usize;
    let mut primary_associated = false;
    let total = nvos_mac_addresses.len();

    for mac_address in &nvos_mac_addresses {
        let mi = db::machine_interface::find_by_mac_address(&mut *txn, *mac_address).await?;
        let interface = match mi.first() {
            Some(iface) => iface,
            None => continue,
        };

        if let Some(existing_switch_id) = interface.switch_id {
            if existing_switch_id != *switch_id {
                tracing::warn!(
                    switch_id = %switch_id,
                    nvos_mac_address = %mac_address,
                    existing_switch_id = %existing_switch_id,
                    "Switch: NVOS MAC already associated with switch",
                );
                return Ok(StateHandlerOutcome::transition(
                    SwitchControllerState::Error {
                        cause: format!(
                            "NVOS MAC {} already associated with switch {}",
                            mac_address, existing_switch_id
                        ),
                    },
                ));
            }
            associated_count += 1;
            if *mac_address == primary_mac_address {
                primary_associated = true;
            }
            continue;
        }

        db::machine_interface::associate_interface_with_machine(
            &interface.id,
            MachineInterfaceAssociation::Switch(*switch_id),
            &mut txn,
        )
        .await?;
        tracing::info!(
            switch_id = %switch_id,
            machine_interface_id = %interface.id,
            nvos_mac_address = %mac_address,
            "Switch associated NVOS interface",
        );
        associated_count += 1;
        if *mac_address == primary_mac_address {
            primary_associated = true;
        }
    }

    txn.commit().await?;

    tracing::info!(
        switch_id = %switch_id,
        associated_nvos_interface_count = associated_count,
        bmc_mac_address = %bmc_mac_address,
        "Switch associated NVOS interfaces",
    );
    if primary_associated {
        tracing::info!(
            switch_id = %switch_id,
            associated_nvos_interface_count = associated_count,
            total_nvos_interface_count = total,
            primary_nvos_mac_address = %primary_mac_address,
            "Switch primary NVOS interface associated, transitioning to Configuring",
        );
        Ok(StateHandlerOutcome::transition(
            SwitchControllerState::Configuring {
                config_state: ConfiguringState::ConfigureCertificate {
                    configure_certificate: ConfigureCertificateState::Start,
                },
            },
        ))
    } else {
        tracing::info!(
            switch_id = %switch_id,
            associated_nvos_interface_count = associated_count,
            total_nvos_interface_count = total,
            primary_nvos_mac_address = %primary_mac_address,
            "Switch primary NVOS interface not yet associated, waiting",
        );
        Ok(StateHandlerOutcome::wait(format!(
            "primary NVOS interface not yet associated ({}/{} ports associated), waiting",
            associated_count, total
        )))
    }
}
