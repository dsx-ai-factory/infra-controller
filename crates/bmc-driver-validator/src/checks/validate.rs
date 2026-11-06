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

//! The `--validate` sequence: every capability the selection supports, in
//! the order NICo takes a BMC from factory state to ready, with a restart
//! wherever a change takes effect on one. [`host`] and [`dpu`] hold each
//! kind of BMC's setup, and [`factory`] the steps back to factory state.

mod dpu;
mod factory;
mod host;

use std::time::{Duration, SystemTime};

use bmc_platform::{Capability, PlatformError, TimeStatus};
use nv_redfish::account::AccountServiceConfig;
use serde_json::Value;

pub(super) use self::factory::factory_reset_first;
use super::context::CheckCx;
use super::errors::{describe, failed};
use super::model::{Check, CheckId, Checked, Detail, Needs, Step};
use super::ops::{self, Attempt, Effect, SETTLE_POLL_INTERVAL, how, poll, requested};
use super::text::reported;
use super::{apply, reads};

/// What `--validate` leaves the BMC in.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq, clap::ValueEnum)]
pub(crate) enum EndState {
    /// Configured and locked down, as NICo leaves it.
    #[default]
    NicoReady,
    /// Reset to factory defaults, with the factory password.
    Factory,
}

/// How far the BMC's clock may be from this machine's once NTP syncs it, and
/// how long NTP may take to bring it there.
const CLOCK_TOLERANCE: Duration = Duration::from_secs(5 * 60);
const CLOCK_TIMEOUT: Duration = Duration::from_secs(10 * 60);

const ACCOUNT_POLICY: CheckId = CheckId::new("accounts.policy");
const BMC_RESET: CheckId = CheckId::new("bmc_control.reset");
const NTP: CheckId = CheckId::new("bmc_control.ntp");
const UTC: CheckId = CheckId::new("bmc_control.utc");
const UEFI_PASSWORD: CheckId = CheckId::new("bios.uefi_password");

pub(super) fn checks(dpu: bool, end_state: EndState) -> Vec<Check> {
    let mut checks = vec![
        Check::apply(ACCOUNT_POLICY, Capability::Accounts, run!(account_policy)),
        apply::accounts_lifecycle(),
        Check::apply(BMC_RESET, Capability::BmcControl, run!(ops::reset_bmc)).once(),
        Check::apply(NTP, Capability::BmcControl, run!(ntp))
            .needs(&[Needs::NtpServers])
            .after(&[BMC_RESET]),
        Check::apply(UTC, Capability::BmcControl, run!(utc)).after(&[BMC_RESET]),
    ];
    checks.extend(if dpu { dpu::setup() } else { host::setup() });
    checks.extend(reads::checks());
    if end_state == EndState::Factory {
        checks.extend(factory::reset());
    }
    checks
}

/// Every step of the `--validate` sequences, once each.
pub(super) fn steps() -> Vec<Check> {
    let mut steps: Vec<Check> = Vec::new();
    for check in checks(false, EndState::Factory)
        .into_iter()
        .chain(checks(true, EndState::Factory))
        .chain(factory_reset_first())
    {
        if !steps.iter().any(|step| step.id == check.id) {
            steps.push(check);
        }
    }
    steps
}

fn uefi_password() -> Check {
    Check::apply(UEFI_PASSWORD, Capability::Bios, run!(set_uefi_password))
        .once()
        .needs(&[Needs::UefiPassword])
}

async fn account_policy(cx: &CheckCx<'_>) -> Checked {
    let target = cx.target();
    let outcome = requested(
        cx,
        "apply_default_policy",
        cx.bmc()
            .accounts()?
            .apply_default_policy(target.username(), target.password()),
    )
    .await?;
    ops::wait_for_operations(cx, &outcome).await?;
    let service = async {
        cx.target()
            .fresh_connection()
            .await?
            .service_root
            .account_service(AccountServiceConfig::standard())
            .await
            .map_err(PlatformError::from_redfish)?
            .ok_or_else(|| PlatformError::InvalidResponse {
                message: "the BMC lists no AccountService".to_string(),
            })
    }
    .await
    .map_err(failed("reading AccountService"))?;
    let raw = service.raw();
    let standard = [
        (
            "lockout threshold",
            raw.account_lockout_threshold
                .flatten()
                .map(|v| v.to_string()),
        ),
        (
            "lockout duration",
            raw.account_lockout_duration
                .flatten()
                .map(|v| v.to_string()),
        ),
        (
            "counter reset after",
            raw.account_lockout_counter_reset_after
                .map(|v| v.to_string()),
        ),
    ];
    let mut fields: Vec<_> = standard
        .iter()
        .filter_map(|(name, value)| Some((name.to_string(), value.clone()?)))
        .collect();
    if let Some(oem) = &raw.oem {
        fields.extend(vendor_settings(&oem.additional_properties));
    }
    if fields.is_empty() {
        fields = standard
            .iter()
            .map(|(name, value)| (name.to_string(), reported(value)))
            .collect();
    }
    Ok(Detail {
        summary: format!("applied ({}); AccountService now reads", how(&outcome)),
        fields,
    })
}

