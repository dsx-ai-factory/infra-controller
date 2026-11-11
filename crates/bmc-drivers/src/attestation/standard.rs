/*
 * SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
 * SPDX-License-Identifier: Apache-2.0
 */

//! Standard Redfish ComponentIntegrity attestation driver.

use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use bmc_platform::{Attestation, EvidenceProgress, OpCx, OperationReference, PlatformError};
use nv_redfish::component_integrity::{ComponentIntegrity, SpdmGetSignedMeasurementsResponse};
use nv_redfish::core::action::ActionTarget;
use nv_redfish::core::{Action, AsyncTask, Bmc};
use nv_redfish::schema::certificate::Certificate;
use nv_redfish::schema::component_integrity::{
    ComponentIntegrity as ComponentIntegritySchema,
    ComponentIntegritySPDMGetSignedMeasurementsAction,
};
use nv_redfish::schema::software_inventory::SoftwareInventory;
use nv_redfish::task_service::AsyncActionResult;

use crate::attestation::support::{RedfishAttestationExt as _, pending};

/// Standard ComponentIntegrity attestation.
pub(crate) struct StandardAttestation;

#[async_trait]
impl<B: Bmc> Attestation<B> for StandardAttestation {
    fn standard(&self) -> &dyn Attestation<B> {
        self
    }

    async fn components(
        &self,
        cx: &OpCx<'_, B>,
    ) -> Result<Vec<Arc<ComponentIntegritySchema>>, PlatformError> {
        Ok(cx
            .integrity_components()
            .await?
            .iter()
            .map(ComponentIntegrity::raw)
            .collect())
    }

    /// Redfish defines no relationship between a component and its firmware
    /// inventory; platforms that have one override this.
    async fn firmware_for_component(
        &self,
        _cx: &OpCx<'_, B>,
        _component_id: &str,
    ) -> Result<Arc<SoftwareInventory>, PlatformError> {
        Err(PlatformError::Unsupported)
    }

    async fn ca_certificate(
        &self,
        cx: &OpCx<'_, B>,
        component_id: &str,
    ) -> Result<Arc<Certificate>, PlatformError> {
        Ok(cx
            .integrity_component(component_id)
            .await?
            .component_certificate()
            .await
            .map_err(|error| cx.map_redfish_error(error))?
            .ok_or(PlatformError::Unsupported)?
            .raw())
    }

    /// Requests signed measurements, returning them when the BMC answers at
    /// once and otherwise the operation to poll.
    async fn request_evidence(
        &self,
        cx: &OpCx<'_, B>,
        component_id: &str,
        nonce: &[u8],
    ) -> Result<EvidenceProgress, PlatformError> {
        let result = cx
            .integrity_component(component_id)
            .await?
            .spdm_get_signed_measurements(Some(hex::encode(nonce)), None, None)
            .await
            .map_err(|error| cx.map_redfish_error(error))?;
        match result.pending_task() {
            Some(task) => Ok(EvidenceProgress::Pending(pending(task))),
            None => cx.poll_signed_measurements(result).await,
        }
    }

    async fn poll_evidence(
        &self,
        cx: &OpCx<'_, B>,
        pending: &OperationReference,
    ) -> Result<EvidenceProgress, PlatformError> {
        let task = AsyncTask {
            location: pending.uri().clone().into(),
            retry_after: pending.retry_after_seconds().map(Duration::from_secs),
            task: pending.task().cloned(),
        };
        cx.poll_signed_measurements(AsyncActionResult::resume(&signed_measurements(), task))
            .await
    }
}

/// The signed-measurements action, typed for [`AsyncActionResult::resume`],
/// which reads only the action's types and never its target.
fn signed_measurements()
-> Action<ComponentIntegritySPDMGetSignedMeasurementsAction, SpdmGetSignedMeasurementsResponse> {
    Action::new(ActionTarget::new(String::new()))
}

#[cfg(test)]
mod tests {
    use axum::http::{Method, StatusCode};
    use serde_json::json;

    use super::*;
    use crate::test_support::{Fixture, body};

