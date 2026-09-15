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

use std::net::IpAddr;

use carbide_uuid::rack::RackId;
use db::db_read::DbReader;
use db::{self, DatabaseResult};

use crate::config::NvLinkConfig;

/// Whether an NMX-C monitor group is keyed by chassis serial or rack id.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum ManagedHostGroupType {
    Chassis,
    Rack,
}

impl ManagedHostGroupType {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Chassis => "chassis",
            Self::Rack => "rack",
        }
    }
}

impl std::fmt::Display for ManagedHostGroupType {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

/// Default NMX-C gRPC port when switch NVOS info does not specify one.
pub const NMX_C_DEFAULT_GRPC_PORT: u16 = 9370;

fn nmx_c_endpoint_uses_tls(config: &NvLinkConfig) -> bool {
    config.nmx_c_tls_client_cert_path.is_some() && config.nmx_c_tls_client_key_path.is_some()
}

fn nmx_c_endpoint_scheme(config: &NvLinkConfig) -> &'static str {
    if config.allow_insecure && !nmx_c_endpoint_uses_tls(config) {
        "http"
    } else {
        "https"
    }
}

/// Builds an NMX-C gRPC URL from a switch NVOS IP (same data as RPC `SwitchNvosInfo`).
pub fn nmx_c_endpoint_url_from_nvos_ip(
    ip: &IpAddr,
    port: Option<u16>,
    config: &NvLinkConfig,
) -> String {
    format!(
        "{}://{}:{}",
        nmx_c_endpoint_scheme(config),
        ip,
        port.or(config.nmx_c_endpoint_port)
            .unwrap_or(NMX_C_DEFAULT_GRPC_PORT)
    )
}

/// Outcome of resolving an NMX-C gRPC endpoint for a chassis- or rack-scoped machine group.
///
/// The `Rack` path can fail in two distinct ways that callers should not conflate: no
/// qualifying switch exists at all, versus a qualifying switch exists but its NVOS IP has not
/// been resolved yet. Collapsing both into a bare `None` misdiagnoses the latter as "switch not
/// found" when a switch was in fact found.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum NmxCEndpointResolution {
    /// The endpoint URL was resolved.
    Resolved(String),
    /// No config row (`Chassis`) or no ready, control-plane-configured switch (`Rack`) exists.
    NotFound,
    /// `Rack` only: a ready, control-plane-configured switch exists in the rack, but its NVOS
    /// IP has not been resolved (`find_switch_endpoints_by_ids` returned a null `nvos_ip`).
    SwitchMissingNvosIp,
}

impl NmxCEndpointResolution {
    /// Discards the distinction between failure modes, for callers that only need the URL.
    pub fn into_url(self) -> Option<String> {
        match self {
            Self::Resolved(url) => Some(url),
            Self::NotFound | Self::SwitchMissingNvosIp => None,
        }
    }
}

/// Resolves the NMX-C gRPC endpoint URL for a chassis- or rack-scoped machine group.
///
/// - [`ManagedHostGroupType::Chassis`]: looks up `nvlink_nmxc_endpoints` by `chassis_serial`.
/// - [`ManagedHostGroupType::Rack`]: uses the first ready Fabric Manager control-plane switch's
///   NVOS IP in `rack_id`. Does not fall back to the chassis-serial mapping.
pub async fn resolve_nmx_c_endpoint_url<DB>(
    db: &mut DB,
    group_type: ManagedHostGroupType,
    rack_id: Option<&RackId>,
    chassis_serial: Option<&str>,
    nvlink_config: &NvLinkConfig,
) -> DatabaseResult<NmxCEndpointResolution>
where
    for<'db> &'db mut DB: DbReader<'db>,
{
    match group_type {
        ManagedHostGroupType::Chassis => {
            let Some(chassis_serial) = chassis_serial else {
                return Ok(NmxCEndpointResolution::NotFound);
            };
            Ok(
                match db::nvlink_nmxc_endpoints::find_by_chassis_serial(
                    &mut *db,
                    chassis_serial.trim(),
                )
                .await?
                {
                    Some(row) => NmxCEndpointResolution::Resolved(row.endpoint),
                    None => NmxCEndpointResolution::NotFound,
                },
            )
        }
        ManagedHostGroupType::Rack => {
            let Some(rack_id) = rack_id else {
                return Ok(NmxCEndpointResolution::NotFound);
            };

            let switch_ids = db::switch::find_ready_control_plane_configured_switch_ids_in_rack(
                &mut *db, rack_id,
            )
            .await?;

            let Some(switch_id) = switch_ids.first() else {
                return Ok(NmxCEndpointResolution::NotFound);
            };

            let endpoint_rows =
                db::switch::find_switch_endpoints_by_ids(&mut *db, &[*switch_id]).await?;

            Ok(
                match endpoint_rows.first().and_then(|row| row.nvos_ip.as_ref()) {
                    Some(nvos_ip) => NmxCEndpointResolution::Resolved(
                        nmx_c_endpoint_url_from_nvos_ip(nvos_ip, None, nvlink_config),
                    ),
                    None => NmxCEndpointResolution::SwitchMissingNvosIp,
                },
            )
        }
    }
}

