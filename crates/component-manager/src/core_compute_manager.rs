// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

use std::sync::Arc;
use std::time::Duration;

use carbide_redfish::libredfish::RedfishClientPool;
use carbide_redfish::libredfish::conv::IntoModel;
use carbide_secrets::credentials::Credentials;
use futures_util::{StreamExt, stream};
use model::component_manager::{ComputeTrayComponent, PowerAction};
use tokio::time::{Instant, timeout_at};

use crate::component_common::ComponentPowerStateResult;
use crate::compute_tray_manager::{
    Backend, ComputeTrayEndpoint, ComputeTrayFirmwareUpdateStatus, ComputeTrayResult,
    ComputeTrayVendor,
};
use crate::error::ComponentManagerError;

/// `CoreComputeTrayManager` uses NICo-core's Redfish stack for power control
/// and observations. Each power-read request allows at most eight active BMCs
/// and shares a ten-second budget across queueing, client creation, and reads.
/// Queued endpoints expire without being read; completed observations remain
/// in input order, and duplicate endpoints are read separately. The request
/// owns its futures, so dropping it cancels unfinished setup and reads.
/// Firmware operations are not supported. The pool must support direct
/// authentication with endpoint credentials.
pub struct CoreComputeTrayManager {
    redfish_pool: Arc<dyn RedfishClientPool>,
}

// Bound request latency even when BMCs are unavailable. Waiting devices
// share the same deadline as client creation and power reads.
const POWER_READ_TIMEOUT: Duration = Duration::from_secs(10);
// Bound simultaneous BMC connections per request while allowing slow devices
// to be read independently. Remaining endpoints stay in the borrowed slice.
const POWER_READ_CONCURRENCY: usize = 8;
const POWER_READ_TIMEOUT_ERROR: &str = "redfish power read deadline exceeded";

impl std::fmt::Debug for CoreComputeTrayManager {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("CoreComputeTrayManager").finish()
    }
}

impl CoreComputeTrayManager {
    pub fn new(redfish_pool: Arc<dyn RedfishClientPool>) -> Self {
        Self { redfish_pool }
    }
}

fn map_vendor(vendor: ComputeTrayVendor) -> Option<libredfish::model::service_root::RedfishVendor> {
    use libredfish::model::service_root::RedfishVendor;
    match vendor {
        ComputeTrayVendor::Dell => Some(RedfishVendor::Dell),
        ComputeTrayVendor::Hpe => Some(RedfishVendor::Hpe),
        ComputeTrayVendor::Lenovo => Some(RedfishVendor::Lenovo),
        ComputeTrayVendor::Supermicro => Some(RedfishVendor::Supermicro),
        ComputeTrayVendor::Nvidia | ComputeTrayVendor::Unknown => None,
    }
}

fn map_power_action(action: PowerAction) -> libredfish::SystemPowerControl {
    match action {
        PowerAction::On => libredfish::SystemPowerControl::On,
        PowerAction::GracefulShutdown => libredfish::SystemPowerControl::GracefulShutdown,
        PowerAction::ForceOff => libredfish::SystemPowerControl::ForceOff,
        PowerAction::GracefulRestart => libredfish::SystemPowerControl::GracefulRestart,
        PowerAction::ForceRestart => libredfish::SystemPowerControl::ForceRestart,
        PowerAction::AcPowercycle => libredfish::SystemPowerControl::ACPowercycle,
    }
}

#[async_trait::async_trait]
impl crate::compute_tray_manager::ComputeTrayManager for CoreComputeTrayManager {
    fn power_state_source(&self) -> Option<crate::component_common::PowerStateSource> {
        Some(crate::component_common::PowerStateSource::Redfish)
    }

    fn name(&self) -> &str {
        "core"
    }

    fn backend(&self) -> Backend {
        Backend::Core
    }

