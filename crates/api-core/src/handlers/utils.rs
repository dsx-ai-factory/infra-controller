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

use std::borrow::Cow;
use std::future::Future;
use std::net::{IpAddr, SocketAddr};
use std::time::Duration;

use carbide_utils::HostPortPair;
use carbide_uuid::machine::{HostMachineId, MachineId, MachineIdSubtypeTrait};
use tokio::net::lookup_host;

use crate::CarbideError;
use crate::api::{Api, log_machine_id};

const DEFAULT_BMC_HTTPS_PORT: u16 = 443;

// Match the default Forge RPC client budget without sharing its transport policy.
const MACHINE_PREPARATION_TIMEOUT: Duration = Duration::from_secs(300);

/// `MachinePreparationDeadline` bounds a handler's Machine database preparation.
/// Construct it at handler entry, after the listener's authentication and
/// configured authorization and gRPC admission middleware. The `grpc-timeout`
/// budget starts here and excludes time in those earlier layers. It accepts one
/// to eight decimal digits followed by `H` (hours), `M` (minutes), `S` (seconds),
/// `m` (milliseconds), `u` (microseconds), or `n` (nanoseconds); zero is valid.
/// A valid shorter timeout reduces the five-minute cap, while missing or malformed
/// metadata keeps it. This bounds only the enclosed database preparation,
/// including pool acquisition and retries.
pub(super) struct MachinePreparationDeadline {
    deadline: tokio::time::Instant,
}

impl MachinePreparationDeadline {
    pub(super) fn from_request<T>(request: &tonic::Request<T>) -> Self {
        let started_at = tokio::time::Instant::now();
        let budget = request
            .metadata()
            .get("grpc-timeout")
            .and_then(|value| value.to_str().ok())
            .and_then(grpc_timeout_duration)
            .unwrap_or(MACHINE_PREPARATION_TIMEOUT)
            .min(MACHINE_PREPARATION_TIMEOUT);
        Self {
            deadline: started_at + budget,
        }
    }

    /// `run` constructs the transaction-owning future only while time remains.
    /// Expiry drops it; SQLx queues an open transaction's rollback. This does not
    /// guarantee immediate PostgreSQL cancellation or resolve an in-flight commit.
    pub(super) async fn run<T, MakeWork, Work>(
        self,
        make_work: MakeWork,
    ) -> Result<T, tonic::Status>
    where
        MakeWork: FnOnce() -> Work,
        Work: Future<Output = Result<T, tonic::Status>>,
    {
        let expired = || tonic::Status::from(CarbideError::MachinePreparationDeadlineExceeded);
        if tokio::time::Instant::now() >= self.deadline {
            return Err(expired());
        }
        tokio::select! {
            biased;
            _ = tokio::time::sleep_until(self.deadline) => Err(expired()),
            result = async move { make_work().await } => result,
        }
    }
}

fn grpc_timeout_duration(value: &str) -> Option<Duration> {
    let (&unit, digits) = value.as_bytes().split_last()?;
    if digits.is_empty() || digits.len() > 8 || !digits.iter().all(u8::is_ascii_digit) {
        return None;
    }
    let count = std::str::from_utf8(digits).ok()?.parse::<u64>().ok()?;
    Some(match unit {
        b'H' => Duration::from_secs(count * 3600),
        b'M' => Duration::from_secs(count * 60),
        b'S' => Duration::from_secs(count),
        b'm' => Duration::from_millis(count),
        b'u' => Duration::from_micros(count),
        b'n' => Duration::from_nanos(count),
        _ => return None,
    })
}

/// `resolve_bmc_address` selects the first address from [`resolve_bmc_addresses`].
pub(super) async fn resolve_bmc_address(address: &str) -> Result<SocketAddr, tonic::Status> {
    resolve_bmc_addresses(address)
        .await?
        .into_iter()
        .next()
        .ok_or_else(|| invalid_bmc_address(address, "name resolution returned no addresses").into())
}

/// `resolve_bmc_addresses` returns all BMC addresses, defaulting to HTTPS port 443.
/// Successful resolution always returns at least one address.
///
/// Bare IP literals are handled before hostname resolution so an IPv6 address's
/// colons are never mistaken for a host/port separator. An explicit port on an
/// IPv6 literal must use the standard `[address]:port` socket syntax.
pub(super) async fn resolve_bmc_addresses(address: &str) -> Result<Vec<SocketAddr>, tonic::Status> {
    if let Some(address) = parse_numeric_bmc_address(address) {
        return Ok(vec![address]);
    }

    let lookup_target = bmc_lookup_target(address).map_err(tonic::Status::from)?;
    let addresses: Vec<_> = lookup_host(lookup_target.as_ref()).await?.collect();
    if addresses.is_empty() {
        return Err(invalid_bmc_address(address, "name resolution returned no addresses").into());
    }

    Ok(addresses)
}

