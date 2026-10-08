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

use ::rpc::forge::{
    SpdmAttestationStatus, SpdmListAttestationRequest, SpdmListAttestationRequestSelector,
};
use uuid::Uuid;

use crate::attestation::spdm::list::args::{Args, Selector};
use crate::errors::{CarbideCliError, CarbideCliResult};
use crate::rpc::ApiClient;

pub(super) async fn list(args: Args, api_client: &ApiClient) -> CarbideCliResult<()> {
    let selector = match args.selector {
        Selector::InProgress => SpdmListAttestationRequestSelector::SpdmListInProgress,
        Selector::Cancelled => SpdmListAttestationRequestSelector::SpdmListCancelled,
        Selector::Failed => SpdmListAttestationRequestSelector::SpdmListFailed,
        Selector::Passed => SpdmListAttestationRequestSelector::SpdmListPassed,
    };

    let attestations: Vec<(Uuid, String)> = api_client
        .0
        .list_attestation(SpdmListAttestationRequest {
            selector: selector.into(),
        })
        .await?
        .statuses
        .into_iter()
        .map(|elem| {
            let job_id = elem.job_id.ok_or_else(|| {
                CarbideCliError::GenericError(
                    "SPDM attestation status missing job_id".to_string(),
                )
            })?;
            let job_id = Uuid::try_from(job_id).map_err(|_| {
                CarbideCliError::GenericError(
                    "SPDM attestation status contains an invalid job_id".to_string(),
                )
            })?;
            let att_status = SpdmAttestationStatus::try_from(elem.attestation_status)
                .map_err(|_| CarbideCliError::GenericError(
                    "SPDM attestation cannot construct SpdmAttestationStatus - unknown integer value".to_string(),
                ))?
                .as_str_name()
                .to_string();
            Ok((job_id, att_status))
        })
        .collect::<CarbideCliResult<Vec<(Uuid, String)>>>()?;

    println!("{}", serde_json::to_string_pretty(&attestations)?);

    Ok(())
}