    /// `get_power_state` reads each BMC with its endpoint's direct credentials.
    /// Each input gets one result in order, including duplicate endpoints.
    /// Setup, read, and deadline errors stay with the affected device, leaving
    /// completed observations available when another device fails or times out.
    async fn get_power_state(
        &self,
        endpoints: &[ComputeTrayEndpoint],
    ) -> Result<Vec<ComponentPowerStateResult>, ComponentManagerError> {
        let deadline = Instant::now() + POWER_READ_TIMEOUT;
        let mut results: Vec<_> = endpoints
            .iter()
            .map(|endpoint| ComponentPowerStateResult {
                mac_address: endpoint.bmc_mac,
                power_state: Err(POWER_READ_TIMEOUT_ERROR.to_owned()),
            })
            .collect();
        let reads = stream::iter(0..endpoints.len())
            .map(|index| async move {
                // `timeout_at` polls its inner future before its timer. Do not
                // start another device after the shared budget has been spent.
                if Instant::now() >= deadline {
                    return (index, Err(POWER_READ_TIMEOUT_ERROR.to_owned()));
                }
                let endpoint = &endpoints[index];
                let read = async {
                    let Credentials::UsernamePassword { username, password } =
                        &endpoint.bmc_credentials;
                    let client = self
                        .redfish_pool
                        .create_client(
                            &endpoint.bmc_ip.to_string(),
                            Some(443),
                            carbide_redfish::libredfish::RedfishAuth::Direct(
                                username.clone(),
                                password.clone(),
                            ),
                            map_vendor(endpoint.vendor),
                        )
                        .await
                        .map_err(|error| format!("failed to create redfish client: {error}"))?;
                    // Setup can finish on the deadline poll. Check again so
                    // the inner-first poll cannot begin a late power read.
                    if Instant::now() >= deadline {
                        return Err(POWER_READ_TIMEOUT_ERROR.to_owned());
                    }
                    client
                        .get_power_state()
                        .await
                        .map(|state| Some(state.into_model()))
                        .map_err(|error| format!("redfish power read failed: {error}"))
                };
                let power_state = timeout_at(deadline, read)
                    .await
                    .unwrap_or_else(|_| Err(POWER_READ_TIMEOUT_ERROR.to_owned()));
                (index, power_state)
            })
            .buffer_unordered(POWER_READ_CONCURRENCY);
        futures_util::pin_mut!(reads);
        while let Some((index, power_state)) = reads.next().await {
            results[index].power_state = power_state;
        }
        Ok(results)
    }

    async fn power_control(
        &self,
        endpoints: &[ComputeTrayEndpoint],
        action: PowerAction,
    ) -> Result<Vec<ComputeTrayResult>, ComponentManagerError> {
        let redfish_action = map_power_action(action);
        let mut results = Vec::with_capacity(endpoints.len());

        for ep in endpoints {
            let Credentials::UsernamePassword {
                ref username,
                ref password,
            } = ep.bmc_credentials;

            let auth = carbide_redfish::libredfish::RedfishAuth::Direct(
                username.clone(),
                password.clone(),
            );
            let vendor = map_vendor(ep.vendor);

            let outcome = async {
                let client = self
                    .redfish_pool
                    .create_client(&ep.bmc_ip.to_string(), Some(443), auth, vendor)
                    .await
                    .map_err(|e| format!("failed to create Redfish client: {e}"))?;

                client
                    .power(redfish_action)
                    .await
                    .map_err(|e| format!("Redfish power control failed: {e}"))
            }
            .await;

            results.push(ComputeTrayResult {
                bmc_ip: ep.bmc_ip,
                bmc_mac: ep.bmc_mac,
                success: outcome.is_ok(),
                error: outcome.err(),
                backend_job_id: None,
            });
        }

        Ok(results)
    }

    async fn update_firmware(
        &self,
        _endpoints: &[ComputeTrayEndpoint],
        _target_version: &str,
        _components: &[ComputeTrayComponent],
        _options: &crate::types::FirmwareUpdateOptions,
    ) -> Result<Vec<ComputeTrayResult>, ComponentManagerError> {
        Err(ComponentManagerError::Internal(
            "firmware update is not supported by the core compute tray backend".into(),
        ))
    }

    async fn get_firmware_status(
        &self,
        _endpoints: &[ComputeTrayEndpoint],
    ) -> Result<Vec<ComputeTrayFirmwareUpdateStatus>, ComponentManagerError> {
        Err(ComponentManagerError::Internal(
            "firmware status is not supported by the core compute tray backend".into(),
        ))
    }

