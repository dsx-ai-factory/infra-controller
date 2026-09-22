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

//! Platform-specific lifecycle timing model for machine-a-tron.
//!
//! # Phased delivery
//!
//! - **Phase 1:** `LifecycleTimings` and `PlatformTimingProfile` structs; wired into
//!   `MachineStateMachine` so timer arms read a single resolved field.
//! - **Phase 2 (this file):** Per-[`HardwareType`] empirical defaults in
//!   `PlatformTimingProfile::for_hardware_type`.
//! - **Phase 3 (this file):** `PartialLifecycleTimings` + `LifecycleTimingOverrides` for
//!   per-group TOML overrides; `LifecycleTimings::with_overrides` and `LifecycleTimings::scale`
//!   for the three-layer resolution chain (platform defaults → group overrides →
//!   `acceleration_factor`).

use std::time::Duration;

use bmc_mock::HardwareType;
use serde::{Deserialize, Serialize};

/// Durations for all observable hardware lifecycle operations for one machine role
/// (host or DPU).
///
/// A `LifecycleTimings` value is resolved once at [`MachineStateMachine`] construction
/// time and stored on the state machine. Timer events (`SetTimer(MachineOn)`,
/// `SetTimer(PowerCycle)`) read from this struct rather than from flat config fields.
///
/// [`MachineStateMachine`]: crate::machine_state_machine::MachineStateMachine
#[derive(Debug, Clone, PartialEq)]
pub struct LifecycleTimings {
    /// Time from power signal to BMC DHCP / Redfish becoming available.
    pub power_on_bmc_ready: Duration,
    /// Time from BMC up to OS agent ready (covers PXE boot).
    pub power_on_os_ready: Duration,
    /// Graceful shutdown signal → host offline.
    pub power_off_graceful: Duration,
    /// Force power off → host offline.  Maps to the `SetTimer(PowerCycle)` arm.
    pub power_off_force: Duration,
    /// Full reboot cycle (off + on).  Maps to the `SetTimer(MachineOn)` arm.
    pub reboot: Duration,
    /// Hard reset — may be faster than a full reboot on some platforms.
    pub reset: Duration,
    /// Offset after `power_on_bmc_ready` before SSH console is available.
    pub bmc_ssh_ready_offset: Duration,
    /// Duration the BMC is unreachable after `Manager.Reset` / `ipmitool bmc reset cold`.
    /// Phase 4 will use this to drive the `BmcAvailability::Resetting` window in bmc-mock.
    pub bmc_reset: Duration,
}

impl LifecycleTimings {
    /// Return a new `LifecycleTimings` with every `Some` field in `overrides` replacing
    /// the corresponding value, leaving `None` fields unchanged.
    pub fn with_overrides(self, overrides: &PartialLifecycleTimings) -> Self {
        Self {
            power_on_bmc_ready: overrides
                .power_on_bmc_ready
                .unwrap_or(self.power_on_bmc_ready),
            power_on_os_ready: overrides
                .power_on_os_ready
                .unwrap_or(self.power_on_os_ready),
            power_off_graceful: overrides
                .power_off_graceful
                .unwrap_or(self.power_off_graceful),
            power_off_force: overrides.power_off_force.unwrap_or(self.power_off_force),
            reboot: overrides.reboot.unwrap_or(self.reboot),
            reset: overrides.reset.unwrap_or(self.reset),
            bmc_ssh_ready_offset: overrides
                .bmc_ssh_ready_offset
                .unwrap_or(self.bmc_ssh_ready_offset),
            bmc_reset: overrides.bmc_reset.unwrap_or(self.bmc_reset),
        }
    }

    /// Return a new `LifecycleTimings` with every duration multiplied by `factor`.
    ///
    /// `factor` is clamped to `[0.0, ∞)` — negative values and NaN become 0.
    /// Use `acceleration_factor = 0.05` to run 20× faster in CI.
    pub fn scale(self, factor: f64) -> Self {
        // clamp handles both negative and NaN (f64::NAN.max(0.0) == 0.0 in Rust)
        let f = factor.max(0.0);
        Self {
            power_on_bmc_ready: self.power_on_bmc_ready.mul_f64(f),
            power_on_os_ready: self.power_on_os_ready.mul_f64(f),
            power_off_graceful: self.power_off_graceful.mul_f64(f),
            power_off_force: self.power_off_force.mul_f64(f),
            reboot: self.reboot.mul_f64(f),
            reset: self.reset.mul_f64(f),
            bmc_ssh_ready_offset: self.bmc_ssh_ready_offset.mul_f64(f),
            bmc_reset: self.bmc_reset.mul_f64(f),
        }
    }
}