fn parse_numeric_bmc_address(address: &str) -> Option<SocketAddr> {
    if let Ok(address) = address.parse::<SocketAddr>() {
        return Some(address);
    }
    if let Ok(address) = address.parse::<IpAddr>() {
        return Some(SocketAddr::new(address, DEFAULT_BMC_HTTPS_PORT));
    }

    let address = address.strip_prefix('[')?.strip_suffix(']')?;
    let address = address.parse::<IpAddr>().ok()?;
    address
        .is_ipv6()
        .then(|| SocketAddr::new(address, DEFAULT_BMC_HTTPS_PORT))
}

fn bmc_lookup_target(address: &str) -> Result<Cow<'_, str>, CarbideError> {
    if address.contains('[') || address.contains(']') {
        return Err(invalid_bmc_address(
            address,
            "brackets are only valid around an IPv6 literal",
        ));
    }

    match address
        .parse::<HostPortPair>()
        .map_err(|error| invalid_bmc_address(address, error))?
    {
        HostPortPair::HostOnly(_) => Ok(Cow::Owned(format!("{address}:{DEFAULT_BMC_HTTPS_PORT}"))),
        HostPortPair::HostAndPort(_, _) => Ok(Cow::Borrowed(address)),
        HostPortPair::PortOnly(_) => Err(invalid_bmc_address(address, "host is missing")),
    }
}

fn invalid_bmc_address(address: &str, reason: impl std::fmt::Display) -> CarbideError {
    CarbideError::InvalidArgument(format!(
        "could not resolve BMC address {address:?}: {reason}; expected a hostname or IP address with an optional port (IPv6 with an explicit port must use [address]:port)"
    ))
}

/// Converts a MachineID from RPC format to Model format
/// and logs the MachineID as MachineID for the current request.
pub(super) fn convert_and_log_machine_id<FromId, ToId>(
    id: Option<&FromId>,
) -> Result<ToId, CarbideError>
where
    FromId: Copy,
    MachineId: From<FromId>,
    ToId: MachineIdSubtypeTrait,
    ToId: TryFrom<MachineId>,
    CarbideError: From<<ToId as TryFrom<MachineId>>::Error>,
{
    let machine_id = match id {
        Some(id) => ToId::try_from(MachineId::from(*id))?,
        None => {
            return Err(CarbideError::MissingArgument("machine ID"));
        }
    };
    log_machine_id(machine_id.as_machine_id());

    Ok(machine_id)
}

/// The recorded change whose processing tried to wake the machine's state
/// handler.
#[derive(Debug, Clone, Copy, PartialEq, Eq, carbide_instrument::LabelValue)]
pub(super) enum WakeupTrigger {
    RebootCompleted,
    CleanupCompleted,
    ScoutFirmwareUpgradeStatus,
    DpuNetworkStatus,
    BootInterfaceIntent,
    NicFirmwareObservation,
}

/// A change was recorded but the machine's state handler could not be woken:
/// the machine sits idle until the next periodic enqueue, so the rate of these
/// is a leading "machine stuck" signal.
#[derive(carbide_instrument::Event)]
#[event(
    event_name = "state_handler_wakeup_failed",
    metric_name = "carbide_state_handler_wakeup_failures_total",
    component = "nico-api",
    log = warn,
    metric = counter,
    message = "Failed to wake up state handler for machine",
    describe = "Number of times a machine's state handler could not be woken after an \
                observed or desired state change"
)]
pub(super) struct StateHandlerWakeupFailed {
    #[label]
    pub(super) trigger: WakeupTrigger,
    #[context]
    pub(super) machine_id: MachineId,
    #[context]
    pub(super) err: String,
}

/// Enqueues a machine after its desired boot interface changes.
///
/// The database remains the durable source if the enqueue fails: the periodic
/// state-controller scan will try again, while this helper records the delay
/// without failing an otherwise successful API request.
pub(super) async fn enqueue_boot_interface_reconciliation(
    api: &Api,
    machine_id: HostMachineId,
    eligible: bool,
) {
    if !eligible {
        return;
    }

    if let Err(err) = api
        .machine_state_handler_enqueuer
        .enqueue_object(&machine_id)
        .await
    {
        carbide_instrument::emit(StateHandlerWakeupFailed {
            trigger: WakeupTrigger::BootInterfaceIntent,
            machine_id: machine_id.into(),
            err: err.to_string(),
        });
    }
}