#[cfg(test)]
mod tests {
    use std::net::Ipv4Addr;

    use carbide_macros::sqlx_test;
    use carbide_uuid::rack::{RackId, RackProfileId};
    use model::rack::RackConfig;
    use model::switch::{
        CONTROL_PLANE_STATE_CONFIGURED, FabricManagerState, FabricManagerStatus,
        SwitchControllerState,
    };

    use super::*;

    #[test]
    fn endpoint_url_uses_http_when_allow_insecure() {
        let config = NvLinkConfig {
            allow_insecure: true,
            ..Default::default()
        };
        assert_eq!(
            nmx_c_endpoint_url_from_nvos_ip(&IpAddr::V4(Ipv4Addr::new(10, 0, 0, 1)), None, &config),
            "http://10.0.0.1:9370"
        );
    }

    #[test]
    fn endpoint_url_uses_https_by_default() {
        let config = NvLinkConfig::default();
        assert_eq!(
            nmx_c_endpoint_url_from_nvos_ip(&IpAddr::V4(Ipv4Addr::new(10, 0, 0, 1)), None, &config),
            "https://10.0.0.1:9370"
        );
    }

    #[test]
    fn endpoint_url_uses_configured_port() {
        let config = NvLinkConfig {
            nmx_c_endpoint_port: Some(9601),
            ..Default::default()
        };
        assert_eq!(
            nmx_c_endpoint_url_from_nvos_ip(&IpAddr::V4(Ipv4Addr::new(10, 0, 0, 1)), None, &config),
            "https://10.0.0.1:9601"
        );
    }

    #[sqlx_test]
    async fn resolve_chassis_uses_nvlink_nmxc_endpoints_mapping(
        pool: sqlx::PgPool,
    ) -> Result<(), Box<dyn std::error::Error>> {
        let chassis_serial = "CHASSIS-ENDPOINT-1";
        let mapped_endpoint = "https://nmxc.example:9370";
        let config = NvLinkConfig::default();

        let mut txn = pool.begin().await?;
        db::nvlink_nmxc_endpoints::create(txn.as_mut(), chassis_serial, mapped_endpoint).await?;

        let resolved = resolve_nmx_c_endpoint_url(
            txn.as_mut(),
            ManagedHostGroupType::Chassis,
            None,
            Some(chassis_serial),
            &config,
        )
        .await?;
        assert_eq!(
            resolved,
            NmxCEndpointResolution::Resolved(mapped_endpoint.to_string())
        );
        txn.rollback().await?;
        Ok(())
    }

    #[sqlx_test]
    async fn resolve_rack_uses_ready_switch_nvos_ip(
        pool: sqlx::PgPool,
    ) -> Result<(), Box<dyn std::error::Error>> {
        let rack_id: RackId = "rack-nmxc-endpoint".parse()?;
        let config = NvLinkConfig::default();

        let mut txn = pool.begin().await?;
        db::rack::create(
            txn.as_mut(),
            &rack_id,
            Some(&RackProfileId::new("NVL72")),
            &RackConfig::default(),
            None,
        )
        .await?;
        txn.commit().await?;

        let mut txn = pool.begin().await?;
        let switch =
            db::test_support::switch::create_seeded_discovered(txn.as_mut(), 1, "Switch1").await?;
        txn.commit().await?;

        let mut txn = pool.begin().await?;
        sqlx::query("UPDATE switches SET rack_id = $1 WHERE id = $2")
            .bind(&rack_id)
            .bind(switch.id)
            .execute(txn.as_mut())
            .await?;

        let switch = db::switch::find_by_id(txn.as_mut(), &switch.id)
            .await?
            .expect("switch should exist");
        assert!(
            db::switch::try_update_controller_state(
                txn.as_mut(),
                switch.id,
                switch.controller_state.version,
                switch.controller_state.version.increment(),
                &SwitchControllerState::Ready,
            )
            .await?
        );
        db::switch::update_fabric_manager_status(
            txn.as_mut(),
            switch.id,
            Some(&FabricManagerStatus {
                fabric_manager_state: FabricManagerState::Ok,
                addition_info: Some(CONTROL_PLANE_STATE_CONFIGURED.to_string()),
                reason: None,
                error_message: None,
            }),
        )
        .await?;

        let expected_nvos_ip = db::switch::find_switch_endpoints_by_ids(txn.as_mut(), &[switch.id])
            .await?
            .pop()
            .expect("switch endpoint")
            .nvos_ip
            .expect("seeded switch should have an NVOS IP");
        let expected_url = nmx_c_endpoint_url_from_nvos_ip(&expected_nvos_ip, None, &config);

        let resolved = resolve_nmx_c_endpoint_url(
            txn.as_mut(),
            ManagedHostGroupType::Rack,
            Some(&rack_id),
            None,
            &config,
        )
        .await?;
        assert_eq!(resolved, NmxCEndpointResolution::Resolved(expected_url));
        txn.rollback().await?;
        Ok(())
    }

