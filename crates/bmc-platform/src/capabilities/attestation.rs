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

use std::sync::Arc;

use async_trait::async_trait;
use nv_redfish::component_integrity::SpdmGetSignedMeasurementsResponse;
use nv_redfish::core::Bmc;
use nv_redfish::schema::certificate::Certificate;
use nv_redfish::schema::component_integrity::ComponentIntegrity;
use nv_redfish::schema::message::Message;
use nv_redfish::schema::software_inventory::SoftwareInventory;
use nv_redfish::schema::task::TaskState;

use crate::{OpCx, OperationReference, PlatformError};

/// Signed measurements, the request still producing them, or how it ended
/// without them.
#[derive(Debug)]
pub enum EvidenceProgress {
    /// The measurements are available. A BMC can serve them from a URI shared
    /// between requests, so check them against the request nonce.
    Ready(SpdmGetSignedMeasurementsResponse),
    /// The BMC is still collecting; persist this and poll again with it.
    Pending(OperationReference),
    /// The collection task ended without measurements; request them again.
    Failed {
        state: TaskState,
        messages: Vec<Message>,
    },
}

/// Collection of hardware attestation evidence.
///
/// Every operation defaults to delegating to [`Self::standard`], so a driver
/// implements only the operations its platform deviates on.
#[async_trait]
pub trait Attestation<B: Bmc>: Send + Sync {
    /// The driver every operation this driver does not implement delegates to.
    ///
    /// Vendor and model drivers return the capability's standard driver and
    /// implement only their deviations. The standard driver implements every
    /// operation and returns `self`.
    fn standard(&self) -> &dyn Attestation<B>;

    async fn components(
        &self,
        cx: &OpCx<'_, B>,
    ) -> Result<Vec<Arc<ComponentIntegrity>>, PlatformError> {
        self.standard().components(cx).await
    }

    async fn firmware_for_component(
        &self,
        cx: &OpCx<'_, B>,
        component_id: &str,
    ) -> Result<Arc<SoftwareInventory>, PlatformError> {
        self.standard()
            .firmware_for_component(cx, component_id)
            .await
    }

    /// The certificate the component's SPDM responder authenticates with.
    async fn ca_certificate(
        &self,
        cx: &OpCx<'_, B>,
        component_id: &str,
    ) -> Result<Arc<Certificate>, PlatformError> {
        self.standard().ca_certificate(cx, component_id).await
    }

    /// Requests signed measurements over `nonce`.
    async fn request_evidence(
        &self,
        cx: &OpCx<'_, B>,
        component_id: &str,
        nonce: &[u8],
    ) -> Result<EvidenceProgress, PlatformError> {
        self.standard()
            .request_evidence(cx, component_id, nonce)
            .await
    }

    /// Polls a pending signed-measurements request once.
    async fn poll_evidence(
        &self,
        cx: &OpCx<'_, B>,
        pending: &OperationReference,
    ) -> Result<EvidenceProgress, PlatformError> {
        self.standard().poll_evidence(cx, pending).await
    }
}