#[cfg(test)]
mod tests {
    use std::str::FromStr as _;

    use carbide_instrument::testing::{MetricsCapture, capture_logs};
    use carbide_test_support::value_scenarios;

    use super::*;

    #[tokio::test(start_paused = true)]
    async fn machine_preparation_deadline_uses_the_finite_caller_budget() {
        value_scenarios!(
            run = |header: Option<&str>| {
                let mut request = tonic::Request::new(());
                if let Some(header) = header {
                    request.metadata_mut().insert("grpc-timeout", header.parse().unwrap());
                }
                MachinePreparationDeadline::from_request(&request)
                    .deadline.duration_since(tokio::time::Instant::now())
            };
            "finite fallback and cap" {
                None => Duration::from_secs(300),
                Some("600S") => Duration::from_secs(300),
                Some("99999999H") => Duration::from_secs(300),
            }
            "gRPC timeout units" {
                Some("1H") => Duration::from_secs(300),
                Some("1M") => Duration::from_secs(60),
                Some("1S") => Duration::from_secs(1),
                Some("1m") => Duration::from_millis(1),
                Some("1u") => Duration::from_micros(1),
                Some("1n") => Duration::from_nanos(1),
                Some("99999999n") => Duration::from_nanos(99_999_999),
                Some("0n") => Duration::ZERO,
            }
            "malformed metadata keeps the finite cap" {
                Some("S") => Duration::from_secs(300),
                Some("123456789S") => Duration::from_secs(300),
                Some("+1S") => Duration::from_secs(300),
                Some("1s") => Duration::from_secs(300),
            }
        );
    }

    #[tokio::test(start_paused = true)]
    async fn expired_machine_preparation_does_not_construct_ready_work() {
        for (scenario, budget) in [
            ("zero", Duration::ZERO),
            ("expired", Duration::from_secs(1)),
        ] {
            let mut request = tonic::Request::new(());
            request.set_timeout(budget);
            let deadline = MachinePreparationDeadline::from_request(&request);
            tokio::time::advance(budget).await;
            let constructed = std::cell::Cell::new(false);
            let error = deadline
                .run(|| {
                    constructed.set(true);
                    std::future::ready(Ok::<_, tonic::Status>(()))
                })
                .await
                .unwrap_err();
            assert_eq!(error.code(), tonic::Code::DeadlineExceeded, "{scenario}");
            assert!(
                !constructed.get(),
                "{scenario}: expired work was constructed"
            );
        }
    }

    #[tokio::test(start_paused = true)]
    async fn machine_preparation_expiry_precedes_an_already_ready_result() {
        let mut request = tonic::Request::new(());
        request.set_timeout(Duration::from_secs(1));
        let deadline = MachinePreparationDeadline::from_request(&request);
        let (ready, result) = tokio::sync::oneshot::channel();
        let work = deadline.run(|| async move {
            result.await.unwrap();
            Ok(())
        });
        tokio::pin!(work);
        assert!(futures_util::poll!(&mut work).is_pending());
        tokio::time::advance(Duration::from_secs(1) + Duration::from_millis(1)).await;
        ready.send(()).unwrap();
        let error = work.await.unwrap_err();
        assert_eq!(error.code(), tonic::Code::DeadlineExceeded);
        let message = "machine database preparation deadline exceeded";
        assert_eq!(error.message(), message);
        assert_eq!(
            error.metadata().get("nico-error-code").unwrap(),
            "NICO-API-504"
        );
        assert_eq!(error.metadata().get("nico-error-text").unwrap(), message);
        let source = std::error::Error::source(&error)
            .unwrap()
            .downcast_ref::<crate::errors::CarbideErrorWithLocation>()
            .unwrap();
        assert!(matches!(
            &source.error,
            CarbideError::MachinePreparationDeadlineExceeded
        ));
        assert!(source.location.starts_with(concat!(file!(), ":")));
    }

    #[test]
    fn parses_numeric_bmc_addresses_with_the_expected_port() {
        value_scenarios!(
            run = |address: &str| parse_numeric_bmc_address(address);
            "default HTTPS port" {
                "192.0.2.10" => Some("192.0.2.10:443".parse().unwrap()),
                "2001:db8::10" => Some("[2001:db8::10]:443".parse().unwrap()),
                "[2001:db8::10]" => Some("[2001:db8::10]:443".parse().unwrap()),
            }

            "explicit port" {
                "192.0.2.10:8443" => Some("192.0.2.10:8443".parse().unwrap()),
                "[2001:db8::10]:8443" => Some("[2001:db8::10]:8443".parse().unwrap()),
            }

            "hostname resolution required" {
                "bmc.example.com" => None,
                "bmc.example.com:8443" => None,
            }
        );
    }

