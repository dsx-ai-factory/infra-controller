// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

use k8s_openapi::ByteString;
use kube::core::{ApiResource, GroupVersionKind, Object};
use serde::{Deserialize, Serialize};

use crate::IssuerKind;

// The subset of cert-manager.io/v1 needed for CSR submission and issuance.
// https://cert-manager.io/docs/reference/api-docs/#cert-manager.io/v1.CertificateRequest
pub(crate) type CertificateRequest = Object<RequestSpec, RequestStatus>;

pub(crate) fn api_resource() -> ApiResource {
    ApiResource::from_gvk_with_plural(
        &GroupVersionKind::gvk("cert-manager.io", "v1", "CertificateRequest"),
        "certificaterequests",
    )
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct RequestSpec {
    pub(crate) request: ByteString,
    pub(crate) issuer_ref: IssuerRef,
    pub(crate) duration: String,
    #[serde(default, rename = "isCA")]
    pub(crate) is_ca: bool,
    pub(crate) usages: Vec<String>,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub(crate) struct IssuerRef {
    pub(crate) name: String,
    pub(crate) kind: IssuerKind,
    pub(crate) group: String,
}

#[derive(Clone, Debug, Default, Deserialize, Serialize)]
pub(crate) struct RequestStatus {
    #[serde(default)]
    pub(crate) conditions: Vec<RequestCondition>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) certificate: Option<ByteString>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) ca: Option<ByteString>,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub(crate) struct RequestCondition {
    #[serde(rename = "type")]
    pub(crate) type_: String,
    pub(crate) status: String,
    #[serde(default)]
    pub(crate) reason: String,
    #[serde(default)]
    pub(crate) message: String,
}