    async fn list_firmware_bundles(&self) -> Result<Vec<String>, ComponentManagerError> {
        Err(ComponentManagerError::Internal(
            "firmware bundles listing is not supported by the core compute tray backend".into(),
        ))
    }
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;
    use std::sync::Mutex;
    use std::sync::atomic::{AtomicUsize, Ordering};

    use carbide_redfish::libredfish::test_support::{RedfishAuthKind, RedfishSim};
    use carbide_redfish::libredfish::{RedfishAuth, RedfishClientCreationError};
    use carbide_secrets::credentials::CredentialReader;
    use carbide_test_support::value_scenarios;
    use libredfish::model::service_root::RedfishVendor;
    use libredfish::{PowerState as RedfishPowerState, Redfish, RedfishError, RedfishFuture};
    use mac_address::MacAddress;
    use model::machine::PowerState;
    use tokio::sync::oneshot;

    use super::*;
    use crate::compute_tray_manager::ComputeTrayManager;

    /// `test_compute_tray_endpoint` gives each index distinct BMC identity and
    /// credentials, cycling vendors so reads also exercise vendor selection.
    fn test_compute_tray_endpoint(index: u8) -> ComputeTrayEndpoint {
        ComputeTrayEndpoint {
            vendor: match index % 3 {
                0 => ComputeTrayVendor::Dell,
                1 => ComputeTrayVendor::Hpe,
                _ => ComputeTrayVendor::Unknown,
            },
            bmc_ip: std::net::Ipv4Addr::new(192, 0, 2, index).into(),
            bmc_mac: MacAddress::new([0, 0, 0, 0, 0, index]),
            bmc_credentials: Credentials::UsernamePassword {
                username: format!("admin-{index}"),
                password: format!("test-password-{index}"),
            },
        }
    }

    /// `PowerReadPhase` selects the failure or cancellation boundary under test.
    #[derive(Clone, Copy, Debug, PartialEq, Eq)]
    enum PowerReadPhase {
        ClientCreation,
        PowerRead,
    }

    /// `PowerReadTestGates` pauses selected hosts at one stage of a test read.
    /// Each registered receiver is consumed by the first wait for that host.
    /// Once consumed, it belongs to the waiting future, so cancelling that
    /// future closes its sender. Gates never reached remain in `waiters`.
    #[derive(Default)]
    struct PowerReadTestGates {
        /// Registered gates that no call has reached yet.
        waiters: Mutex<HashMap<String, oneshot::Receiver<()>>>,
        /// Cumulative reached gates; excludes ungated calls and never decrements.
        started: AtomicUsize,
    }

    impl PowerReadTestGates {
        /// `block` registers a gate for the next wait on this host, replacing
        /// any prior gate. Keep the returned sender alive until releasing the
        /// gate or cancelling the request, or a waiting future will panic.
        fn block(&self, endpoint: &ComputeTrayEndpoint) -> oneshot::Sender<()> {
            let (sender, receiver) = oneshot::channel();
            self.waiters
                .lock()
                .unwrap()
                .insert(endpoint.bmc_ip.to_string(), receiver);
            sender
        }

        /// `wait` consumes a registered gate and waits for the test to release
        /// it. Hosts without a registered gate continue immediately.
        async fn wait(&self, host: &str) {
            let waiter = self.waiters.lock().unwrap().remove(host);
            if let Some(waiter) = waiter {
                self.started.fetch_add(1, Ordering::Relaxed);
                waiter
                    .await
                    .expect("test must keep unreleased senders alive");
            }
        }
    }

    /// `PowerReadTestPool` checks endpoint credentials, port, and vendor before
    /// forwarding client creation to `RedfishSim`. Separate setup and read
    /// gates let tests stop either phase without relying on wall-clock delays.
    struct PowerReadTestPool {
        sim: RedfishSim,
        endpoints: HashMap<String, ComputeTrayEndpoint>,
        setup_gates: PowerReadTestGates,
        read_gates: Arc<PowerReadTestGates>,
    }

