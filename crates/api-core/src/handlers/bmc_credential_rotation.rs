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
use ::rpc::forge as rpc;
use ::rpc::forge::bmc_credential_rotation_request::Mode;
use carbide_uuid::device::DeviceId;
use mac_address::MacAddress;
use sqlx::PgConnection;
use tonic::{Request, Response, Status};

use crate::CarbideError;
use crate::api::{Api, log_machine_id, log_request_data};

/// Operator force-converge escape hatch: record (or clear) a request to
/// immediately rotate a device's BMC credentials, bypassing the passive
/// site-wide gate and the device's backoff quarantine. The target BMC is
/// addressed by the owning device's id (a machine, switch, or power shelf), its
/// BMC MAC, or a combination (see [`resolve_target`]); the flag is written on
/// that device's row. The owning device's state controller consumes the request
/// on its next sweep; this handler only writes the flag (it performs no Redfish
/// work itself).
pub(crate) async fn trigger_bmc_credential_rotation(
    api: &Api,
    request: Request<rpc::BmcCredentialRotationRequest>,
) -> Result<Response<()>, Status> {
    log_request_data(&request);
    let req = request.into_inner();
    let mode = req.mode();

    let mut txn = api.txn_begin().await?;

    let target = resolve_target(&mut txn, req.device_id, req.bmc_mac).await?;

    match mode {
        Mode::Set => match target {
            DeviceId::Machine(id) => {
                db::machine::set_bmc_credential_rotation_requested(&mut txn, &id).await?;
            }
            DeviceId::Switch(id) => {
                db::switch::set_bmc_credential_rotation_requested(&mut txn, id).await?;
            }
            DeviceId::PowerShelf(id) => {
                db::power_shelf::set_bmc_credential_rotation_requested(&mut txn, id).await?;
            }
        },
        Mode::Clear => match target {
            DeviceId::Machine(id) => {
                db::machine::clear_bmc_credential_rotation_requested(&mut txn, &id).await?;
            }
            DeviceId::Switch(id) => {
                db::switch::clear_bmc_credential_rotation_requested(&mut txn, id).await?;
            }
            DeviceId::PowerShelf(id) => {
                db::power_shelf::clear_bmc_credential_rotation_requested(&mut txn, id).await?;
            }
        },
        // An omitted `mode` decodes as `Unspecified`; reject it rather than let
        // a request fall through to an action it did not name.
        Mode::Unspecified => {
            return Err(
                CarbideError::InvalidArgument("mode must be set or clear".to_string()).into(),
            );
        }
    };

    txn.commit().await?;

    Ok(Response::new(()))
}

/// Resolve the device that owns the target BMC from an operator request that
/// carries a `device_id` (a machine, switch, or power shelf), a BMC MAC, or
/// both. A device has exactly one BMC, so any single identifier uniquely names
/// it. When a MAC is supplied alongside a `device_id` they must resolve to the
/// same device, which lets an operator double-check that a MAC pulled from an
/// alert really is the BMC of the device they mean.
async fn resolve_target(
    txn: &mut PgConnection,
    device_id: Option<DeviceId>,
    bmc_mac: Option<String>,
) -> Result<DeviceId, CarbideError> {
    let bmc_mac = bmc_mac
        .map(|mac| {
            mac.parse::<MacAddress>().map_err(|_| {
                CarbideError::InvalidArgument(format!("bmc_mac '{mac}' is not a valid MAC address"))
            })
        })
        .transpose()?;

    // A MAC uniquely names one BMC device; resolve which device kind owns it,
    // keeping the parsed MAC alongside its owner for cross-check error messages.
    let mac_address_and_target = match bmc_mac {
        Some(mac) => Some((mac, resolve_mac_owner(txn, mac).await?)),
        None => None,
    };

    let target = match (device_id, mac_address_and_target) {
        // Both supplied: the explicit id and the MAC's owner must be the same
        // device, so a mismatched cross-check is rejected.
        (Some(id_target), Some((mac, mac_target))) => {
            if id_target != mac_target {
                return Err(CarbideError::InvalidArgument(format!(
                    "bmc {} belongs to {} {}, not the requested {} {}",
                    mac,
                    mac_target.kind(),
                    mac_target,
                    id_target.kind(),
                    id_target,
                )));
            }
            id_target
        }
        (Some(id_target), None) => id_target,
        // MAC only: the owner the MAC resolved to.
        (None, Some((_mac, mac_target))) => mac_target,
        (None, None) => {
            return Err(CarbideError::InvalidArgument(
                "one of device_id or bmc_mac must be provided".to_string(),
            ));
        }
    };

    if let DeviceId::Machine(machine_id) = &target {
        log_machine_id(machine_id);
    }
    Ok(target)
}

