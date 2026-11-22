// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

use model::hardware_dispatch::{HardwareDispatch, HardwareDispatchOwner, MlxAdminCommand};

use crate::forge::NicFirmwareDispatchStatus;
use crate::forge::nic_firmware_dispatch_status::mlx_admin::Command;
use crate::forge::nic_firmware_dispatch_status::{InstanceReboot, MlxAdmin, Origin};

impl From<HardwareDispatch> for NicFirmwareDispatchStatus {
    fn from(value: HardwareDispatch) -> Self {
        let origin = match value.owner {
            HardwareDispatchOwner::MlxAdmin {
                machine_id,
                command,
            } => Origin::MlxAdmin(MlxAdmin {
                machine_id: Some(machine_id),
                command: Command::from(command).into(),
            }),
            HardwareDispatchOwner::InstanceReboot {
                machine_id,
                instance_id,
                bmc,
                last_attempt_at,
                reconcile_after,
            } => Origin::InstanceReboot(InstanceReboot {
                machine_id: Some(machine_id),
                instance_id: Some(instance_id),
                bmc: Some((&bmc).into()),
                last_attempt_at: last_attempt_at.map(Into::into),
                reconcile_after: reconcile_after.map(Into::into),
            }),
        };
        Self {
            token: Some(value.token.into()),
            started_at: Some(value.started_at.into()),
            settled_at: value.settled_at.map(Into::into),
            origin: Some(origin),
        }
    }
}

impl From<MlxAdminCommand> for Command {
    fn from(value: MlxAdminCommand) -> Self {
        match value {
            MlxAdminCommand::ProfileSync => Self::ProfileSync,
            MlxAdminCommand::ConfigSet => Self::ConfigSet,
            MlxAdminCommand::ConfigSync => Self::ConfigSync,
            MlxAdminCommand::Lock => Self::Lock,
            MlxAdminCommand::Unlock => Self::Unlock,
        }
    }
}

#[cfg(test)]
mod tests {
    use carbide_test_support::{Check, check_values, value_scenarios};
    use carbide_uuid::machine::MachineInterfaceId;
    use chrono::{DateTime, Utc};
    use model::bmc_info::BmcInfo;
    use model::test_support::machine_snapshot;
    use uuid::Uuid;

    use super::*;
    use crate::forge::{BmcEndpoint, Machine};

    /// Read the saved command through the same host-to-RPC conversion as machine status.
    fn machine_dispatch(record: Option<HardwareDispatch>) -> Option<NicFirmwareDispatchStatus> {
        let mut host = machine_snapshot::host_machine();
        host.status.hardware_dispatch = record;
        Machine::from(host).status.unwrap().nic_firmware_dispatch
    }

    #[test]
    fn machine_status_omits_absent_dispatch() {
        assert_eq!(machine_dispatch(None), None);
    }

    /// The host owns the reservation even when an attached DPU is the command target.
    #[test]
    fn machine_status_projects_mlx_dispatch() {
        let machine_id = machine_snapshot::dpu_machine_id(1).into();
        let token = Uuid::from_u128(42);
        let started_at = DateTime::from_timestamp(1_000, 123_456_000).unwrap();
        value_scenarios!(
            run = |command| {
                let record = HardwareDispatch {
                    token,
                    started_at,
                    settled_at: None,
                    owner: HardwareDispatchOwner::MlxAdmin { machine_id, command },
                };
                let status = machine_dispatch(Some(record)).unwrap();
                assert_eq!(status.token.unwrap().value, "00000000-0000-0000-0000-00000000002a");
                let started = status.started_at.unwrap();
                assert_eq!((started.seconds, started.nanos), (1_000, 123_456_000));
                assert_eq!(status.settled_at, None);
                let Some(Origin::MlxAdmin(mlx)) = status.origin else {
                    panic!("expected the saved MLX admin origin");
                };
                assert_eq!(mlx.machine_id, Some(machine_id));
                Command::try_from(mlx.command).unwrap()
            };
            "host reservation names the DPU Scout and its saved command" {
                MlxAdminCommand::ProfileSync => Command::ProfileSync,
                MlxAdminCommand::ConfigSet => Command::ConfigSet,
                MlxAdminCommand::ConfigSync => Command::ConfigSync,
                MlxAdminCommand::Lock => Command::Lock,
                MlxAdminCommand::Unlock => Command::Unlock,
            }
        );
    }

