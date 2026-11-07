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
use axum::extract::{FromRequestParts, Query};
use axum::http::request::Parts;
use serde::{Deserialize, Serialize};

use crate::common::MachineInterface;
use crate::extractors::machine_architecture::MachineArchitecture;
use crate::rpc_error::PxeRequestError;

#[derive(Clone, Serialize, Deserialize, Debug)]
struct MaybeMachineInterface {
    #[serde(rename(deserialize = "buildarch"))]
    build_architecture: String,
    #[serde(default)]
    platform: Option<String>,
    #[serde(default)]
    manufacturer: Option<String>,
    #[serde(default)]
    product: Option<String>,
    #[serde(default)]
    serial: Option<String>,
    #[serde(default)]
    asset: Option<String>,
}

impl<S> FromRequestParts<S> for MachineInterface
where
    S: Send + Sync,
{
    type Rejection = PxeRequestError;

    async fn from_request_parts(parts: &mut Parts, state: &S) -> Result<Self, Self::Rejection> {
        // A missing or malformed build architecture is a boot outcome
        // operators watch for, so both rejection shapes count as one --
        // even though the client gets a real 4xx rather than a template.
        // This extractor serves only the boot route.
        let count_bad_architecture = || {
            carbide_instrument::emit(crate::metrics::PxeBootOutcome {
                endpoint: crate::metrics::BootEndpoint::Boot,
                reason: crate::metrics::OutcomeReason::ArchitectureNotFound,
            });
        };

        let Ok(maybe) = Query::<MaybeMachineInterface>::from_request_parts(parts, state).await
        else {
            // Query parsing only fails on the required build_architecture
            // field; everything else is optional.
            count_bad_architecture();
            return Err(PxeRequestError::InvalidBuildArch);
        };
        let maybe = maybe.0;

        let build_architecture = MachineArchitecture::try_from(maybe.build_architecture.as_str())
            .inspect_err(|_| count_bad_architecture())?;

        // Note: This does *NOT* look at X-Forwarded-For, due to security issues with the header. We
        // don't currently have use cases for a proxy in front of carbide-pxe... if that changes
        // someday we will need to configure a request extractor that conditionally uses
        // X-Forwarded-For if it's present and falling back on ClientIp if it's not.
        let client_ip = super::client_ip(parts, state).await?;

        Ok(MachineInterface {
            architecture: Some(build_architecture),
            client_ip,
            platform: maybe.platform,
            manufacturer: maybe.manufacturer,
            product: maybe.product,
            serial: maybe.serial,
            asset: maybe.asset,
        })
    }
}