// ── Phase 3: partial overrides ────────────────────────────────────────────────

/// Serde helpers for `Option<Duration>` using the same "300s" / "500ms" string
/// format as the rest of the machine-a-tron config.
mod opt_duration_str {
    use std::time::Duration;

    use serde::{Deserialize, Deserializer, Serializer};

    pub(super) fn serialize<S>(d: &Option<Duration>, s: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        match d {
            None => s.serialize_none(),
            Some(d) => {
                if d.subsec_millis() > 0 || d.as_secs() == 0 {
                    s.serialize_str(&format!("{}ms", d.as_millis()))
                } else {
                    s.serialize_str(&format!("{}s", d.as_secs()))
                }
            }
        }
    }

    pub(super) fn deserialize<'de, D>(d: D) -> Result<Option<Duration>, D::Error>
    where
        D: Deserializer<'de>,
    {
        let s: Option<String> = Option::deserialize(d)?;
        match s {
            None => Ok(None),
            Some(s) => duration_str::parse(&s)
                .map(Some)
                .map_err(serde::de::Error::custom),
        }
    }
}

/// Partial override set for one machine role (host or DPU).
///
/// Every field is optional — only the fields present in TOML replace the
/// corresponding platform default.  Absent fields leave the platform default
/// unchanged.
///
/// ```toml
/// [machines.my-group.timing_overrides]
/// host.reboot = "300s"
/// dpu.power_on_bmc_ready = "45s"
/// ```
#[derive(Debug, Clone, Default, Deserialize, Serialize, PartialEq)]
pub struct PartialLifecycleTimings {
    #[serde(
        with = "opt_duration_str",
        default,
        skip_serializing_if = "Option::is_none"
    )]
    pub power_on_bmc_ready: Option<Duration>,
    #[serde(
        with = "opt_duration_str",
        default,
        skip_serializing_if = "Option::is_none"
    )]
    pub power_on_os_ready: Option<Duration>,
    #[serde(
        with = "opt_duration_str",
        default,
        skip_serializing_if = "Option::is_none"
    )]
    pub power_off_graceful: Option<Duration>,
    #[serde(
        with = "opt_duration_str",
        default,
        skip_serializing_if = "Option::is_none"
    )]
    pub power_off_force: Option<Duration>,
    #[serde(
        with = "opt_duration_str",
        default,
        skip_serializing_if = "Option::is_none"
    )]
    pub reboot: Option<Duration>,
    #[serde(
        with = "opt_duration_str",
        default,
        skip_serializing_if = "Option::is_none"
    )]
    pub reset: Option<Duration>,
    #[serde(
        with = "opt_duration_str",
        default,
        skip_serializing_if = "Option::is_none"
    )]
    pub bmc_ssh_ready_offset: Option<Duration>,
    #[serde(
        with = "opt_duration_str",
        default,
        skip_serializing_if = "Option::is_none"
    )]
    pub bmc_reset: Option<Duration>,
}

/// Per-group TOML timing overrides — one [`PartialLifecycleTimings`] for the host
/// role and one for the DPU role.  Absent fields default to the platform profile.
#[derive(Debug, Clone, Default, Deserialize, Serialize, PartialEq)]
pub struct LifecycleTimingOverrides {
    #[serde(default)]
    pub host: PartialLifecycleTimings,
    #[serde(default)]
    pub dpu: PartialLifecycleTimings,
}

// ── Phase 2: per-platform defaults ───────────────────────────────────────────

/// Per-platform lifecycle timing profile — one [`LifecycleTimings`] for the host role
/// and one for the DPU role.
///
/// Constructed via [`PlatformTimingProfile::for_hardware_type`] and resolved to a
/// single `LifecycleTimings` per machine at construction time.
#[derive(Debug, Clone)]
pub struct PlatformTimingProfile {
    /// Lifecycle timings applied to host (server) machines of this platform.
    pub host: LifecycleTimings,
    /// Lifecycle timings applied to DPU machines of this platform.
    pub dpu: LifecycleTimings,
}