    /// `power_read_test_pool` records expected endpoints and starts with no
    /// blocked hosts; each test registers only the gates it needs.
    fn power_read_test_pool(endpoints: &[ComputeTrayEndpoint]) -> Arc<PowerReadTestPool> {
        Arc::new(PowerReadTestPool {
            sim: RedfishSim::default(),
            endpoints: endpoints
                .iter()
                .map(|endpoint| (endpoint.bmc_ip.to_string(), endpoint.clone()))
                .collect(),
            setup_gates: PowerReadTestGates::default(),
            read_gates: Arc::new(PowerReadTestGates::default()),
        })
    }

    #[async_trait::async_trait]
    impl RedfishClientPool for PowerReadTestPool {
        /// `create_client` validates the request before waiting at the setup
        /// gate, then wraps the simulator client with its separate read gate.
        async fn create_client(
            &self,
            host: &str,
            port: Option<u16>,
            auth: RedfishAuth,
            vendor: Option<RedfishVendor>,
        ) -> Result<Box<dyn Redfish>, RedfishClientCreationError> {
            let expected = &self.endpoints[host];
            let Credentials::UsernamePassword { username, password } = &expected.bmc_credentials;
            assert_eq!(port, Some(443));
            assert!(matches!(
                &auth,
                RedfishAuth::Direct(actual_username, actual_password)
                    if actual_username == username && actual_password == password
            ));
            assert_eq!(vendor, map_vendor(expected.vendor));

            self.setup_gates.wait(host).await;
            let inner = self.sim.create_client(host, port, auth, vendor).await?;
            Ok(Box::new(PowerReadTestClient {
                inner,
                gates: self.read_gates.clone(),
                host: host.to_owned(),
            }))
        }

        fn credential_reader(&self) -> &dyn CredentialReader {
            self.sim.credential_reader()
        }
    }

    /// `PowerReadTestClient` pauses power reads before they reach the simulator.
    /// Power commands still reach `RedfishSim`, so the tests can detect an
    /// accidental mutation through its recorded actions.
    struct PowerReadTestClient {
        inner: Box<dyn Redfish>,
        gates: Arc<PowerReadTestGates>,
        host: String,
    }

    impl Redfish for PowerReadTestClient {
        fn std_redfish(&self) -> &libredfish::standard::RedfishStandard {
            self.inner.std_redfish()
        }