/// Resolve which device kind owns a BMC MAC. A physical BMC MAC lives on exactly
/// one interface row, keyed to a machine, a switch, *or* a power shelf, so try
/// the machine resolver first (its `machine_id`-keyed BMC interface), then the
/// switch one, then the power shelf one.
async fn resolve_mac_owner(
    txn: &mut PgConnection,
    mac: MacAddress,
) -> Result<DeviceId, CarbideError> {
    if let Some(machine_id) = db::machine_topology::find_machine_id_by_bmc_mac(txn, mac).await? {
        return Ok(DeviceId::Machine(machine_id));
    }
    if let Some(switch_id) = db::switch::find_switch_id_by_bmc_mac(txn, mac).await? {
        return Ok(DeviceId::Switch(switch_id));
    }
    if let Some(power_shelf) = db::power_shelf::find_by_bmc_mac_address(txn, mac).await? {
        return Ok(DeviceId::PowerShelf(power_shelf.id));
    }
    Err(CarbideError::NotFoundError {
        kind: "BMC",
        id: mac.to_string(),
    })
}

#[cfg(test)]
mod tests {
    use carbide_uuid::power_shelf::{PowerShelfId, PowerShelfIdSource, PowerShelfType};
    use model::power_shelf::{NewPowerShelf, PowerShelfConfig};

    use super::*;

    fn mac(last: u8) -> MacAddress {
        MacAddress::new([0x02, 0, 0, 0, 0, last])
    }

    /// Insert a power shelf carrying `bmc_mac` on its row so `resolve_mac_owner`
    /// resolves the MAC to it. `power_shelves.bmc_mac_address` has a foreign key
    /// into `expected_power_shelves`, so seed that row first. Returns the shelf's
    /// id.
    async fn seed_power_shelf(
        conn: &mut PgConnection,
        seed: u8,
        bmc_mac: MacAddress,
    ) -> PowerShelfId {
        sqlx::query(
            "INSERT INTO expected_power_shelves \
                 (serial_number, bmc_mac_address, bmc_username, bmc_password) \
             VALUES ($1, $2::macaddr, 'admin', 'pw')",
        )
        .bind(format!("resolve-target-sn-{seed}"))
        .bind(bmc_mac)
        .execute(&mut *conn)
        .await
        .expect("seeding the expected_power_shelves row should succeed");

        let id = PowerShelfId::new(
            PowerShelfIdSource::ProductBoardChassisSerial,
            [seed; 32],
            PowerShelfType::Rack,
        );
        let new_power_shelf = NewPowerShelf {
            id,
            config: PowerShelfConfig {
                name: format!("resolve-target-shelf-{seed}"),
                capacity: Some(100),
                voltage: Some(240),
            },
            bmc_mac_address: Some(bmc_mac),
            metadata: None,
            rack_id: None,
        };
        db::power_shelf::create(conn, &new_power_shelf)
            .await
            .expect("seeding a power shelf should succeed");
        id
    }

    /// A power shelf device id resolves to the owning power shelf, whether
    /// addressed by id alone, by its BMC MAC alone, or by both when they agree.
    #[crate::sqlx_test]
    async fn resolve_target_resolves_a_power_shelf_by_id_and_mac(pool: sqlx::PgPool) {
        let mut conn = pool.acquire().await.unwrap();
        let shelf_mac = mac(1);
        let shelf_id = seed_power_shelf(&mut conn, 1, shelf_mac).await;

        // Id only: no MAC lookup, returns the addressed shelf.
        let by_id = resolve_target(&mut conn, Some(DeviceId::PowerShelf(shelf_id)), None)
            .await
            .expect("a power shelf id alone should resolve");
        assert_eq!(by_id, DeviceId::PowerShelf(shelf_id));

        // MAC only: the owner the MAC resolves to.
        let by_mac = resolve_target(&mut conn, None, Some(shelf_mac.to_string()))
            .await
            .expect("a power shelf BMC MAC alone should resolve to its shelf");
        assert_eq!(by_mac, DeviceId::PowerShelf(shelf_id));

        // Id + agreeing MAC: the cross-check passes.
        let by_both = resolve_target(
            &mut conn,
            Some(DeviceId::PowerShelf(shelf_id)),
            Some(shelf_mac.to_string()),
        )
        .await
        .expect("a power shelf id with its own BMC MAC should resolve");
        assert_eq!(by_both, DeviceId::PowerShelf(shelf_id));
    }

    /// A power shelf id cross-checked against a BMC MAC that belongs to a
    /// *different* device is rejected rather than silently trusting either.
    #[crate::sqlx_test]
    async fn resolve_target_rejects_a_power_shelf_id_that_disagrees_with_the_mac(
        pool: sqlx::PgPool,
    ) {
        let mut conn = pool.acquire().await.unwrap();
        let shelf_a = seed_power_shelf(&mut conn, 1, mac(1)).await;
        let mac_b = mac(2);
        let _shelf_b = seed_power_shelf(&mut conn, 2, mac_b).await;

        // Request shelf A but hand over shelf B's BMC MAC: the id and the MAC's
        // owner disagree, so the request is an invalid argument.
        let err = resolve_target(
            &mut conn,
            Some(DeviceId::PowerShelf(shelf_a)),
            Some(mac_b.to_string()),
        )
        .await
        .expect_err("a power shelf id that disagrees with the MAC owner must be rejected");
        assert!(
            matches!(err, CarbideError::InvalidArgument(_)),
            "expected InvalidArgument, got {err:?}"
        );
    }
}
