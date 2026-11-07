// SPDX-FileCopyrightText: Copyright (c) 2025-2026 MIRANTIS, INC. & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0
//
// Licensed under the Apache License, Version 2.0 (the "License");
// you may not use this file except in compliance with the License.
// You may obtain a copy of the License at
//
// http://www.apache.org/licenses/LICENSE-2.0
//
// Unless required by applicable law or agreed to in writing, software
// distributed under the License is distributed on an "AS IS" BASIS,
// WITHOUT WARRANTIES OR CONDITIONS OF ANY KIND, either express or implied.
// See the License for the specific language governing permissions and
// limitations under the License.

//! Reports this host's LLDP neighbors to nico-api over `ReportLldpNeighbors`,
//! skipping the RPC when the snapshot has not changed since the last one
//! nico-api acknowledged.

use std::time::Duration;

use ::rpc::forge::{LldpNeighborReport, LldpReport, LldpReportResult};
use ::rpc::forge_tls_client::{ApiConfig, ForgeClientConfig, ForgeTlsClient};
use carbide_host_support::lldp_collector::{LldpCollectorResult, LldpNeighbor};
use carbide_host_support::lldp_snapshot_cache::{LldpSnapshotCache, result_of};
use carbide_uuid::machine::MachineId;

#[derive(thiserror::Error, Debug)]
pub(crate) enum LldpReportError {
    #[error("could not connect to the nico-api server: {0}")]
    Connect(String),
    #[error("report_lldp_neighbors gRPC call failed: {0}")]
    Rpc(String),
    #[error("report to nico-api timed out after {0:?}")]
    Timeout(Duration),
}

/// Bounds one poll iteration's report, so a stalled nico-api cannot hold up the
/// scout loop behind it.
const REPORT_TIMEOUT: Duration = Duration::from_secs(5);

/// Outcome of a [`LldpReporter::report`] call.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ReportOutcome {
    /// The report was sent to nico-api and the cache advanced.
    Sent,
    /// The snapshot matched the last one nico-api acknowledged, so nothing was
    /// sent. nico-api already holds this topology.
    UnchangedSkipped,
}

/// Sends LLDP reports over the standalone `ReportLldpNeighbors` RPC, skipping
/// the call when the snapshot has not changed. Hold one instance across the
/// poll loop so the cache persists between iterations.
pub(crate) struct LldpReporter {
    machine_id: MachineId,
    api: String,
    client_config: ForgeClientConfig,
    cache: LldpSnapshotCache,
}

impl LldpReporter {
    pub(crate) fn new(
        machine_id: MachineId,
        api: String,
        client_config: ForgeClientConfig,
    ) -> Self {
        Self {
            machine_id,
            api,
            client_config,
            cache: LldpSnapshotCache::new(),
        }
    }

    /// Report one LLDP collection attempt to nico-api, skipping the RPC when
    /// the snapshot matches the last one nico-api acknowledged.
    pub(crate) async fn report(
        &mut self,
        collected: LldpCollectorResult<Vec<LldpNeighbor>>,
    ) -> Result<ReportOutcome, LldpReportError> {
        let Self {
            machine_id,
            api,
            client_config,
            cache,
        } = self;

        let report = cache.classify(collected);
        if result_of(&report) == LldpReportResult::Unchanged {
            return Ok(ReportOutcome::UnchangedSkipped);
        }

        Self::send_and_confirm(cache, *machine_id, report, REPORT_TIMEOUT, |request| {
            send_report(api, client_config, request)
        })
        .await
    }

    /// Send effect injected so the transport can be unit-tested without a live
    /// gRPC connection. The cache advances only after `send` succeeds, so a
    /// failed or timed-out send is retried on the next poll rather than being
    /// suppressed as unchanged.
    async fn send_and_confirm<F, Fut>(
        cache: &mut LldpSnapshotCache,
        machine_id: MachineId,
        report: LldpReport,
        report_timeout: Duration,
        send: F,
    ) -> Result<ReportOutcome, LldpReportError>
    where
        F: FnOnce(LldpNeighborReport) -> Fut,
        Fut: std::future::Future<Output = Result<(), LldpReportError>>,
    {
        let request = LldpNeighborReport {
            machine_id: Some(machine_id),
            report: Some(report.clone()),
        };
        tokio::time::timeout(report_timeout, send(request))
            .await
            .map_err(|_| LldpReportError::Timeout(report_timeout))??;

        cache.confirm_reported(report);
        Ok(ReportOutcome::Sent)
    }
}

