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

//! Coverage for the ingestion `UefiSetupState::SetUefiPassword` step's error
//! split: a client-creation failure is transient and must retry in place. On
//! a vendor outside the tested set (Dell/Lenovo/NVIDIA) the device-level
//! error path deliberately skips ahead and ingests the host without a BIOS
//! password, so a transient blip taking that path would be permanent -- this
//! pins the retry.

use carbide_secrets::credentials::{CredentialKey, Credentials};
use carbide_test_harness::prelude::*;
use carbide_test_harness::test_support::fixture_config::FixtureDefault as _;
use model::machine::{MachineState, ManagedHostState, UefiSetupInfo, UefiSetupState};
use model::test_support::ManagedHostConfig;

use crate::env::Env;

fn uefi_setup_state(state: UefiSetupState) -> ManagedHostState {
    ManagedHostState::HostInit {
        machine_state: MachineState::UefiSetup {
            uefi_setup_info: UefiSetupInfo {
                uefi_password_jid: None,
                uefi_setup_state: state,
            },
        },
    }
}

fn at_uefi_step(state: &ManagedHostState, step: UefiSetupState) -> bool {
    matches!(
        state,
        ManagedHostState::HostInit {
            machine_state: MachineState::UefiSetup { uefi_setup_info },
        } if uefi_setup_info.uefi_setup_state == step
    )
}

/// A client-creation failure while setting the ingestion BIOS password is
/// transient: the step must hold in place (not skip ahead to lockdown, which
/// on an untested vendor would permanently ingest the host without a BIOS
/// password), and a later tick must proceed once creation succeeds again.
#[sqlx_test]
async fn set_uefi_password_client_creation_failure_retries_in_place(
    pool: PgPool,
) -> Result<(), Box<dyn std::error::Error>> {
    let mut env = Env::builder(pool.clone()).build().await;

    // A vendor outside the tested set: its device-level error path skips the
    // password rather than retrying, so this fixture can tell the two error
    // classes apart.
    let domain = env.test_harness.test_domain().await;
    let network_controller = env.test_harness.network_controller();
    let underlay_segment = network_controller.create_underlay_segment(&domain).await;
    network_controller.create_admin_segment(&domain).await;
    let site_explorer = env.test_harness.default_test_site_explorer();
    let mh = env
        .test_harness
        .managed_host_builder(&site_explorer, underlay_segment)
        .with_config(ManagedHostConfig {
            vendor: Some(bmc_vendor::BMCVendor::Supermicro),
            ..ManagedHostConfig::default()
        })
        .build()
        .await
        .0;

    // The SET step resolves the site-wide host UEFI credential (version 0 on
    // a fresh site) before dispatching.
    env.redfish_sim
        .seed_credential(
            &CredentialKey::host_uefi_site_default(0),
            &Credentials::UsernamePassword {
                username: String::new(),
                password: "uefi-v0".to_string(),
            },
        )
        .await;

    mh.advance_state(uefi_setup_state(UefiSetupState::SetUefiPassword))
        .await;

    // A tick with the credential op failing on client creation must hold in
    // place rather than fall through to the vendor skip.
    env.redfish_sim
        .set_uefi_setup_client_creation_error("bmc unreachable");
    env.run_single_iteration().await;
    assert!(
        at_uefi_step(
            &mh.host.machine().await.state.value,
            UefiSetupState::SetUefiPassword
        ),
        "a transient creation failure must retry the SET step in place, got {:?}",
        mh.host.machine().await.state.value,
    );

    // Once creation succeeds again, the step proceeds normally.
    env.redfish_sim.clear_uefi_setup_client_creation_error();
    env.run_single_iteration().await;
    assert!(
        at_uefi_step(
            &mh.host.machine().await.state.value,
            UefiSetupState::WaitForPasswordJobScheduled
        ),
        "the retried SET step should dispatch and advance, got {:?}",
        mh.host.machine().await.state.value,
    );

    Ok(())
}
