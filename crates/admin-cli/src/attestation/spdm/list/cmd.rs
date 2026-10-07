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

use ::rpc::forge::spdm_list_attestation_machines_request::Variant;
use ::rpc::forge::{
    SpdmAttestationStatus, SpdmListAttestationMachinesRequest,
    SpdmListAttestationMachinesRequestSelector,
};
use carbide_uuid::machine::MachineId;

use crate::attestation::spdm::list::args::{Args, Selector};
use crate::errors::{CarbideCliError, CarbideCliResult};
use crate::rpc::ApiClient;

pub(super) async fn list(args: Args, api_client: &ApiClient) -> CarbideCliResult<()> {
    let variant = match (args.machine_id, args.selector) {
        (Some(machine_id), None) => Some(Variant::MachineId(machine_id)),
        (None, Some(Selector::InProgress)) => Some(Variant::Selector(
            SpdmListAttestationMachinesRequestSelector::SpdmListInProgress.into(),
        )),
        (None, Some(Selector::Unsuccessful)) => Some(Variant::Selector(
            SpdmListAttestationMachinesRequestSelector::SpdmListUnsuccessful.into(),
        )),
        (None, None) => None,
        (Some(_), Some(_)) => unreachable!("clap prevents selecting both machine_id and selector"),
    };

    let attestations: Vec<(MachineId, String)> = api_client
        .0
        .list_attestation_machines(SpdmListAttestationMachinesRequest { variant })
        .await?
        .statuses
        .into_iter()
        .map(|elem| {
            let machine_id = elem.machine_id.ok_or_else(|| {
                CarbideCliError::GenericError(
                    "SPDM attestation status missing machine_id".to_string(),
                )
            })?;
            let att_status = SpdmAttestationStatus::try_from(elem.attestation_status)
                .map_err(|_| CarbideCliError::GenericError(
                    "SPDM attestation cannot construct SpdmAttestationStatus - unknown integer value".to_string(),
                ))?
                .as_str_name()
                .to_string();
            Ok((machine_id, att_status))
        })
        .collect::<CarbideCliResult<Vec<(MachineId, String)>>>()?;

    println!("{}", serde_json::to_string_pretty(&attestations)?);

    Ok(())
}