    const COLLECTION: &str = "/redfish/v1/ComponentIntegrity";
    const COMPONENT: &str = "/redfish/v1/ComponentIntegrity/HGX_IRoT_GPU_0";
    const TARGET: &str = "/redfish/v1/ComponentIntegrity/HGX_IRoT_GPU_0/Actions/ComponentIntegrity.SPDMGetSignedMeasurements";
    const TASK: &str = "/redfish/v1/TaskService/Tasks/95";
    const DATA: &str = "/redfish/v1/ComponentIntegrity/HGX_IRoT_GPU_0/Actions/ComponentIntegrity.SPDMGetSignedMeasurements/data";

    #[tokio::test]
    async fn evidence_request_persists_the_task_and_a_poll_reads_the_recorded_result() {
        let task = |state: &str, headers: serde_json::Value| {
            json!({
                "@odata.id": TASK,
                "@odata.type": "#Task.v1_7_0.Task",
                "Id": "95",
                "Name": "Task 95",
                "TaskState": state,
                "Payload": {"HttpHeaders": headers},
            })
        };
        let bmc = Fixture::new("NVIDIA", "GB200 NVL", "System_0", "HGX_BMC_0")
            .document(
                "/redfish/v1",
                json!({
                    "@odata.id": "/redfish/v1",
                    "Id": "RootService",
                    "Name": "Root Service",
                    "ComponentIntegrity": {"@odata.id": COLLECTION},
                    "Links": {"Sessions": {"@odata.id": "/redfish/v1/SessionService/Sessions"}},
                }),
            )
            .document(
                COLLECTION,
                json!({
                    "@odata.id": COLLECTION,
                    "@odata.type": "#ComponentIntegrityCollection.ComponentIntegrityCollection",
                    "Name": "Component Integrity",
                    "Members": [{"@odata.id": COMPONENT}],
                }),
            )
            .document(
                COMPONENT,
                json!({
                    "@odata.id": COMPONENT,
                    "@odata.type": "#ComponentIntegrity.v1_2_0.ComponentIntegrity",
                    "Id": "HGX_IRoT_GPU_0",
                    "Name": "GPU 0 root of trust",
                    "ComponentIntegrityEnabled": true,
                    "ComponentIntegrityType": "SPDM",
                    "ComponentIntegrityTypeVersion": "1.1.0",
                    "TargetComponentURI": "/redfish/v1/Chassis/HGX_GPU_0",
                    "Actions": {"#ComponentIntegrity.SPDMGetSignedMeasurements": {"target": TARGET}},
                }),
            )
            .respond(
                Method::POST,
                TARGET,
                StatusCode::OK,
                Some(task("Running", json!([]))),
            )
            .document(TASK, task("Completed", json!([format!("Location: {DATA}")])))
            .document(
                DATA,
                json!({
                    "HashingAlgorithm": "TPM_ALG_SHA_384",
                    "SignedMeasurements": "bWVhc3VyZW1lbnRz",
                    "SigningAlgorithm": "TPM_ALG_ECDSA_ECC_NIST_P384",
                    "Version": "1.1",
                }),
            )
            .build()
            .await;
        let cx = bmc.lazy_cx();

        let progress = StandardAttestation
            .request_evidence(&cx, "HGX_IRoT_GPU_0", &[0xab, 0xcd])
            .await
            .expect("request accepted");
        let pending = OperationReference::RedfishTask {
            uri: TASK.to_string().into(),
            retry_after_seconds: None,
            task: None,
        };
        assert!(
            matches!(&progress, EvidenceProgress::Pending(reference) if *reference == pending),
            "expected the task to poll, got {progress:?}"
        );
        assert_eq!(body(&bmc.writes()[0]), json!({"Nonce": "abcd"}));

        match StandardAttestation.poll_evidence(&cx, &pending).await {
            Ok(EvidenceProgress::Ready(measurements)) => {
                assert_eq!(measurements.hashing_algorithm, "TPM_ALG_SHA_384");
                assert_eq!(measurements.signed_measurements, "bWVhc3VyZW1lbnRz");
                assert_eq!(
                    measurements.signing_algorithm,
                    "TPM_ALG_ECDSA_ECC_NIST_P384"
                );
                assert_eq!(measurements.version, "1.1");
            }
            other => panic!("expected measurements, got {other:?}"),
        }
    }
}
