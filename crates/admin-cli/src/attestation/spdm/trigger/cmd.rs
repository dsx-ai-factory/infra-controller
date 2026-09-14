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

use ::rpc::forge::{SpdmMachineAttestationTriggerRequest, SpdmSchedulingOutcome};

use crate::attestation::spdm::trigger::Args;
use crate::errors::CarbideCliResult;
use crate::rpc::ApiClient;

/// Why nothing was scheduled, or that something was. A trigger that schedules
/// nothing still succeeds, so the outcome is the only thing that says which
/// happened.
fn outcome(outcome: i32) -> &'static str {
    match SpdmSchedulingOutcome::try_from(outcome) {
        Ok(SpdmSchedulingOutcome::Scheduled) => "scheduled",
        Ok(SpdmSchedulingOutcome::AttestationDisabled) => {
            "nothing scheduled: the profile's mode is none"
        }
        Ok(SpdmSchedulingOutcome::NoAttestersFound) => {
            "nothing scheduled: the BMC offered nothing eligible"
        }
        Ok(SpdmSchedulingOutcome::PolicyMatchedNothing) => {
            "nothing scheduled: the profile's requirement went unsatisfied"
        }
        Ok(SpdmSchedulingOutcome::ClassNotRecorded) => {
            "nothing scheduled: no hardware class recorded; explore this BMC again"
        }
        Ok(SpdmSchedulingOutcome::NoProfile) => {
            "nothing scheduled: neither this class nor any has a profile"
        }
        Ok(SpdmSchedulingOutcome::ClassUnrecognized) => {
            "nothing scheduled: exploration did not recognise this hardware"
        }
        Ok(SpdmSchedulingOutcome::Unspecified) | Err(_) => {
            "unknown to this client; the server is newer"
        }
    }
}

pub(super) async fn trigger(args: Args, api_client: &ApiClient) -> CarbideCliResult<()> {
    let res = api_client
        .0
        .trigger_machine_attestation(SpdmMachineAttestationTriggerRequest {
            machine_id: Some(args.machine_id),
            redfish_timeout_secs: args.redfish_timeout_secs,
        })
        .await?;

    println!(
        "Attestation triggered for machine {}",
        res.machine_id
            .map(|e| e.to_string())
            .unwrap_or("No MachineId returned".to_string())
    );
    println!("  outcome:        {}", outcome(res.outcome));
    println!("  devices:        {}", res.devices_under_attestation);
    // Empty when exploration has recorded no class, which is itself an outcome.
    println!(
        "  hardware class: {}",
        if res.resolved_hardware_class.is_empty() {
            "none recorded"
        } else {
            &res.resolved_hardware_class
        }
    );
    println!(
        "  profile:        {}",
        match (res.profile_version, res.used_any_fallback) {
            (Some(version), true) => format!("{version} (the any fallback)"),
            (Some(version), false) => version,
            (None, _) => "none applied".to_string(),
        }
    );

    Ok(())
}