        /// `get_power_state` waits at this host's read gate before consuming a
        /// simulated state or injected read failure.
        fn get_power_state<'a>(
            &'a self,
        ) -> RedfishFuture<'a, Result<RedfishPowerState, RedfishError>> {
            Box::pin(async move {
                self.gates.wait(&self.host).await;
                self.inner.get_power_state().await
            })
        }

        /// `power` forwards commands so an unintended write stays observable.
        fn power<'a>(
            &'a self,
            action: libredfish::SystemPowerControl,
        ) -> RedfishFuture<'a, Result<(), RedfishError>> {
            self.inner.power(action)
        }
    }

    /// Each input, including repeats, must retain its identity and reported
    /// state. Observing power must use its endpoint's credentials and vendor
    /// without issuing power commands.
    #[tokio::test]
    async fn power_reads_preserve_states_identity_and_order_without_mutations() {
        // Transitional and unknown states must survive alongside on and off.
        let states = [
            (RedfishPowerState::On, PowerState::On),
            (RedfishPowerState::Off, PowerState::Off),
            (RedfishPowerState::PoweringOn, PowerState::PoweringOn),
            (RedfishPowerState::PoweringOff, PowerState::PoweringOff),
            (RedfishPowerState::Paused, PowerState::Paused),
            (RedfishPowerState::Reset, PowerState::Reset),
            (RedfishPowerState::Unknown, PowerState::Unknown),
        ];

        // Repeat one endpoint to catch accidental deduplication of results.
        let mut endpoints: Vec<_> = (1..=7).map(test_compute_tray_endpoint).collect();
        endpoints.push(endpoints[0].clone());
        let pool = power_read_test_pool(&endpoints);
        let manager = CoreComputeTrayManager::new(pool.clone());
        assert!(manager.get_power_state(&[]).await.unwrap().is_empty());

        for (endpoint, (state, _)) in endpoints.iter().zip(states) {
            pool.sim
                .set_power_state(&endpoint.bmc_ip.to_string(), state);
        }

        let before = pool.sim.timepoint();
        let results = manager.get_power_state(&endpoints).await.unwrap();

        // Every result belongs to its input slot, even the repeated endpoint.
        let expected: Vec<_> = endpoints
            .iter()
            .zip(states.iter().chain(&states[..1]))
            .map(|(endpoint, (_, state))| ComponentPowerStateResult {
                mac_address: endpoint.bmc_mac,
                power_state: Ok(Some(*state)),
            })
            .collect();
        assert_eq!(results, expected);
        assert!(pool.sim.actions_since(&before).all_hosts().is_empty());

        // Each input must create its own client with the supplied connection
        // details; the pool also checks exact credentials and port.
        let calls = pool.sim.create_client_calls();
        assert_eq!(calls.len(), endpoints.len());

        for endpoint in &endpoints {
            let host = endpoint.bmc_ip.to_string();
            let expected_count = endpoints
                .iter()
                .filter(|candidate| candidate.bmc_ip == endpoint.bmc_ip)
                .count();
            assert_eq!(
                calls.iter().filter(|call| call.host == host).count(),
                expected_count
            );
        }

        for call in calls {
            assert_eq!(call.vendor, map_vendor(pool.endpoints[&call.host].vendor));
            assert_eq!(call.auth, RedfishAuthKind::Direct);
        }
    }

    /// Client creation and power-read failures must identify the failed phase
    /// on that device, while a healthy peer still returns its observation.
    #[tokio::test]
    async fn power_reads_report_client_and_read_failures_per_device() {
        for (phase, expected_prefix) in [
            (
                PowerReadPhase::ClientCreation,
                "failed to create redfish client:",
            ),
            (PowerReadPhase::PowerRead, "redfish power read failed:"),
        ] {
            let endpoints = [test_compute_tray_endpoint(1), test_compute_tray_endpoint(2)];
            let pool = power_read_test_pool(&endpoints);
            let healthy_setup = pool.setup_gates.block(&endpoints[1]);
            pool.sim
                .set_power_state(&endpoints[1].bmc_ip.to_string(), RedfishPowerState::Off);

            // Setup errors persist until cleared; a read error affects only
            // the next read. Hold the healthy peer so the first device fails.
            match phase {
                PowerReadPhase::ClientCreation => {
                    pool.sim.set_create_client_error("client unavailable")
                }
                PowerReadPhase::PowerRead => {
                    pool.sim.fail_next_power_state_read("read unavailable")
                }
            }

            let manager = CoreComputeTrayManager::new(pool.clone());
            let before = pool.sim.timepoint();
            let mut request = Box::pin(manager.get_power_state(&endpoints));

            // Keep the healthy peer out of the simulator until the first
            // device has consumed the injected setup or read failure.
            assert!(futures_util::poll!(request.as_mut()).is_pending());
            let calls = pool.sim.create_client_calls();
            assert_eq!(calls.len(), 1);
            assert_eq!(calls[0].host, endpoints[0].bmc_ip.to_string());

            // Release the healthy peer only after removing any setup error.
            pool.sim.clear_create_client_error();
            healthy_setup.send(()).unwrap();
            let results = request.await.unwrap();

            // Failure context must stay with the first device, without hiding
            // or changing the second device's state.
            assert_eq!(results.len(), endpoints.len());
            assert_eq!(results[0].mac_address, endpoints[0].bmc_mac);
            let error = results[0].power_state.as_ref().unwrap_err();
            assert!(error.starts_with(expected_prefix), "{phase:?}: {error}");
            assert!(error.contains("unavailable"), "{phase:?}: {error}");
            assert_eq!(
                results[1],
                ComponentPowerStateResult {
                    mac_address: endpoints[1].bmc_mac,
                    power_state: Ok(Some(PowerState::Off)),
                }
            );

            assert_eq!(pool.sim.create_client_calls().len(), endpoints.len());
            assert!(pool.sim.actions_since(&before).all_hosts().is_empty());
        }
    }

    /// Queueing, setup, and reads share one deadline, so slow devices cannot
    /// extend a request or discard completed observations. Using more than
    /// eight blocked devices also exercises the concurrency limit.
    #[tokio::test(start_paused = true)]
    async fn power_read_deadline_bounds_concurrency_and_preserves_completed_observations() {
        // Complete one observation immediately and another after six seconds,
        // while the remaining devices stay blocked or queued.
        let endpoints: Vec<_> = (0..11).map(test_compute_tray_endpoint).collect();
        let pool = power_read_test_pool(&endpoints);
        pool.sim
            .set_power_state(&endpoints[0].bmc_ip.to_string(), RedfishPowerState::On);
        pool.sim
            .set_power_state(&endpoints[3].bmc_ip.to_string(), RedfishPowerState::Off);
        let mut setup_senders: Vec<_> = endpoints[1..]
            .iter()
            .map(|endpoint| pool.setup_gates.block(endpoint))
            .collect();
        let first_read = pool.read_gates.block(&endpoints[1]);
        let later_read = pool.read_gates.block(&endpoints[3]);

        let manager = CoreComputeTrayManager::new(pool.clone());
        let before = pool.sim.timepoint();
        let mut request = Box::pin(manager.get_power_state(&endpoints));

        // The first device completes immediately, leaving eight setup waits
        // active and the last two devices still queued.
        assert!(futures_util::poll!(request.as_mut()).is_pending());
        assert_eq!(pool.setup_gates.started.load(Ordering::Relaxed), 8);

        // Setup and power reads must spend the same budget. Start two reads
        // after six seconds, leaving one blocked and finishing the later one.
        tokio::time::advance(Duration::from_secs(6)).await;
        setup_senders.remove(0).send(()).unwrap();
        setup_senders.remove(1).send(()).unwrap();
        later_read.send(()).unwrap();
        assert!(futures_util::poll!(request.as_mut()).is_pending());
        assert_eq!(pool.read_gates.started.load(Ordering::Relaxed), 2);
        assert_eq!(pool.setup_gates.started.load(Ordering::Relaxed), 9);

        // The newly admitted device gets the remaining four seconds, not a
        // fresh deadline. Also prove that the request does not expire early.
        tokio::time::advance(Duration::from_millis(3999)).await;
        assert!(futures_util::poll!(request.as_mut()).is_pending());

        // Tokio rounds timer deadlines up to its next millisecond tick.
        tokio::time::advance(Duration::from_millis(2)).await;
        let std::task::Poll::Ready(results) = futures_util::poll!(request.as_mut()) else {
            panic!("power read exceeded the ten-second request budget");
        };

        // Completed observations survive in their original slots; every
        // unfinished device, including the queued one, reports a timeout.
        let results = results.unwrap();
        assert_eq!(results.len(), endpoints.len());

        for (index, result) in results.iter().enumerate() {
            assert_eq!(result.mac_address, endpoints[index].bmc_mac);
            if index == 0 {
                assert_eq!(result.power_state, Ok(Some(PowerState::On)));
            } else if index == 3 {
                assert_eq!(result.power_state, Ok(Some(PowerState::Off)));
            } else {
                assert_eq!(result.power_state, Err(POWER_READ_TIMEOUT_ERROR.into()));
            }
        }

        // Started waits lose their receivers when the deadline cancels them.
        // The final setup receiver stays registered because it was not reached.
        assert_eq!(pool.setup_gates.started.load(Ordering::Relaxed), 9);
        assert_eq!(pool.read_gates.started.load(Ordering::Relaxed), 2);
        assert!(
            first_read.is_closed(),
            "timed-out power read was not dropped"
        );
        assert!(
            setup_senders[..setup_senders.len() - 1]
                .iter()
                .all(oneshot::Sender::is_closed)
        );
        assert!(
            !setup_senders.last().unwrap().is_closed(),
            "queued device started after deadline"
        );
        assert!(pool.sim.actions_since(&before).all_hosts().is_empty());
    }

    /// Setup completing exactly at the deadline must not start a power read.
    /// This protects the explicit guard after setup: `timeout_at` polls its
    /// inner future before checking whether the deadline has expired.
    #[tokio::test(start_paused = true)]
    async fn setup_finishing_at_deadline_does_not_start_a_power_read() {
        let endpoints = [test_compute_tray_endpoint(1)];
        let pool = power_read_test_pool(&endpoints);
        let setup = pool.setup_gates.block(&endpoints[0]);
        let read = pool.read_gates.block(&endpoints[0]);
        let manager = CoreComputeTrayManager::new(pool.clone());
        let before = pool.sim.timepoint();
        let mut request = Box::pin(manager.get_power_state(&endpoints));

        // Reach setup while keeping the read gate registered but untouched.
        assert!(futures_util::poll!(request.as_mut()).is_pending());
        assert_eq!(pool.setup_gates.started.load(Ordering::Relaxed), 1);

        // Make setup ready on the deadline itself. `timeout_at` polls it
        // first, so only the post-setup guard can prevent the power read.
        tokio::time::advance(Duration::from_secs(10)).await;
        setup.send(()).unwrap();
        let std::task::Poll::Ready(results) = futures_util::poll!(request.as_mut()) else {
            panic!("setup finishing at the deadline did not return immediately");
        };

        // Client creation finished, but the read gate must remain unconsumed.
        assert_eq!(
            results.unwrap(),
            vec![ComponentPowerStateResult {
                mac_address: endpoints[0].bmc_mac,
                power_state: Err(POWER_READ_TIMEOUT_ERROR.into()),
            }]
        );
        assert_eq!(pool.sim.create_client_calls().len(), 1);
        assert_eq!(pool.read_gates.started.load(Ordering::Relaxed), 0);
        assert!(!read.is_closed(), "power read started at the deadline");
        assert!(pool.sim.actions_since(&before).all_hosts().is_empty());
    }

    /// Dropping the caller's future must drop active setup or read waits and
    /// leave queued work unstarted. No detached task may keep using the BMCs.
    #[tokio::test]
    async fn dropping_power_read_request_cancels_setup_reads_and_queued_work() {
        for phase in [PowerReadPhase::ClientCreation, PowerReadPhase::PowerRead] {
            // Block nine devices at the selected phase so one stays queued
            // behind the eight active requests.
            let endpoints: Vec<_> = (0..9).map(test_compute_tray_endpoint).collect();
            let pool = power_read_test_pool(&endpoints);
            let gates = match phase {
                PowerReadPhase::ClientCreation => &pool.setup_gates,
                PowerReadPhase::PowerRead => pool.read_gates.as_ref(),
            };
            let senders: Vec<_> = endpoints
                .iter()
                .map(|endpoint| gates.block(endpoint))
                .collect();

            let manager = CoreComputeTrayManager::new(pool.clone());
            let before = pool.sim.timepoint();
            let mut request = Box::pin(manager.get_power_state(&endpoints));

            // Reach the eight waits before simulating caller cancellation.
            assert!(
                futures_util::poll!(request.as_mut()).is_pending(),
                "{phase:?}"
            );
            assert_eq!(gates.started.load(Ordering::Relaxed), 8, "{phase:?}");
            drop(request);

            // Consumed receivers drop with the request; the ninth receiver
            // remains registered in the fixture because no call reached it.
            assert!(
                senders[..8].iter().all(oneshot::Sender::is_closed),
                "{phase:?}"
            );
            assert!(
                !senders.last().unwrap().is_closed(),
                "{phase:?}: queued device started after cancellation"
            );
            assert_eq!(gates.started.load(Ordering::Relaxed), 8, "{phase:?}");
            assert!(
                pool.sim.actions_since(&before).all_hosts().is_empty(),
                "{phase:?}"
            );
        }
    }

    /// Vendor selection must use the matching Redfish adapter where supported
    /// and leave the generic path available for other compute trays.
    #[test]
    fn compute_tray_vendor_maps_to_redfish_vendor() {
        value_scenarios!(map_vendor:
            "supported Redfish vendors" {
                ComputeTrayVendor::Dell => Some(RedfishVendor::Dell),
                ComputeTrayVendor::Hpe => Some(RedfishVendor::Hpe),
                ComputeTrayVendor::Lenovo => Some(RedfishVendor::Lenovo),
                ComputeTrayVendor::Supermicro => Some(RedfishVendor::Supermicro),
            }

            "vendors without a Redfish adapter" {
                ComputeTrayVendor::Nvidia => None,
                ComputeTrayVendor::Unknown => None,
            }
        );
    }
}
