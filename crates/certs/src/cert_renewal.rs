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

use std::ops::Add;
use std::sync::Arc;
use std::time::{Duration, Instant};

use ::rpc::forge as rpc;
use ::rpc::forge_tls_client::{self, ApiConfig, ForgeClientConfig};
use carbide_host_support::registration;
use carbide_instrument::{Event, Outcome, emit};
use eyre::Context;
use forge_tls::client_config::ClientCert;
use rand::RngExt;

/// Certificates are renewed between in these 2 time intervals
const MIN_CERT_RENEWAL_TIME_SECS: u64 = 5 * 24 * 60 * 60; // 5 days
const MAX_CERT_RENEWAL_TIME_SECS: u64 = 7 * 24 * 60 * 60; // 7 days

const MIN_CERT_RENEWAL_FAILURE_TIME_SECS: u64 = 60; // 1 min
const MAX_CERT_RENEWAL_FAILURE_TIME_SECS: u64 = 5 * 60; // 5min

/// A client-certificate renewal ran to completion, successfully or not. A
/// call that finds the renewal window still closed neither counts nor logs.
///
/// One client-certificate renewal attempt. Both cases move the same counter
/// and both schedule the next attempt; only a failure has an error to report.
#[derive(Event)]
#[event(
    event_name = "cert_renewal_completed",
    metric_name = "carbide_certs_renewals_total",
    component = "carbide-certs",
    metric = counter,
    describe = "Number of client certificate renewal attempts, by outcome",
    labels(outcome: Outcome),
)]
enum CertRenewalCompleted {
    #[event(labels(outcome = Ok), log = info, message = "Client certificate renewal completed")]
    Succeeded {
        /// Seconds until the next renewal attempt.
        #[context]
        next_attempt_in_secs: u64,
    },

    #[event(labels(outcome = Error), log = error, message = "Client certificate renewal completed")]
    Failed {
        /// The failure's error chain.
        #[context]
        error: String,
        /// Seconds until the next renewal attempt.
        #[context]
        next_attempt_in_secs: u64,
    },
}

pub struct ClientCertRenewer {
    cert_renewal_time: std::time::Instant,
    forge_api_server: String,
    client_config: Arc<ForgeClientConfig>,
}

impl ClientCertRenewer {
    pub fn new(forge_api_server: String, client_config: Arc<ForgeClientConfig>) -> Self {
        let cert_renewal_period =
            rand::rng().random_range(MIN_CERT_RENEWAL_TIME_SECS..MAX_CERT_RENEWAL_TIME_SECS);
        let cert_renewal_time = Instant::now().add(Duration::from_secs(cert_renewal_period));

        Self {
            cert_renewal_time,
            forge_api_server,
            client_config,
        }
    }

    /// Renews Client certificates once a certain timeframe has elapsed
    pub async fn renew_certificates_if_necessary(
        &mut self,
        override_client_cert: Option<&ClientCert>,
    ) {
        let now = std::time::Instant::now();
        if now > self.cert_renewal_time {
            let result = self.renew_certificates(override_client_cert).await;
            let cert_renewal_period = match &result {
                Ok(()) => {
                    rand::rng().random_range(MIN_CERT_RENEWAL_TIME_SECS..MAX_CERT_RENEWAL_TIME_SECS)
                }
                Err(_) => rand::rng().random_range(
                    MIN_CERT_RENEWAL_FAILURE_TIME_SECS..MAX_CERT_RENEWAL_FAILURE_TIME_SECS,
                ),
            };
            emit(match result.err() {
                None => CertRenewalCompleted::Succeeded {
                    next_attempt_in_secs: cert_renewal_period,
                },
                Some(err) => CertRenewalCompleted::Failed {
                    error: format!("{err:#}"),
                    next_attempt_in_secs: cert_renewal_period,
                },
            });
            self.cert_renewal_time = now.add(Duration::from_secs(cert_renewal_period));
        }
    }

    /// Enforces cert renewal on the next renew_certificates_if_necessary call
    pub fn renew_on_next_check(&mut self) {
        self.cert_renewal_time = std::time::Instant::now();
    }