    #[sqlx_test]
    async fn resolve_rack_reports_switch_missing_nvos_ip_distinctly_from_not_found(
        pool: sqlx::PgPool,
    ) -> Result<(), Box<dyn std::error::Error>> {
        let rack_id: RackId = "rack-nmxc-no-nvos-ip".parse()?;
        let config = NvLinkConfig::default();

        let mut txn = pool.begin().await?;
        db::rack::create(
            txn.as_mut(),
            &rack_id,
            Some(&RackProfileId::new("NVL72")),
            &RackConfig::default(),
            None,
        )
        .await?;
        txn.commit().await?;

        let mut txn = pool.begin().await?;
        let switch =
            db::test_support::switch::create_seeded_discovered(txn.as_mut(), 2, "Switch2").await?;
        txn.commit().await?;

        let mut txn = pool.begin().await?;
        sqlx::query("UPDATE switches SET rack_id = $1 WHERE id = $2")
            .bind(&rack_id)
            .bind(switch.id)
            .execute(txn.as_mut())
            .await?;

        // Simulate a switch whose NVOS management interface has never been discovered:
        // ready and control-plane-configured, but with no NVOS MAC/IP to resolve.
        sqlx::query(
            "UPDATE expected_switches SET nvos_mac_addresses = NULL WHERE bmc_mac_address = $1",
        )
        .bind(
            switch
                .bmc_mac_address
                .expect("seeded switch should have a BMC MAC"),
        )
        .execute(txn.as_mut())
        .await?;

        let switch = db::switch::find_by_id(txn.as_mut(), &switch.id)
            .await?
            .expect("switch should exist");
        assert!(
            db::switch::try_update_controller_state(
                txn.as_mut(),
                switch.id,
                switch.controller_state.version,
                switch.controller_state.version.increment(),
                &SwitchControllerState::Ready,
            )
            .await?
        );
        db::switch::update_fabric_manager_status(
            txn.as_mut(),
            switch.id,
            Some(&FabricManagerStatus {
                fabric_manager_state: FabricManagerState::Ok,
                addition_info: Some(CONTROL_PLANE_STATE_CONFIGURED.to_string()),
                reason: None,
                error_message: None,
            }),
        )
        .await?;

        let endpoint_row = db::switch::find_switch_endpoints_by_ids(txn.as_mut(), &[switch.id])
            .await?
            .pop()
            .expect("switch endpoint row should still be returned");
        assert!(
            endpoint_row.nvos_ip.is_none(),
            "test setup should leave nvos_ip unresolved"
        );

        let resolved = resolve_nmx_c_endpoint_url(
            txn.as_mut(),
            ManagedHostGroupType::Rack,
            Some(&rack_id),
            None,
            &config,
        )
        .await?;
        assert_eq!(resolved, NmxCEndpointResolution::SwitchMissingNvosIp);
        txn.rollback().await?;
        Ok(())
    }

    #[sqlx_test]
    async fn resolve_rack_does_not_fall_back_to_chassis_mapping(
        pool: sqlx::PgPool,
    ) -> Result<(), Box<dyn std::error::Error>> {
        let rack_id: RackId = "rack-nmxc-no-fallback".parse()?;
        let chassis_serial = "CHASSIS-NO-FALLBACK";
        let mapped_endpoint = "https://chassis-fallback.example:9370";
        let config = NvLinkConfig::default();

        let mut txn = pool.begin().await?;
        db::rack::create(
            txn.as_mut(),
            &rack_id,
            Some(&RackProfileId::new("NVL72")),
            &RackConfig::default(),
            None,
        )
        .await?;
        db::nvlink_nmxc_endpoints::create(txn.as_mut(), chassis_serial, mapped_endpoint).await?;

        // Rack has no ready control-plane switch. Chassis mapping exists and must not be used.
        let rack_resolved = resolve_nmx_c_endpoint_url(
            txn.as_mut(),
            ManagedHostGroupType::Rack,
            Some(&rack_id),
            Some(chassis_serial),
            &config,
        )
        .await?;
        assert_eq!(rack_resolved, NmxCEndpointResolution::NotFound);

        let chassis_resolved = resolve_nmx_c_endpoint_url(
            txn.as_mut(),
            ManagedHostGroupType::Chassis,
            None,
            Some(chassis_serial),
            &config,
        )
        .await?;
        assert_eq!(
            chassis_resolved,
            NmxCEndpointResolution::Resolved(mapped_endpoint.to_string())
        );
        txn.rollback().await?;
        Ok(())
    }
}