/// The number and boolean settings `oem` has under each vendor, as
/// `Hpe.AuthFailuresBeforeDelay`: where a vendor such as HPE keeps the
/// account policy instead of the standard lockout properties.
fn vendor_settings(oem: &Value) -> Vec<(String, String)> {
    oem.as_object()
        .into_iter()
        .flatten()
        .filter_map(|(vendor, settings)| Some((vendor, settings.as_object()?)))
        .flat_map(|(vendor, settings)| {
            settings
                .iter()
                .filter(|(_, value)| value.is_number() || value.is_boolean())
                .map(move |(name, value)| (format!("{vendor}.{name}"), value.to_string()))
        })
        .collect()
}

/// Sets the NTP servers and waits until they are listed and the clock is
/// within [`CLOCK_TOLERANCE`] of this machine's.
async fn ntp(cx: &CheckCx<'_>) -> Step {
    let servers = &cx.inputs().ntp_servers;
    let bmc = cx.bmc();
    let control = bmc.bmc_control()?;
    let outcome = requested(cx, "set_ntp_servers", control.set_ntp_servers(servers)).await?;
    let listed = ops::read_back(cx, &outcome, Effect::Immediate, async || {
        match control.time_status().await {
            Ok(status) if lists_ntp_servers(&status, servers) => Attempt::Done(status.ntp_servers),
            Ok(status) => Attempt::NotYet(format!(
                "NTP is enabled: {}; it lists {}",
                reported(&status.ntp_enabled),
                status.ntp_servers.join(", ")
            )),
            Err(error) => Attempt::NotYet(describe("time_status", &error)),
        }
    })
    .await?;
    let skew = poll(
        cx,
        CLOCK_TIMEOUT,
        SETTLE_POLL_INTERVAL,
        async || match control.time_status().await.map(|status| status.now) {
            Ok(None) => Attempt::Done(None),
            Ok(Some(now)) => {
                let skew = now
                    .duration_since(SystemTime::now())
                    .unwrap_or_else(|behind| behind.duration());
                if skew <= CLOCK_TOLERANCE {
                    Attempt::Done(Some(skew))
                } else {
                    Attempt::NotYet(format!("the BMC clock is {}s off", skew.as_secs()))
                }
            }
            Err(error) => Attempt::NotYet(describe("time_status", &error)),
        },
    )
    .await?;
    Ok(format!(
        "lists {} ({}); {}",
        listed.join(", "),
        how(&outcome),
        match skew {
            Some(skew) => format!("the clock is within {}s of this machine's", skew.as_secs()),
            None => "the BMC reports no clock".to_string(),
        }
    ))
}

/// Whether the BMC lists the servers in `wanted` it has room for: as many as
/// it lists entries, from the front, with NTP not switched off.
fn lists_ntp_servers(status: &TimeStatus, wanted: &[String]) -> bool {
    let kept = status.ntp_servers.len().min(wanted.len());
    status.ntp_enabled != Some(false)
        && kept > 0
        && wanted[..kept]
            .iter()
            .all(|server| status.ntp_servers.contains(server))
}

async fn utc(cx: &CheckCx<'_>) -> Step {
    const UTC_OFFSETS: &[&str] = &["+00:00", "-00:00", "Z"];
    let bmc = cx.bmc();
    let control = bmc.bmc_control()?;
    let outcome = requested(cx, "set_utc_timezone", control.set_utc_timezone()).await?;
    let offset = ops::read_back(cx, &outcome, Effect::Immediate, async || {
        match control.time_status().await.map(|status| status.utc_offset) {
            Ok(None) => Attempt::Done(None),
            Ok(Some(offset)) if UTC_OFFSETS.contains(&offset.as_str()) => {
                Attempt::Done(Some(offset))
            }
            Ok(Some(offset)) => Attempt::NotYet(format!("the UTC offset is {offset}")),
            Err(error) => Attempt::NotYet(describe("time_status", &error)),
        }
    })
    .await?;
    Ok(format!(
        "{} ({})",
        match offset {
            Some(offset) => format!("the UTC offset is {offset}"),
            None => "the BMC reports no UTC offset".to_string(),
        },
        how(&outcome)
    ))
}

