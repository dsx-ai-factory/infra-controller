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

//! Model for operating system definitions (CRUD resource, table operating_systems).
//!
//! Conversions follow db <-> model <-> RPC: database rows are converted to
//! this model (in api-db), then this model is converted to RPC types (here).
//! The model type name matches the RPC message name (OperatingSystem).

use carbide_ipxe_renderer::{IpxeTemplateArtifact, IpxeTemplateParameter};

/// Database value for the raw inline iPXE script OS type.
pub const OS_TYPE_IPXE: &str = "iPXE";
/// Database value for the iPXE OS definition (template-based) OS type.
pub const OS_TYPE_TEMPLATED_IPXE: &str = "ipxe_os_definition";

/// Operating system definition (list/get/create/update response).
///
/// Name matches the RPC message `rpc::forge::OperatingSystem`;
/// DB row type is `OperatingSystem` (in api-db).
#[derive(Clone, Debug)]
pub struct OperatingSystem {
    pub id: String,
    pub name: String,
    pub description: Option<String>,
    pub tenant_organization_id: Option<String>,
    pub type_: String,
    pub status: String,
    pub is_active: bool,
    pub allow_override: bool,
    pub phone_home_enabled: bool,
    pub user_data: Option<String>,
    pub created: String,
    pub updated: String,
    pub ipxe_script: Option<String>,
    pub ipxe_template_id: Option<String>,
    pub ipxe_template_parameters: Vec<IpxeTemplateParameter>,
    pub ipxe_template_artifacts: Vec<IpxeTemplateArtifact>,
    pub ipxe_template_definition_hash: Option<String>,
}