    /// Preserve the captured route and only the timestamps the reboot controller recorded.
    #[test]
    fn machine_status_projects_instance_reboot_dispatch() {
        #[derive(Default)]
        struct RebootEvidence {
            last_attempt_at: Option<DateTime<Utc>>,
            reconcile_after: Option<DateTime<Utc>>,
            settled_at: Option<DateTime<Utc>>,
            port: Option<u16>,
            interface_id: Option<MachineInterfaceId>,
        }

        let machine_id = *machine_snapshot::host_machine_id();
        let instance_id = Uuid::from_u128(51).into();
        let token = Uuid::from_u128(52);
        let started_at = DateTime::from_timestamp(100, 234_567_000).unwrap();
        let attempt = DateTime::from_timestamp_secs(101).unwrap();
        let boundary = DateTime::from_timestamp_secs(102).unwrap();
        let settled = DateTime::from_timestamp_secs(103).unwrap();
        let cases = [
            ("queued before its first attempt", RebootEvidence::default()),
            (
                "older unresolved attempt without the worker's reconciliation boundary",
                RebootEvidence {
                    last_attempt_at: Some(attempt),
                    ..Default::default()
                },
            ),
            (
                "attempt remains unresolved after its saved worker boundary",
                RebootEvidence {
                    last_attempt_at: Some(attempt),
                    reconcile_after: Some(boundary),
                    port: Some(8443),
                    interface_id: Some(Uuid::from_u128(53).into()),
                    ..Default::default()
                },
            ),
            (
                "settled after an attempt",
                RebootEvidence {
                    last_attempt_at: Some(attempt),
                    reconcile_after: Some(boundary),
                    settled_at: Some(settled),
                    ..Default::default()
                },
            ),
            (
                "settled after cancellation before any attempt",
                RebootEvidence {
                    settled_at: Some(settled),
                    ..Default::default()
                },
            ),
        ];
        check_values(
            cases.into_iter().map(|(scenario, case)| Check {
                scenario,
                input: Some(HardwareDispatch {
                    token,
                    started_at,
                    settled_at: case.settled_at,
                    owner: HardwareDispatchOwner::InstanceReboot {
                        machine_id,
                        instance_id,
                        bmc: BmcInfo {
                            ip: Some("192.0.2.10".parse().unwrap()),
                            mac: Some("02:00:00:00:00:01".parse().unwrap()),
                            port: case.port,
                            machine_interface_id: case.interface_id,
                            ..Default::default()
                        },
                        last_attempt_at: case.last_attempt_at,
                        reconcile_after: case.reconcile_after,
                    },
                }),
                expect: Some(NicFirmwareDispatchStatus {
                    token: Some(crate::common::Uuid {
                        value: "00000000-0000-0000-0000-000000000034".to_string(),
                    }),
                    started_at: Some(started_at.into()),
                    settled_at: case.settled_at.map(Into::into),
                    origin: Some(Origin::InstanceReboot(InstanceReboot {
                        machine_id: Some(machine_id),
                        instance_id: Some(instance_id),
                        bmc: Some(BmcEndpoint {
                            ip: Some("192.0.2.10".to_string()),
                            mac: Some("02:00:00:00:00:01".to_string()),
                            port: case.port.map(u32::from),
                            machine_interface_id: case.interface_id,
                        }),
                        last_attempt_at: case.last_attempt_at.map(Into::into),
                        reconcile_after: case.reconcile_after.map(Into::into),
                    })),
                }),
            }),
            machine_dispatch,
        );
    }
}
