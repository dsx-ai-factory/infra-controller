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

pub mod compute_allocation;
pub mod device;
pub mod domain;
pub mod dpa_interface;
pub mod dpu_remediations;
pub mod extension_service;
pub mod infiniband;
pub mod instance;
pub mod instance_type;
pub mod ipxe_template;
pub mod machine;
pub mod machine_validation;
pub mod measured_boot;
pub mod network;
pub mod network_security_group;
pub mod nvlink;
pub mod operating_system;
pub mod power_shelf;
pub mod rack;
pub mod secret;
pub mod site_prefix;
pub mod spx;
pub mod switch;
pub mod typed_uuids;
pub mod vpc;
pub mod vpc_peering;

/// DbPrimaryUuid is a trait intended for primary keys which
/// derive the sqlx UUID type. The intent is the db_primary_uuid_name
/// function should return the name of the column for the primary
/// UUID-typed key, which allows dynamic compositon of a SQL query.
///
/// This was originally introduced as part of the measured boot
/// generics (and lived in src/measured_boot/), but moved here.
pub trait DbPrimaryUuid {
    fn db_primary_uuid_name() -> &'static str;
}

/// DbTable is a trait intended for table records which derive
/// sqlx FromRow. The intent here is db_table_name() will return
/// the actual name of the table the records are in, allowing for
/// dynamic composition of an SQL query for that table.
///
/// This was originally introduced as part of the measured boot
/// generics (and lived in src/measured_boot/), but moved here.
pub trait DbTable {
    fn db_table_name() -> &'static str;
}

#[derive(thiserror::Error, Debug)]
pub enum UuidConversionError {
    #[error("invalid UUID for {ty}: {value}")]
    InvalidUuid { ty: &'static str, value: String },
    #[error("missing ID for {0}")]
    MissingId(&'static str),
    #[error("invalid MachineId: {0}")]
    InvalidMachineId(String),
    #[error("UUID parse error: {0}")]
    UuidError(#[from] uuid::Error),
}

#[derive(
    Ord,
    PartialOrd,
    serde::Deserialize,
    serde::Serialize,
    Clone,
    PartialEq,
    Eq,
    Hash,
    ::prost::Message,
)]
pub(crate) struct CommonUuidPlaceholder {
    #[prost(string, tag = "1")]
    pub value: ::prost::alloc::string::String,
}