/// Report one completed LLDP collection to nico-api, re-sending only when the
/// snapshot changed since the last successful report.
///
/// A failed collection is reported too, so nico-api can tell a host whose
/// `lldpcli` has stopped answering from one whose topology is simply stable.
/// The collection itself is made by the collector task, which also logs the
/// failure.
pub(crate) async fn report_lldp_neighbors(
    reporter: &mut LldpReporter,
    collected: LldpCollectorResult<Vec<LldpNeighbor>>,
) {
    match reporter.report(collected).await {
        Ok(ReportOutcome::Sent) => tracing::info!("Reported LLDP neighbors"),
        Ok(ReportOutcome::UnchangedSkipped) => {
            tracing::debug!("LLDP neighbors unchanged; skipping report")
        }
        Err(error) => tracing::warn!(%error, "Could not report LLDP neighbors"),
    }
}

async fn send_report(
    api: &str,
    client_config: &ForgeClientConfig,
    report: LldpNeighborReport,
) -> Result<(), LldpReportError> {
    let mut client = ForgeTlsClient::retry_build(&ApiConfig::new(api, client_config))
        .await
        .map_err(|e| LldpReportError::Connect(e.to_string()))?;

    tracing::trace!("report_lldp_neighbors: {report:?}");
    client
        .report_lldp_neighbors(tonic::Request::new(report))
        .await
        .map_err(|e| LldpReportError::Rpc(e.to_string()))?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use std::cell::RefCell;

    use ::rpc::machine_discovery::LldpSwitchData;

    use super::*;

    const TEST_REPORT_TIMEOUT: Duration = Duration::from_millis(50);

    fn neighbor(mac: &str, local_port: &str, remote_port: &str) -> LldpNeighbor {
        LldpNeighbor {
            local_mac: mac.to_string(),
            switch: LldpSwitchData {
                local_port: local_port.to_string(),
                remote_port_type: "ifname".to_string(),
                remote_port_value: remote_port.to_string(),
                ..Default::default()
            },
        }
    }

    /// One neighbor on one port, the simplest snapshot the collector can yield.
    fn one_neighbor() -> Vec<LldpNeighbor> {
        vec![neighbor("aa:bb:cc:dd:ee:ff", "p0", "p0")]
    }

    fn machine_id() -> MachineId {
        // Any valid id works; its value is irrelevant to the transport.
        use std::str::FromStr;
        MachineId::from_str("fm100dsasb5dsh6e6ogogslpovne4rj82rp9jlf00qd7mcvmaadv85phk3g")
            .expect("valid test machine id")
    }

    #[derive(Default)]
    struct MockGrpc {
        received: RefCell<Vec<LldpNeighborReport>>,
    }

    impl MockGrpc {
        fn sender(
            &self,
        ) -> impl FnOnce(LldpNeighborReport) -> std::future::Ready<Result<(), LldpReportError>> + '_
        {
            move |request| {
                self.received.borrow_mut().push(request);
                std::future::ready(Ok(()))
            }
        }

        fn call_count(&self) -> usize {
            self.received.borrow().len()
        }
    }

    // The transport wraps the report in a `LldpNeighborReport` addressed to this
    // machine, and confirms the cache only after the send succeeds.
    #[tokio::test]
    async fn successful_send_is_confirmed() {
        let mut cache = LldpSnapshotCache::new();
        let grpc = MockGrpc::default();

        let report = cache.classify(Ok(one_neighbor()));
        let outcome = LldpReporter::send_and_confirm(
            &mut cache,
            machine_id(),
            report.clone(),
            TEST_REPORT_TIMEOUT,
            grpc.sender(),
        )
        .await
        .unwrap();

        assert_eq!(outcome, ReportOutcome::Sent);
        assert_eq!(grpc.call_count(), 1);
        assert_eq!(
            grpc.received.borrow()[0],
            LldpNeighborReport {
                machine_id: Some(machine_id()),
                report: Some(report),
            }
        );
        assert_eq!(
            result_of(&cache.classify(Ok(one_neighbor()))),
            LldpReportResult::Unchanged,
            "a confirmed send advances the cache"
        );
    }

    // A send that never resolves is abandoned after the configured timeout and
    // must leave the cache untouched, so the snapshot is retried rather than
    // suppressed as unchanged. `start_paused` lets the runtime jump the clock to
    // the timer deadline, so the test does not actually wait.
    #[tokio::test(start_paused = true)]
    async fn timed_out_send_is_not_confirmed() {
        let mut cache = LldpSnapshotCache::new();
        let report = cache.classify(Ok(one_neighbor()));

        let outcome = LldpReporter::send_and_confirm(
            &mut cache,
            machine_id(),
            report,
            TEST_REPORT_TIMEOUT,
            |_request| std::future::pending(),
        )
        .await;

        assert!(
            matches!(outcome, Err(LldpReportError::Timeout(timeout)) if timeout == TEST_REPORT_TIMEOUT),
            "a send that never resolves must time out, got {outcome:?}"
        );
        assert_eq!(
            result_of(&cache.classify(Ok(one_neighbor()))),
            LldpReportResult::Updated,
            "a timed-out send must leave the snapshot to be retried"
        );
    }
}