    #[test]
    fn hostname_lookup_targets_preserve_or_default_the_port() {
        value_scenarios!(
            run = |address: &str| bmc_lookup_target(address).ok().map(Cow::into_owned);
            "default HTTPS port" {
                "bmc.example.com" => Some("bmc.example.com:443".to_string()),
            }

            "explicit port" {
                "bmc.example.com:8443" => Some("bmc.example.com:8443".to_string()),
            }

            "invalid host and port forms" {
                ":8443" => None,
                "bmc.example.com:notaport" => None,
                "[192.0.2.10]" => None,
                "[192.0.2.10]:8443" => None,
            }
        );
    }

    #[tokio::test]
    async fn resolves_hostname_forms_with_the_expected_port() {
        for (scenario, address, expected_port) in [
            ("hostname", "localhost", 443),
            ("hostname and port", "localhost:8443", 8443),
        ] {
            let resolved = resolve_bmc_address(address)
                .await
                .expect("localhost should resolve");
            assert!(resolved.ip().is_loopback(), "{scenario}");
            assert_eq!(resolved.port(), expected_port, "{scenario}");
            let all_resolved = resolve_bmc_addresses(address).await.unwrap();
            let expected: Vec<_> = lookup_host(("localhost", expected_port))
                .await
                .unwrap()
                .collect();
            assert_eq!(all_resolved, expected, "{scenario}");
            assert_eq!(all_resolved.first(), Some(&resolved), "{scenario}");
        }
    }

    #[tokio::test]
    async fn invalid_bmc_address_error_describes_accepted_forms() {
        let error = resolve_bmc_address(":8443")
            .await
            .expect_err("an address without a host must be rejected");

        assert_eq!(error.code(), tonic::Code::InvalidArgument);
        assert_eq!(
            error.message(),
            "could not resolve BMC address \":8443\": host is missing; expected a hostname or IP address with an optional port (IPv6 with an explicit port must use [address]:port)"
        );
    }

    /// One emit writes the WARN line (machine id and error as fields) AND
    /// moves the counter, with every trigger variant rendering as its
    /// snake_case label value on both sides.
    #[test]
    fn wakeup_failure_logs_and_counts_by_trigger() {
        let machine_id =
            MachineId::from_str("fm100htes3rn1npvbtm5qd57dkilaag7ljugl1llmm7rfuq1ov50i0rpl30")
                .expect("a valid machine id");

        let metrics = MetricsCapture::start();
        let logs = capture_logs(|| {
            for trigger in [
                WakeupTrigger::RebootCompleted,
                WakeupTrigger::CleanupCompleted,
                WakeupTrigger::ScoutFirmwareUpgradeStatus,
                WakeupTrigger::DpuNetworkStatus,
                WakeupTrigger::BootInterfaceIntent,
                WakeupTrigger::NicFirmwareObservation,
            ] {
                carbide_instrument::emit(StateHandlerWakeupFailed {
                    trigger,
                    machine_id,
                    err: "enqueue failed".to_string(),
                });
            }
        });

        assert_eq!(logs.len(), 6);
        for log in &logs {
            assert_eq!(log.level, tracing::Level::WARN);
            assert_eq!(log.message, "Failed to wake up state handler for machine");
        }
        let field = |log: &carbide_instrument::testing::CapturedLog, name: &str| {
            log.fields
                .iter()
                .find(|(key, _)| key == name)
                .map(|(_, value)| value.clone())
        };
        assert_eq!(
            field(&logs[0], "trigger"),
            Some("reboot_completed".to_string())
        );
        assert_eq!(field(&logs[0], "machine_id"), Some(machine_id.to_string()));
        assert_eq!(field(&logs[0], "err"), Some("enqueue failed".to_string()));

        for label in [
            "reboot_completed",
            "cleanup_completed",
            "scout_firmware_upgrade_status",
            "dpu_network_status",
            "boot_interface_intent",
            "nic_firmware_observation",
        ] {
            assert_eq!(
                metrics.counter_delta(
                    "carbide_state_handler_wakeup_failures_total",
                    &[("trigger", label)],
                ),
                1.0,
                "counter for trigger={label}"
            );
        }
    }
}