impl PlatformTimingProfile {
    /// Return the timing profile for the given hardware type.
    ///
    /// Values are empirically tuned per platform.  All platforms share the same
    /// DPU timings (BF3/BF4 boot characteristics are similar across host platforms).
    /// Non-compute hardware (switches, power shelves) is given zero timings because
    /// it does not go through the compute lifecycle FSM.
    pub fn for_hardware_type(hw: &HardwareType) -> Self {
        // Must stay in sync with POWER_CYCLE_DELAY in bmc-mock/src/lib.rs (5 s).
        let power_off_force = Duration::from_secs(5);

        // DPU timings are shared across all host platforms — BF3/BF4 have similar
        // boot characteristics regardless of the server chassis they are installed in.
        let dpu = LifecycleTimings {
            power_on_bmc_ready: Duration::from_secs(60),
            power_on_os_ready: Duration::from_secs(120),
            power_off_graceful: Duration::from_secs(30),
            power_off_force,
            reboot: Duration::from_secs(180),
            reset: Duration::from_secs(180),
            bmc_ssh_ready_offset: Duration::from_secs(20),
            bmc_reset: Duration::from_secs(90),
        };

        let host = match hw {
            // GB200 NVL: large GPU chassis, complex OpenBMC, longer POST due to GPU init.
            HardwareType::WiwynnGB200Nvl => LifecycleTimings {
                power_on_bmc_ready: Duration::from_secs(180),
                power_on_os_ready: Duration::from_secs(420),
                power_off_graceful: Duration::from_secs(45),
                power_off_force,
                reboot: Duration::from_secs(600),
                reset: Duration::from_secs(600),
                bmc_ssh_ready_offset: Duration::from_secs(30),
                bmc_reset: Duration::from_secs(120),
            },

            // Dell iDRAC9 servers (R750 = BF3, R760 = BF4): standard boot cadence.
            HardwareType::DellPowerEdgeR750 | HardwareType::DellPowerEdgeR760Bf4 => {
                LifecycleTimings {
                    power_on_bmc_ready: Duration::from_secs(90),
                    power_on_os_ready: Duration::from_secs(300),
                    power_off_graceful: Duration::from_secs(30),
                    power_off_force,
                    reboot: Duration::from_secs(390),
                    reset: Duration::from_secs(390),
                    bmc_ssh_ready_offset: Duration::from_secs(30),
                    bmc_reset: Duration::from_secs(90),
                }
            }

            // Lenovo GB300 NVL: similar GPU complexity to WiwynnGB200.
            HardwareType::LenovoGB300Nvl => LifecycleTimings {
                power_on_bmc_ready: Duration::from_secs(150),
                power_on_os_ready: Duration::from_secs(360),
                power_off_graceful: Duration::from_secs(45),
                power_off_force,
                reboot: Duration::from_secs(510),
                reset: Duration::from_secs(510),
                bmc_ssh_ready_offset: Duration::from_secs(30),
                bmc_reset: Duration::from_secs(120),
            },

            // DGX GB300 / DGX VR: large NVIDIA GPU system, extended POST.
            HardwareType::NvidiaDgxGb300 | HardwareType::NvidiaDgxVr => LifecycleTimings {
                power_on_bmc_ready: Duration::from_secs(150),
                power_on_os_ready: Duration::from_secs(480),
                power_off_graceful: Duration::from_secs(45),
                power_off_force,
                reboot: Duration::from_secs(630),
                reset: Duration::from_secs(630),
                bmc_ssh_ready_offset: Duration::from_secs(30),
                bmc_reset: Duration::from_secs(120),
            },

            // DGX H100: NVIDIA GPU server, moderately long POST.
            HardwareType::NvidiaDgxH100 => LifecycleTimings {
                power_on_bmc_ready: Duration::from_secs(120),
                power_on_os_ready: Duration::from_secs(420),
                power_off_graceful: Duration::from_secs(45),
                power_off_force,
                reboot: Duration::from_secs(540),
                reset: Duration::from_secs(540),
                bmc_ssh_ready_offset: Duration::from_secs(30),
                bmc_reset: Duration::from_secs(120),
            },

            // Supermicro GB300 NVL and generic Supermicro.
            HardwareType::SupermicroGb300Nvl | HardwareType::GenericSupermicro => {
                LifecycleTimings {
                    power_on_bmc_ready: Duration::from_secs(120),
                    power_on_os_ready: Duration::from_secs(360),
                    power_off_graceful: Duration::from_secs(30),
                    power_off_force,
                    reboot: Duration::from_secs(480),
                    reset: Duration::from_secs(480),
                    bmc_ssh_ready_offset: Duration::from_secs(30),
                    bmc_reset: Duration::from_secs(90),
                }
            }

            // HPE iLO6: faster BMC reset than most platforms.
            HardwareType::HpeProliantDl380aGen11 => LifecycleTimings {
                power_on_bmc_ready: Duration::from_secs(90),
                power_on_os_ready: Duration::from_secs(300),
                power_off_graceful: Duration::from_secs(30),
                power_off_force,
                reboot: Duration::from_secs(390),
                reset: Duration::from_secs(390),
                bmc_ssh_ready_offset: Duration::from_secs(30),
                bmc_reset: Duration::from_secs(60),
            },

            // Generic AMI: moderate defaults for unknown AMI-BMC servers.
            HardwareType::GenericAmi => LifecycleTimings {
                power_on_bmc_ready: Duration::from_secs(120),
                power_on_os_ready: Duration::from_secs(300),
                power_off_graceful: Duration::from_secs(30),
                power_off_force,
                reboot: Duration::from_secs(420),
                reset: Duration::from_secs(420),
                bmc_ssh_ready_offset: Duration::from_secs(30),
                bmc_reset: Duration::from_secs(90),
            },

            // Non-compute hardware: switches and power shelves do not go through the
            // compute lifecycle FSM, so all durations are zero (except power_off_force
            // which stays at 5 s to match the bmc-mock constant).
            HardwareType::LiteOnPowerShelf
            | HardwareType::DeltaPowerShelf
            | HardwareType::NvidiaSwitchNd5200Ld
            | HardwareType::NvidiaSwitchN5700Ld => LifecycleTimings {
                power_on_bmc_ready: Duration::ZERO,
                power_on_os_ready: Duration::ZERO,
                power_off_graceful: Duration::ZERO,
                power_off_force,
                reboot: Duration::ZERO,
                reset: Duration::ZERO,
                bmc_ssh_ready_offset: Duration::ZERO,
                bmc_reset: Duration::ZERO,
            },
        };

        PlatformTimingProfile { host, dpu }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // ── Phase 2: platform defaults ────────────────────────────────────────────

    #[test]
    fn all_hardware_types_return_a_profile() {
        let types = [
            HardwareType::DellPowerEdgeR750,
            HardwareType::DellPowerEdgeR760Bf4,
            HardwareType::WiwynnGB200Nvl,
            HardwareType::LenovoGB300Nvl,
            HardwareType::NvidiaDgxGb300,
            HardwareType::NvidiaDgxH100,
            HardwareType::NvidiaDgxVr,
            HardwareType::SupermicroGb300Nvl,
            HardwareType::GenericAmi,
            HardwareType::GenericSupermicro,
            HardwareType::HpeProliantDl380aGen11,
            HardwareType::LiteOnPowerShelf,
            HardwareType::DeltaPowerShelf,
            HardwareType::NvidiaSwitchNd5200Ld,
            HardwareType::NvidiaSwitchN5700Ld,
        ];
        for hw in &types {
            // must not panic
            let _ = PlatformTimingProfile::for_hardware_type(hw);
        }
    }

    #[test]
    fn compute_platforms_have_nonzero_timings() {
        let compute = [
            HardwareType::DellPowerEdgeR750,
            HardwareType::DellPowerEdgeR760Bf4,
            HardwareType::WiwynnGB200Nvl,
            HardwareType::LenovoGB300Nvl,
            HardwareType::NvidiaDgxGb300,
            HardwareType::NvidiaDgxH100,
            HardwareType::SupermicroGb300Nvl,
            HardwareType::GenericAmi,
            HardwareType::HpeProliantDl380aGen11,
        ];
        for hw in &compute {
            let p = PlatformTimingProfile::for_hardware_type(hw);
            assert!(
                p.host.power_on_bmc_ready > Duration::ZERO,
                "{hw:?}: power_on_bmc_ready is zero"
            );
            assert!(
                p.host.power_on_os_ready > Duration::ZERO,
                "{hw:?}: power_on_os_ready is zero"
            );
            assert!(p.host.reboot > Duration::ZERO, "{hw:?}: reboot is zero");
            assert!(
                p.host.bmc_reset > Duration::ZERO,
                "{hw:?}: bmc_reset is zero"
            );
            assert!(p.dpu.reboot > Duration::ZERO, "{hw:?}: dpu reboot is zero");
        }
    }

    #[test]
    fn non_compute_platforms_have_zero_reboot() {
        let non_compute = [
            HardwareType::LiteOnPowerShelf,
            HardwareType::DeltaPowerShelf,
            HardwareType::NvidiaSwitchNd5200Ld,
            HardwareType::NvidiaSwitchN5700Ld,
        ];
        for hw in &non_compute {
            let p = PlatformTimingProfile::for_hardware_type(hw);
            assert_eq!(
                p.host.reboot,
                Duration::ZERO,
                "{hw:?}: reboot should be zero"
            );
            assert_eq!(
                p.host.power_on_bmc_ready,
                Duration::ZERO,
                "{hw:?}: power_on_bmc_ready should be zero"
            );
        }
    }

    #[test]
    fn power_off_force_matches_bmc_mock_constant() {
        // Must stay in sync with POWER_CYCLE_DELAY in bmc-mock/src/lib.rs (5 s).
        for hw in &[
            HardwareType::WiwynnGB200Nvl,
            HardwareType::DellPowerEdgeR750,
        ] {
            let p = PlatformTimingProfile::for_hardware_type(hw);
            assert_eq!(p.host.power_off_force, Duration::from_secs(5));
            assert_eq!(p.dpu.power_off_force, Duration::from_secs(5));
        }
    }

    #[test]
    fn wiwynn_gb200_has_longer_timings_than_dell() {
        let gb200 = PlatformTimingProfile::for_hardware_type(&HardwareType::WiwynnGB200Nvl);
        let dell = PlatformTimingProfile::for_hardware_type(&HardwareType::DellPowerEdgeR750);
        assert!(gb200.host.power_on_bmc_ready > dell.host.power_on_bmc_ready);
        assert!(gb200.host.power_on_os_ready > dell.host.power_on_os_ready);
        assert!(gb200.host.reboot > dell.host.reboot);
    }

    #[test]
    fn dpu_timings_are_same_across_host_platforms() {
        let hw_types = [
            HardwareType::WiwynnGB200Nvl,
            HardwareType::DellPowerEdgeR750,
            HardwareType::LenovoGB300Nvl,
            HardwareType::NvidiaDgxGb300,
        ];
        let reference = PlatformTimingProfile::for_hardware_type(&hw_types[0]).dpu;
        for hw in &hw_types[1..] {
            let p = PlatformTimingProfile::for_hardware_type(hw);
            assert_eq!(
                p.dpu, reference,
                "{hw:?}: DPU timings differ from reference"
            );
        }
    }

    // ── Phase 3: with_overrides ───────────────────────────────────────────────

    #[test]
    fn with_overrides_replaces_only_specified_fields() {
        let base = PlatformTimingProfile::for_hardware_type(&HardwareType::WiwynnGB200Nvl).host;
        let original_os_ready = base.power_on_os_ready;

        let overrides = PartialLifecycleTimings {
            reboot: Some(Duration::from_secs(300)),
            ..Default::default()
        };
        let result = base.with_overrides(&overrides);

        assert_eq!(
            result.reboot,
            Duration::from_secs(300),
            "override should apply"
        );
        assert_eq!(
            result.power_on_os_ready, original_os_ready,
            "unset field should be unchanged"
        );
    }

    #[test]
    fn with_overrides_empty_is_identity() {
        let base = PlatformTimingProfile::for_hardware_type(&HardwareType::DellPowerEdgeR750).host;
        let result = base
            .clone()
            .with_overrides(&PartialLifecycleTimings::default());
        assert_eq!(result, base);
    }

    #[test]
    fn with_overrides_all_fields() {
        let base = PlatformTimingProfile::for_hardware_type(&HardwareType::GenericAmi).host;
        let overrides = PartialLifecycleTimings {
            power_on_bmc_ready: Some(Duration::from_secs(1)),
            power_on_os_ready: Some(Duration::from_secs(2)),
            power_off_graceful: Some(Duration::from_secs(3)),
            power_off_force: Some(Duration::from_secs(4)),
            reboot: Some(Duration::from_secs(5)),
            reset: Some(Duration::from_secs(6)),
            bmc_ssh_ready_offset: Some(Duration::from_secs(7)),
            bmc_reset: Some(Duration::from_secs(8)),
        };
        let result = base.with_overrides(&overrides);
        assert_eq!(result.power_on_bmc_ready, Duration::from_secs(1));
        assert_eq!(result.power_on_os_ready, Duration::from_secs(2));
        assert_eq!(result.power_off_graceful, Duration::from_secs(3));
        assert_eq!(result.power_off_force, Duration::from_secs(4));
        assert_eq!(result.reboot, Duration::from_secs(5));
        assert_eq!(result.reset, Duration::from_secs(6));
        assert_eq!(result.bmc_ssh_ready_offset, Duration::from_secs(7));
        assert_eq!(result.bmc_reset, Duration::from_secs(8));
    }

    // ── Phase 3: scale ────────────────────────────────────────────────────────

    #[test]
    fn scale_by_one_is_identity() {
        let base = PlatformTimingProfile::for_hardware_type(&HardwareType::WiwynnGB200Nvl).host;
        let scaled = base.clone().scale(1.0);
        assert_eq!(scaled, base);
    }

    #[test]
    fn scale_by_zero_gives_zero_durations() {
        let base = PlatformTimingProfile::for_hardware_type(&HardwareType::WiwynnGB200Nvl).host;
        let scaled = base.scale(0.0);
        assert_eq!(scaled.reboot, Duration::ZERO);
        assert_eq!(scaled.power_on_bmc_ready, Duration::ZERO);
        assert_eq!(scaled.bmc_reset, Duration::ZERO);
    }

    #[test]
    fn scale_halves_all_durations() {
        let base = PlatformTimingProfile::for_hardware_type(&HardwareType::DellPowerEdgeR750).host;
        let scaled = base.clone().scale(0.5);
        assert_eq!(scaled.reboot, base.reboot / 2);
        assert_eq!(scaled.power_on_bmc_ready, base.power_on_bmc_ready / 2);
    }

    #[test]
    fn scale_clamps_negative_to_zero() {
        let base = PlatformTimingProfile::for_hardware_type(&HardwareType::GenericAmi).host;
        let scaled = base.scale(-1.0);
        assert_eq!(scaled.reboot, Duration::ZERO);
    }

    #[test]
    fn scale_nan_treated_as_zero() {
        let base = PlatformTimingProfile::for_hardware_type(&HardwareType::GenericAmi).host;
        let scaled = base.scale(f64::NAN);
        assert_eq!(scaled.reboot, Duration::ZERO);
    }

    // ── Phase 3: resolution chain ─────────────────────────────────────────────

    #[test]
    fn resolution_chain_overrides_then_scale() {
        let profile = PlatformTimingProfile::for_hardware_type(&HardwareType::WiwynnGB200Nvl);
        let overrides = PartialLifecycleTimings {
            reboot: Some(Duration::from_secs(400)),
            ..Default::default()
        };
        let result = profile.host.with_overrides(&overrides).scale(0.5);
        // override takes effect first, then scale
        assert_eq!(result.reboot, Duration::from_secs(200));
    }

    // ── Phase 3: serde round-trip ─────────────────────────────────────────────

    #[test]
    fn lifecycle_timing_overrides_toml_round_trip() {
        let overrides = LifecycleTimingOverrides {
            host: PartialLifecycleTimings {
                reboot: Some(Duration::from_secs(300)),
                power_on_bmc_ready: Some(Duration::from_secs(45)),
                ..Default::default()
            },
            dpu: PartialLifecycleTimings {
                bmc_reset: Some(Duration::from_secs(60)),
                ..Default::default()
            },
        };
        let toml_str = toml::to_string(&overrides).expect("serializes");
        let round_tripped: LifecycleTimingOverrides =
            toml::from_str(&toml_str).expect("deserializes");
        assert_eq!(round_tripped, overrides);
    }

    #[test]
    fn empty_overrides_toml_round_trip() {
        let overrides = LifecycleTimingOverrides::default();
        let toml_str = toml::to_string(&overrides).expect("serializes");
        let round_tripped: LifecycleTimingOverrides =
            toml::from_str(&toml_str).expect("deserializes");
        assert_eq!(round_tripped, overrides);
    }
}
