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

use carbide_uuid::power_shelf::PowerShelfId;
use clap::Parser;
use rpc::forge as forgerpc;

/// Drive one or more power shelves into maintenance and request a power
/// operation (PowerOn / PowerOff). All listed power shelves receive the same
/// operation in a single atomic request.
#[derive(Parser, Debug)]
#[allow(clippy::enum_variant_names)]
pub(crate) enum Args {
    /// Request the listed power shelves to power on.
    #[command(after_long_help = "\
EXAMPLES:

Power on a power shelf:
    $ nico-admin-cli power-shelf maintenance power-on --power-shelf-id 12345678-1234-5678-90ab-cdef01234567

Power on several at once, citing a reference ticket:
    $ nico-admin-cli power-shelf maintenance power-on \
    --power-shelf-id 12345678-1234-5678-90ab-cdef01234567 abcdef01-2345-6789-abcd-ef0123456789 \
    --reference https://tickets.example.com/PS-42

")]
    PowerOn(MaintenancePowerArgs),
    /// Request the listed power shelves to power off.
    #[command(after_long_help = "\
EXAMPLES:

Power off a power shelf (forced, the default):
    $ nico-admin-cli power-shelf maintenance power-off --power-shelf-id 12345678-1234-5678-90ab-cdef01234567

Request a graceful shutdown instead of a forced power-off:
    $ nico-admin-cli power-shelf maintenance power-off --power-shelf-id 12345678-1234-5678-90ab-cdef01234567 --graceful

Power off several at once, citing a reference ticket:
    $ nico-admin-cli power-shelf maintenance power-off \
    --power-shelf-id 12345678-1234-5678-90ab-cdef01234567 abcdef01-2345-6789-abcd-ef0123456789 \
    --reference https://tickets.example.com/PS-42

")]
    PowerOff(PowerOffArgs),
}

#[derive(Parser, Debug)]
pub(crate) struct MaintenancePowerArgs {
    /// One or more Power Shelf IDs. Repeat the flag or pass multiple values:
    ///   --power-shelf-id <id1> --power-shelf-id <id2>
    ///   --power-shelf-id <id1> <id2>
    #[clap(
        long = "power-shelf-id",
        visible_alias = "id",
        required(true),
        num_args = 1..,
        value_name = "POWER_SHELF_ID",
        help = "One or more Power Shelf IDs to drive into maintenance"
    )]
    power_shelf_ids: Vec<PowerShelfId>,

    #[clap(
        long,
        visible_alias = "ref",
        help = "URL of reference (ticket, issue, etc) for this maintenance request"
    )]
    reference: Option<String>,
}

/// Arguments for `power-off`. Adds `--graceful` to the shared power arguments so
/// an operator can opt into an OS-ordered shutdown; without it a power-off is
/// forced.
#[derive(Parser, Debug)]
pub(crate) struct PowerOffArgs {
    #[clap(flatten)]
    common: MaintenancePowerArgs,

    #[clap(
        long,
        help = "Request a graceful OS-ordered shutdown instead of the default forced power-off"
    )]
    graceful: bool,
}

impl Args {
    pub(super) fn into_request(self) -> forgerpc::PowerShelfMaintenanceRequest {
        match self {
            Args::PowerOn(args) => forgerpc::PowerShelfMaintenanceRequest {
                power_shelf_ids: args.power_shelf_ids,
                operation: forgerpc::PowerShelfMaintenanceOperation::PowerOn.into(),
                reference: args.reference,
                // `graceful` applies only to power-off; leave it unset here.
                graceful: None,
            },
            Args::PowerOff(args) => forgerpc::PowerShelfMaintenanceRequest {
                power_shelf_ids: args.common.power_shelf_ids,
                operation: forgerpc::PowerShelfMaintenanceOperation::PowerOff.into(),
                reference: args.common.reference,
                graceful: Some(args.graceful),
            },
        }
    }
}

#[cfg(test)]
mod tests {
    use std::str::FromStr;

    use carbide_uuid::power_shelf::PowerShelfId;

    use super::{Args, MaintenancePowerArgs, PowerOffArgs};

    const SAMPLE_PS_ID_1: &str = "ps100htjtiaehv1n5vh67tbmqq4eabcjdng40f7jupsadbedhruh6rag1l0";
    const SAMPLE_PS_ID_2: &str = "ps100hsasb5dsh6e6ogogslpovne4rj82rp9jlf00qd7mcvmaadv85phk3g";

    fn parse_ps_id(id: &str) -> PowerShelfId {
        PowerShelfId::from_str(id)
            .unwrap_or_else(|error| panic!("invalid sample power-shelf id {id}: {error}"))
    }

    #[test]
    fn power_on_into_request_uses_power_on_operation() {
        let args = Args::PowerOn(MaintenancePowerArgs {
            power_shelf_ids: vec![parse_ps_id(SAMPLE_PS_ID_1)],
            reference: Some("ref-1".to_string()),
        });
        let request = args.into_request();

        assert_eq!(
            request.operation,
            rpc::forge::PowerShelfMaintenanceOperation::PowerOn as i32,
        );
        assert_eq!(request.power_shelf_ids, vec![parse_ps_id(SAMPLE_PS_ID_1)]);
        assert_eq!(request.reference.as_deref(), Some("ref-1"));
        assert_eq!(
            request.graceful, None,
            "power-on must not set the power-off graceful flag"
        );
    }

    #[test]
    fn power_off_into_request_uses_power_off_operation() {
        let args = Args::PowerOff(PowerOffArgs {
            common: MaintenancePowerArgs {
                power_shelf_ids: vec![parse_ps_id(SAMPLE_PS_ID_1), parse_ps_id(SAMPLE_PS_ID_2)],
                reference: None,
            },
            graceful: false,
        });
        let request = args.into_request();

        assert_eq!(
            request.operation,
            rpc::forge::PowerShelfMaintenanceOperation::PowerOff as i32,
        );
        assert_eq!(
            request.power_shelf_ids,
            vec![parse_ps_id(SAMPLE_PS_ID_1), parse_ps_id(SAMPLE_PS_ID_2)],
        );
        assert!(request.reference.is_none());
        assert_eq!(
            request.graceful,
            Some(false),
            "omitting --graceful requests a forced power-off"
        );
    }

    #[test]
    fn power_off_into_request_sets_graceful_flag() {
        let args = Args::PowerOff(PowerOffArgs {
            common: MaintenancePowerArgs {
                power_shelf_ids: vec![parse_ps_id(SAMPLE_PS_ID_1)],
                reference: None,
            },
            graceful: true,
        });
        let request = args.into_request();

        assert_eq!(
            request.operation,
            rpc::forge::PowerShelfMaintenanceOperation::PowerOff as i32,
        );
        assert_eq!(request.graceful, Some(true));
    }
}