/// Sets the UEFI password and restarts the host so it applies. The password
/// cannot be read back, so an accepted request is taken as set.
async fn set_uefi_password(cx: &CheckCx<'_>) -> Step {
    let password = cx.inputs().required_uefi_password()?;
    let outcome = requested(
        cx,
        "change_uefi_password",
        cx.bmc()
            .bios()?
            .change_uefi_password(&password.current, &password.new),
    )
    .await?;
    ops::take_effect(cx, &outcome).await?;
    Ok(format!("set ({}) and restarted", how(&outcome)))
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    /// A BMC with fewer slots than servers given keeps the first ones; one
    /// still listing its placeholders has taken none.
    #[test]
    fn ntp_servers_are_listed_up_to_the_slots_the_bmc_has() {
        let list = |entries: &[&str]| entries.iter().map(ToString::to_string).collect::<Vec<_>>();
        let wanted = list(&["10.0.0.1", "10.0.0.2", "10.0.0.3"]);
        let cases = [
            (
                "every server listed",
                list(&["10.0.0.1", "10.0.0.2", "10.0.0.3"]),
                Some(true),
                true,
            ),
            (
                "two slots, first two kept",
                list(&["10.0.0.1", "10.0.0.2"]),
                None,
                true,
            ),
            (
                "factory placeholders",
                list(&["localhost", "127.0.0.1"]),
                Some(false),
                false,
            ),
            (
                "listed but disabled",
                list(&["10.0.0.1", "10.0.0.2"]),
                Some(false),
                false,
            ),
            ("nothing listed", list(&[]), Some(true), false),
        ];
        for (scenario, ntp_servers, ntp_enabled, listed) in cases {
            let status = TimeStatus {
                ntp_enabled,
                ntp_servers,
                ..TimeStatus::default()
            };
            assert_eq!(lists_ntp_servers(&status, &wanted), listed, "{scenario}");
        }
    }

    /// HPE keeps the account policy under `Oem.Hpe`; its numbers and
    /// booleans are read back, and text and nested objects are left out.
    #[test]
    fn vendor_account_settings_are_its_numbers_and_booleans() {
        let oem = json!({"Hpe": {
            "AuthFailuresBeforeDelay": 0,
            "EnforcePasswordComplexity": false,
            "DefaultUserName": "Administrator",
            "DirectorySettings": {"Enabled": true},
        }});
        assert_eq!(
            vendor_settings(&oem),
            [
                ("Hpe.AuthFailuresBeforeDelay".to_string(), "0".to_string()),
                (
                    "Hpe.EnforcePasswordComplexity".to_string(),
                    "false".to_string()
                ),
            ]
        );
    }

    /// Lockdown is the last host setup step and follows every other, so a
    /// step added later cannot run against a locked-down BMC.
    #[test]
    fn lockdown_follows_every_host_setup_step() {
        let setup = host::setup();
        let (lockdown, steps) = setup.split_last().expect("host setup has steps");
        assert_eq!(lockdown.id.as_str(), "lockdown.enable");
        for step in steps {
            assert!(lockdown.after.contains(&step.id), "{}", step.id);
        }
    }

    /// Gating looks dependencies up by id: one on a later, absent, or
    /// repeated step would gate nothing or the wrong step.
    #[test]
    fn every_dependency_names_one_earlier_step() {
        let sequences = [
            checks(false, EndState::Factory),
            checks(true, EndState::Factory),
            factory_reset_first(),
        ];
        for checks in sequences {
            for (index, check) in checks.iter().enumerate() {
                for dependency in &check.after {
                    let runs = |checks: &[Check]| {
                        checks
                            .iter()
                            .filter(|other| other.id == *dependency)
                            .count()
                    };
                    assert!(
                        runs(&checks[..index]) == 1 && runs(&checks) == 1,
                        "{} follows {dependency}, which must run once, before it",
                        check.id
                    );
                }
            }
        }
    }
}
