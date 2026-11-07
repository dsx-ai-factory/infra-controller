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

use ::rpc::forge::MachineCredentialsUpdateResponse;
use ::rpc::forge::machine_credentials_update_request::{CredentialPurpose, Credentials};
use carbide_secrets::credentials::{BmcCredentialType, CredentialKey, CredentialWriter};
use carbide_uuid::machine::MachineId;
use mac_address::MacAddress;

mod bmc_session_manager;

pub(crate) use bmc_session_manager::{
    BmcAuthMaterial, BmcSessionError, BmcSessionManager, BmcSessionStore, PgBmcSessionStore,
};

use crate::{CarbideError, CarbideResult};

pub(crate) struct UpdateCredentials {
    pub(crate) machine_id: MachineId,
    pub(crate) mac_address: Option<MacAddress>,
    pub(crate) credentials: Vec<Credentials>,
}

impl UpdateCredentials {
    pub(crate) async fn execute(
        &self,
        credential_writer: &dyn CredentialWriter,
    ) -> CarbideResult<MachineCredentialsUpdateResponse> {
        for credential in self.credentials.iter() {
            let credential_purpose = CredentialPurpose::try_from(credential.credential_purpose)
                .map_err(|error| {
                    CarbideError::internal(format!(
                        "invalid discriminant {error:?} for credential purpose from grpc?"
                    ))
                })?;

            let key = match credential_purpose {
                CredentialPurpose::Hbn => CredentialKey::DpuHbn {
                    machine_id: self.machine_id,
                },
                CredentialPurpose::Bmc => CredentialKey::BmcCredentials {
                    credential_type: BmcCredentialType::BmcRoot {
                        bmc_mac_address: self
                            .mac_address
                            .ok_or_else(|| CarbideError::MissingArgument("MAC address"))?,
                    },
                },
            };

            credential_writer
                .set_credentials(
                    &key,
                    &carbide_secrets::credentials::Credentials::UsernamePassword {
                        username: credential.user.clone(),
                        password: credential.password.clone(),
                    },
                )
                .await
                .map_err(|err| CarbideError::internal(format!("{err}")))?;
        }

        Ok(MachineCredentialsUpdateResponse {})
    }
}