    async fn renew_certificates(
        &mut self,
        override_client_cert: Option<&ClientCert>,
    ) -> Result<(), eyre::Report> {
        tracing::info!("Trying to renew TLS client certificates");
        let mut client = forge_tls_client::ForgeTlsClient::retry_build(&ApiConfig::new(
            &self.forge_api_server,
            &self.client_config,
        ))
        .await
        .wrap_err("renew_certificates: failed to build forge API server client")?;

        let request = tonic::Request::new(rpc::MachineCertificateRenewRequest {});
        let machine_certificate_result = client
            .renew_machine_certificate(request)
            .await
            .wrap_err("renew_certificates: error while executing the renew_certificates gRPC call")?
            .into_inner();

        tracing::info!("Received new machine certificate. Attempting to write to disk.");
        registration::write_certs(
            machine_certificate_result.machine_certificate,
            override_client_cert,
        )
        .await
        .wrap_err("renew_certificates: failed to write certs to disk")?;

        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use carbide_instrument::testing::{CapturedLog, MetricsCapture, capture_logs};

    use super::*;

    fn field<'a>(log: &'a CapturedLog, name: &str) -> Option<&'a str> {
        log.fields
            .iter()
            .find(|(key, _)| key == name)
            .map(|(_, value)| value.as_str())
    }

    /// A completed renewal logs the completion at INFO -- with the next
    /// renewal window as context -- and moves only the `outcome="ok"` series.
    #[test]
    fn successful_renewal_logs_info_and_counts_ok() {
        let metrics = MetricsCapture::start();
        let logs = capture_logs(|| {
            emit(CertRenewalCompleted::Succeeded {
                next_attempt_in_secs: 432_000,
            });
        });

        assert_eq!(logs.len(), 1);
        assert_eq!(logs[0].level, tracing::Level::INFO);
        assert_eq!(logs[0].message, "Client certificate renewal completed");
        assert_eq!(field(&logs[0], "outcome"), Some("ok"));
        assert_eq!(field(&logs[0], "next_attempt_in_secs"), Some("432000"));
        assert_eq!(
            metrics.counter_delta("carbide_certs_renewals_total", &[("outcome", "ok")]),
            1.0
        );
        assert_eq!(
            metrics.counter_delta("carbide_certs_renewals_total", &[("outcome", "error")]),
            0.0
        );
    }

    /// A failed renewal logs the completion at ERROR -- with the error chain
    /// and the retry window as context -- and moves only the
    /// `outcome="error"` series.
    #[test]
    fn failed_renewal_logs_error_and_counts_error() {
        let metrics = MetricsCapture::start();
        let logs = capture_logs(|| {
            emit(CertRenewalCompleted::Failed {
                error: "renew_certificates: deadline exceeded".to_string(),
                next_attempt_in_secs: 90,
            });
        });

        assert_eq!(logs.len(), 1);
        assert_eq!(logs[0].level, tracing::Level::ERROR);
        assert_eq!(logs[0].message, "Client certificate renewal completed");
        assert_eq!(field(&logs[0], "outcome"), Some("error"));
        assert_eq!(
            field(&logs[0], "error"),
            Some("renew_certificates: deadline exceeded")
        );
        assert_eq!(field(&logs[0], "next_attempt_in_secs"), Some("90"));
        assert_eq!(
            metrics.counter_delta("carbide_certs_renewals_total", &[("outcome", "error")]),
            1.0
        );
        assert_eq!(
            metrics.counter_delta("carbide_certs_renewals_total", &[("outcome", "ok")]),
            0.0
        );
    }

    /// A call inside the renewal window skips: no renewal is attempted, so
    /// nothing counts and nothing logs.
    #[test]
    fn skipped_renewal_neither_counts_nor_logs() {
        let metrics = MetricsCapture::start();
        // A fresh renewer's first renewal is days away, so this call skips.
        let mut renewer = ClientCertRenewer::new(
            "https://localhost:1".to_string(),
            Arc::new(ForgeClientConfig::new(String::new(), None)),
        );
        let runtime = tokio::runtime::Builder::new_current_thread()
            .build()
            .expect("current-thread runtime");
        let logs = capture_logs(|| {
            runtime.block_on(renewer.renew_certificates_if_necessary(None));
        });

        assert!(logs.is_empty(), "a skipped renewal must not log: {logs:?}");
        assert_eq!(
            metrics.counter_delta("carbide_certs_renewals_total", &[("outcome", "ok")]),
            0.0
        );
        assert_eq!(
            metrics.counter_delta("carbide_certs_renewals_total", &[("outcome", "error")]),
            0.0
        );
    }
}
