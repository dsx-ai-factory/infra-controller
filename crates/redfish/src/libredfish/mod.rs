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

mod implementation;
mod instrumented;

pub mod auth;
pub mod conv;
pub mod dpu_bios;
pub mod error;
pub mod host_interface;
#[cfg(feature = "test-support")]
pub mod test_support;

use std::net::SocketAddr;
use std::sync::Arc;

use arc_swap::ArcSwap;
use async_trait::async_trait;
pub use auth::RedfishAuth;
use carbide_instrument::{Event, Instrumentation, LabelValue};
use carbide_secrets::credentials::{CredentialReader, Credentials};
use carbide_utils::HostPortPair;
use carbide_utils::redfish::{BmcAccessInfo, redact_redfish_response_body};
pub use error::RedfishClientCreationError;
use libredfish::Redfish;
use libredfish::model::service_root::RedfishVendor;

#[derive(Debug, Clone, Copy, PartialEq, Eq, LabelValue)]
enum DpuUefiPasswordSetupSkipReason {
    BiosAttributesMissing,
    BiosAttributesNotObject,
    CurrentUefiPasswordMissing,
}

/// UEFI password setup was skipped. Each variant is one reason, and picks the
/// diagnostic that reason already logged.
#[derive(Event)]
#[event(
    event_name = "dpu_uefi_password_setup_skipped",
    metric_name = "carbide_dpu_uefi_password_setup_skips_total",
    component = "carbide-redfish",
    metric = counter,
    describe = "Number of DPU UEFI password setup operations skipped, by reason.",
    labels(reason: DpuUefiPasswordSetupSkipReason),
)]
enum DpuUefiPasswordSetupSkipped {
    #[event(
        labels(reason = DpuUefiPasswordSetupSkipReason::BiosAttributesMissing),
        log = warn,
        message = "BIOS Attributes are missing in the Redfish System BIOS endpoint, skipping UEFI password setting"
    )]
    BiosAttributesMissing {},

    #[event(
        labels(reason = DpuUefiPasswordSetupSkipReason::BiosAttributesNotObject),
        log = warn,
        message = "BIOS attributes are not an object in the Redfish System BIOS endpoint, skipping UEFI password setting"
    )]
    BiosAttributesNotObject {},

    #[event(
        labels(reason = DpuUefiPasswordSetupSkipReason::CurrentUefiPasswordMissing),
        log = warn,
        message = "BIOS Attributes exist, but is missing CurrentUefiPassword key, skipping UEFI password setting"
    )]
    CurrentUefiPasswordMissing {},
}

/// Whether a DPU's BIOS attributes lack a UEFI password to set, emitting the
/// skip event with the reason when they do.
pub fn emit_dpu_uefi_password_setup_skipped_if_needed(
    bios_attrs: &std::collections::HashMap<String, serde_json::Value>,
    instrumentation: &Instrumentation,
) -> bool {
    let skipped = match bios_attrs.get("Attributes") {
        None => DpuUefiPasswordSetupSkipped::BiosAttributesMissing {},
        Some(attrs) => match attrs.as_object() {
            None => DpuUefiPasswordSetupSkipped::BiosAttributesNotObject {},
            Some(attrs) if !attrs.contains_key("CurrentUefiPassword") => {
                DpuUefiPasswordSetupSkipped::CurrentUefiPasswordMissing {}
            }
            Some(_) => return false,
        },
    };

    instrumentation.emit(skipped);
    true
}

/// The pool whose clients authenticate to the BMC itself rather than through
/// nico-bmc-proxy, which authenticates with the stored credentials. Only
/// [`new_pool_with_direct`] and test support construct one, so code that takes
/// this type cannot be handed a proxied pool. `BmcConnector` takes any pool; a
/// proxied one refuses its explicit-credential connections at runtime.
#[derive(Clone)]
pub struct DirectRedfishPool(Arc<dyn RedfishClientPool>);

impl DirectRedfishPool {
    /// The pool itself, for APIs that take any [`RedfishClientPool`].
    pub fn pool(&self) -> Arc<dyn RedfishClientPool> {
        self.0.clone()
    }
}

/// The general pool and its [`DirectRedfishPool`] handle, backed by one
/// object built from the raw `libredfish` pool.
/// RED metrics use `instrumentation`; retain its provider while clients can run.
pub fn new_pool_with_direct(
    credential_reader: Arc<dyn CredentialReader>,
    pool: libredfish::RedfishClientPool,
    proxy_address: Arc<ArcSwap<Option<HostPortPair>>>,
    instrumentation: Instrumentation,
) -> (Arc<dyn RedfishClientPool>, DirectRedfishPool) {
    let inner: Arc<dyn RedfishClientPool> = Arc::new(implementation::RedfishClientPoolImpl::new(
        credential_reader,
        pool,
        proxy_address,
        instrumentation,
    ));
    (inner.clone(), DirectRedfishPool(inner))
}

/// A pool whose clients all target nico-bmc-proxy: the proxy resolves the
/// BMC from the RFC 7239 `Forwarded` header and authenticates upstream
/// itself, so these clients carry no BMC credentials, and the pool rejects
/// `RedfishAuth::Direct` and non-443 BMC ports loudly. It is never a
/// [`DirectRedfishPool`]. `pool`
/// should be built with the client identity the proxy's mTLS listener
/// expects, and *without* `danger_accept_invalid_certs` -- the proxy,
/// unlike a BMC, presents a verifiable certificate.
/// RED metrics use `instrumentation`; retain its provider while clients can run.
pub fn new_proxied_pool(
    credential_reader: Arc<dyn CredentialReader>,
    pool: libredfish::RedfishClientPool,
    proxy: HostPortPair,
    instrumentation: Instrumentation,
) -> Arc<dyn RedfishClientPool> {
    Arc::new(implementation::ProxiedRedfishClientPoolImpl::new(
        credential_reader,
        pool,
        proxy,
        instrumentation,
    ))
}

/// Create Redfish clients for a certain Redfish BMC endpoint
#[async_trait]
pub trait RedfishClientPool: Send + Sync + 'static {
    // MARK: - Required methods

    /// Creates a new Redfish client for a Machines BMC.
    /// `host` is the IP address or hostname of the BMC.
    /// `vendor` allows you to pre-assign the underlying
    /// RedfishVendor to use for the client, saving the
    /// service root call to auto-detect the vendor.
    async fn create_client(
        &self,
        host: &str,
        port: Option<u16>,
        auth: RedfishAuth,
        vendor: Option<RedfishVendor>,
    ) -> Result<Box<dyn Redfish>, RedfishClientCreationError>;

    /// Creates a client whose libredfish transport does not repeat non-GET
    /// requests after network errors. GET retries and vendor-specific command
    /// sequences retain their behavior. Pools without this capability return
    /// `Unsupported`; they must never fall back to an ordinary retrying client.
    async fn create_client_without_mutation_retries(
        &self,
        _host: &str,
        _port: Option<u16>,
        _auth: RedfishAuth,
        _vendor: Option<RedfishVendor>,
    ) -> Result<Box<dyn Redfish>, RedfishClientCreationError> {
        Err(RedfishClientCreationError::Unsupported(
            "this Redfish pool cannot disable mutation retries".into(),
        ))
    }

    /// Returns a CredentialReader for use in setting credentials in the UEFI/BMC.
    fn credential_reader(&self) -> &dyn CredentialReader;

    // MARK: - Default (helper) methods

    async fn probe_redfish_endpoint(
        &self,
        bmc_ip_address: SocketAddr,
    ) -> Result<(), RedfishClientCreationError> {
        let client = self
            .create_client(
                &bmc_ip_address.ip().to_string(),
                Some(bmc_ip_address.port()),
                RedfishAuth::Anonymous,
                Some(RedfishVendor::Unknown),
            )
            .await?;

        client
            .get_service_root()
            .await
            .map_err(RedfishClientCreationError::RedfishError)?;

        Ok(())
    }

    async fn client_by_info(
        &self,
        access: &BmcAccessInfo,
    ) -> Result<Box<dyn Redfish>, RedfishClientCreationError> {
        self.create_client(
            &access.host,
            access.port,
            RedfishAuth::for_bmc_mac(access.mac_address),
            None,
        )
        .await
    }
}

/// Resolves a BMC's [`RedfishVendor`] via an anonymous service-root probe,
/// falling back to an authenticated `Chassis` manufacturer read for power
/// shelves (Lite-On / Delta) that omit `ServiceRoot.Vendor`.
pub async fn probe_bmc_vendor<P: RedfishClientPool + ?Sized>(
    pool: &P,
    host: &str,
    port: Option<u16>,
    credentials: Credentials,
) -> Result<RedfishVendor, RedfishClientCreationError> {
    // Anonymous service-root probe. An uninitialized `Unknown` client is
    // enough to read `/redfish/v1`, and works on factory BMCs that would
    // otherwise block reads until the password is rotated.
    let anon_client = pool
        .create_client(
            host,
            port,
            RedfishAuth::Anonymous,
            Some(RedfishVendor::Unknown),
        )
        .await?;

    if let Ok(service_root) = anon_client.get_service_root().await
        && let Some(vendor) = service_root.vendor()
        && vendor != RedfishVendor::Unknown
    {
        return Ok(vendor);
    }

    // Chassis `Manufacturer` fallback for BMCs (Lite-On / Delta power
    // shelves) that don't expose a recognized vendor in the service root.
    let Credentials::UsernamePassword { username, password } = credentials;
    let client = pool
        .create_client(host, port, RedfishAuth::Direct(username, password), None)
        .await?;

    let chassis_ids = client
        .get_chassis_all()
        .await
        .map_err(RedfishClientCreationError::RedfishError)?;
    for chassis_id in &chassis_ids {
        let chassis = client
            .get_chassis(chassis_id)
            .await
            .map_err(RedfishClientCreationError::RedfishError)?;
        if let Some(manufacturer) = chassis.manufacturer {
            let manufacturer_lc = manufacturer.to_lowercase();
            if manufacturer_lc.contains("lite-on") {
                return Ok(RedfishVendor::LiteOnPowerShelf);
            } else if manufacturer_lc.contains("delta") {
                return Ok(RedfishVendor::DeltaPowerShelf);
            }
        }
    }

    Err(RedfishClientCreationError::RedfishError(
        libredfish::RedfishError::MissingVendor,
    ))
}

// Some BMC implementations may return passwords in a response body that is
// later displayed to a user. This helper removes that exposure from
// password-related Redfish functions.
pub fn redact_password(err: libredfish::RedfishError, password: &str) -> libredfish::RedfishError {
    redact_passwords(err, &[password])
}

/// [`redact_password`] over several passwords at once, with union masking
/// (see [`carbide_utils::redfish::mask_all`]) so overlapping matches cannot
/// leave fragments of one password behind after another is replaced. JSON
/// response bodies are decoded first so escaped forms of a password are
/// covered as well.
pub fn redact_passwords(
    err: libredfish::RedfishError,
    passwords: &[&str],
) -> libredfish::RedfishError {
    type RfError = libredfish::RedfishError;
    let redact = |v: String| carbide_utils::redfish::mask_all(&v, passwords.iter().copied());
    let redact_response_body =
        |v: String| redact_redfish_response_body(&v, passwords.iter().copied());
    match err {
        RfError::HTTPErrorCode {
            url,
            status_code,
            response_body,
        } => RfError::HTTPErrorCode {
            url,
            status_code,
            response_body: redact_response_body(response_body),
        },
        RfError::JsonDeserializeError { url, body, source } => RfError::JsonDeserializeError {
            url,
            body: redact_response_body(body),
            source,
        },
        RfError::JsonSerializeError {
            url,
            object_debug,
            source,
        } => RfError::JsonSerializeError {
            url,
            object_debug: redact(object_debug),
            source,
        },
        RfError::InvalidValue {
            url,
            field,
            err: libredfish::model::InvalidValueError(v),
        } => RfError::InvalidValue {
            url,
            field,
            err: libredfish::model::InvalidValueError(redact(v)),
        },
        RfError::GenericError { error } => RfError::GenericError {
            error: redact(error),
        },
        // All errors are enumerated here instead of default to get
        // compile error on any new error in libredfish added. This
        // gives you chance to think if password may leak to the new
        // error or not.
        RfError::NetworkError { .. }
        | RfError::NoContent
        | RfError::NoHeader
        | RfError::Lockdown
        | RfError::MissingVendor
        | RfError::PasswordChangeRequired { .. }
        | RfError::FileError(_)
        | RfError::UserNotFound(_)
        | RfError::NotSupported(_)
        | RfError::UnnecessaryOperation
        | RfError::MissingKey { .. }
        | RfError::InvalidKeyType { .. }
        | RfError::TooManyUsers
        | RfError::NoDpu
        | RfError::ReqwestError(_)
        | RfError::TypeMismatch { .. }
        | RfError::MissingBootOption(_) => err,
    }
}

#[cfg(test)]
mod tests {
    use carbide_instrument::testing::{MetricsCapture, capture_logs};
    use carbide_secrets::credentials::{CredentialKey, CredentialType};
    use libredfish::PowerState;

    use super::*;
    use crate::libredfish::test_support::*;

    #[derive(Debug, PartialEq)]
    struct DpuUefiPasswordSetupObservation {
        skipped: bool,
        counter_deltas: [f64; 3],
        log_count: usize,
        level: Option<tracing::Level>,
        metadata_name: Option<String>,
        message: Option<String>,
        event_name: Option<String>,
        metric_name: Option<String>,
        reason: Option<String>,
    }

    #[test]
    fn dpu_uefi_password_setup_skips_only_incompatible_bios_attributes() {
        const EVENT_NAME: &str = "dpu_uefi_password_setup_skipped";
        const METRIC_NAME: &str = "carbide_dpu_uefi_password_setup_skips_total";

        carbide_test_support::value_scenarios!(
            run = |bios_attrs: serde_json::Value| {
                let bios_attrs: std::collections::HashMap<String, serde_json::Value> =
                    serde_json::from_value(bios_attrs).expect("valid test BIOS response");
                let metrics = MetricsCapture::owned();
                let mut skipped = false;
                let logs = capture_logs(|| {
                    skipped = emit_dpu_uefi_password_setup_skipped_if_needed(&bios_attrs,
                        metrics.instrumentation(),
                    );
                });
                let logs = logs
                    .into_iter()
                    .filter(|log| log.field("event_name") == Some(EVENT_NAME))
                    .collect::<Vec<_>>();
                let log = logs.first();

                DpuUefiPasswordSetupObservation {
                    skipped,
                    counter_deltas: [
                        metrics.counter_delta(
                            METRIC_NAME,
                            &[("reason", "bios_attributes_missing")],
                        ),
                        metrics.counter_delta(
                            METRIC_NAME,
                            &[("reason", "bios_attributes_not_object")],
                        ),
                        metrics.counter_delta(
                            METRIC_NAME,
                            &[("reason", "current_uefi_password_missing")],
                        ),
                    ],
                    log_count: logs.len(),
                    level: log.map(|log| log.level),
                    metadata_name: log.map(|log| log.metadata_name.clone()),
                    message: log.map(|log| log.message.clone()),
                    event_name: log
                        .and_then(|log| log.field("event_name"))
                        .map(str::to_string),
                    metric_name: log
                        .and_then(|log| log.field("metric_name"))
                        .map(str::to_string),
                    reason: log
                        .and_then(|log| log.field("reason"))
                        .map(str::to_string),
                }
            };
            "incompatible BIOS attributes skip password setup" {
                serde_json::json!({}) => DpuUefiPasswordSetupObservation {
                    skipped: true,
                    counter_deltas: [1.0, 0.0, 0.0],
                    log_count: 1,
                    level: Some(tracing::Level::WARN),
                    metadata_name: Some(EVENT_NAME.to_string()),
                    message: Some(
                        "BIOS Attributes are missing in the Redfish System BIOS endpoint, skipping UEFI password setting".to_string(),
                    ),
                    event_name: Some(EVENT_NAME.to_string()),
                    metric_name: Some(METRIC_NAME.to_string()),
                    reason: Some("bios_attributes_missing".to_string()),
                },
                serde_json::json!({"Attributes": []}) => DpuUefiPasswordSetupObservation {
                    skipped: true,
                    counter_deltas: [0.0, 1.0, 0.0],
                    log_count: 1,
                    level: Some(tracing::Level::WARN),
                    metadata_name: Some(EVENT_NAME.to_string()),
                    message: Some(
                        "BIOS attributes are not an object in the Redfish System BIOS endpoint, skipping UEFI password setting".to_string(),
                    ),
                    event_name: Some(EVENT_NAME.to_string()),
                    metric_name: Some(METRIC_NAME.to_string()),
                    reason: Some("bios_attributes_not_object".to_string()),
                },
                serde_json::json!({"Attributes": {"BootMode": "Uefi"}}) => DpuUefiPasswordSetupObservation {
                    skipped: true,
                    counter_deltas: [0.0, 0.0, 1.0],
                    log_count: 1,
                    level: Some(tracing::Level::WARN),
                    metadata_name: Some(EVENT_NAME.to_string()),
                    message: Some(
                        "BIOS Attributes exist, but is missing CurrentUefiPassword key, skipping UEFI password setting".to_string(),
                    ),
                    event_name: Some(EVENT_NAME.to_string()),
                    metric_name: Some(METRIC_NAME.to_string()),
                    reason: Some("current_uefi_password_missing".to_string()),
                },
            }

            "compatible BIOS attributes continue password setup" {
                serde_json::json!({"Attributes": {"CurrentUefiPassword": ""}}) => DpuUefiPasswordSetupObservation {
                    skipped: false,
                    counter_deltas: [0.0, 0.0, 0.0],
                    log_count: 0,
                    level: None,
                    metadata_name: None,
                    message: None,
                    event_name: None,
                    metric_name: None,
                    reason: None,
                },
            }
        );
    }

    /// The mask covers the union of ALL matches, including a needle's
    /// self-overlapping matches (`aa` occurs at both offsets in `xaaay`) and
    /// matches of different needles sharing a boundary -- so no fragment of
    /// any needle is left unmasked.
    #[test]
    fn mask_all_covers_overlapping_and_repeated_matches() {
        struct Case {
            name: &'static str,
            text: &'static str,
            needles: &'static [&'static str],
            expected: &'static str,
        }
        let cases = [
            Case {
                name: "self-overlapping needle",
                text: "xaaay",
                needles: &["aa"],
                expected: "xREDACTEDy",
            },
            Case {
                name: "boundary overlap between two needles",
                text: "rejected abcdefghi",
                needles: &["abcdef", "defghi"],
                expected: "rejected REDACTED",
            },
            Case {
                name: "containment",
                text: "rejected foobar, and foo separately",
                needles: &["foo", "foobar"],
                expected: "rejected REDACTED, and REDACTED separately",
            },
            Case {
                name: "disjoint matches and empty needles ignored",
                text: "a secret and a token",
                needles: &["secret", "token", ""],
                expected: "a REDACTED and a REDACTED",
            },
        ];
        for case in cases {
            assert_eq!(
                carbide_utils::redfish::mask_all(case.text, case.needles.iter().copied()),
                case.expected,
                "case: {}",
                case.name,
            );
        }
    }

    #[tokio::test]
    async fn test_power_state() {
        let sim = RedfishSim::default();
        let client = sim
            .create_client(
                "localhost",
                None,
                RedfishAuth::Key(CredentialKey::HostRedfish {
                    credential_type: CredentialType::SiteDefault,
                }),
                None,
            )
            .await
            .unwrap();

        assert_eq!(PowerState::On, client.get_power_state().await.unwrap());
        client
            .power(libredfish::SystemPowerControl::ForceOff)
            .await
            .unwrap();

        assert_eq!(PowerState::Off, client.get_power_state().await.unwrap());
        let client = sim
            .create_client(
                "localhost",
                None,
                RedfishAuth::Key(CredentialKey::HostRedfish {
                    credential_type: CredentialType::SiteDefault,
                }),
                None,
            )
            .await
            .unwrap();
        assert_eq!(PowerState::Off, client.get_power_state().await.unwrap());
    }

    #[test]
    fn password_redact_from_error() {
        const PASSWORD: &str = "1234";
        let err = libredfish::RedfishError::HTTPErrorCode {
            url: "https://example.com/redfish/v1/Systems/1/Bios/Actions/Bios.ChangePassword".into(),
            status_code: http::StatusCode::BAD_REQUEST,
            response_body: format!(r#""MessageArgs":["{PASSWORD}"]"#),
        };
        assert!(err.to_string().contains(PASSWORD));
        assert!(
            !redact_password(err, PASSWORD)
                .to_string()
                .contains(PASSWORD)
        );
    }

    #[test]
    fn password_redact_from_error_decodes_json_strings() {
        const PASSWORD: &str = "secret";
        let err = libredfish::RedfishError::HTTPErrorCode {
            url: "https://example.com/redfish/v1/Systems/1".into(),
            status_code: http::StatusCode::BAD_REQUEST,
            response_body: r#"{"error":{"message":"credential s\u0065cret rejected"}}"#.into(),
        };

        let redacted = redact_password(err, PASSWORD);
        let libredfish::RedfishError::HTTPErrorCode { response_body, .. } = redacted else {
            panic!("HTTP error remains an HTTP error after redaction");
        };
        let response: serde_json::Value =
            serde_json::from_str(&response_body).expect("redacted body remains valid JSON");
        assert_eq!(response["error"]["message"], "credential REDACTED rejected");
    }

    /// Verifies libredfish's local non-response masker removes the bare Basic
    /// payload even when the authentication scheme is normalized or omitted.
    #[test]
    fn password_redaction_masks_basic_payload_variants_in_local_errors() {
        // Derive the same complete redaction context retained by the direct
        // client, then echo its payload through a non-response error variant.
        let (authorization, sensitive_values) =
            carbide_utils::redfish::redfish_basic_authorization_context("root", Some("secret"));
        let payload = &sensitive_values[1];
        let error = libredfish::RedfishError::GenericError {
            error: format!(
                "exact {authorization}; lower basic {payload}; upper BASIC {payload}; bare {payload}"
            ),
        };
        let sensitive_values = sensitive_values
            .iter()
            .map(String::as_str)
            .collect::<Vec<_>>();

        // Run the local variant-aware sanitizer rather than the JSON response
        // path so both masking implementations protect the same credential.
        let redacted = redact_passwords(error, &sensitive_values);
        let libredfish::RedfishError::GenericError { error } = redacted else {
            panic!("generic error remains a generic error after redaction");
        };

        // The scheme is not itself secret, but no reusable Base64 payload may survive.
        assert_eq!(
            error,
            "exact REDACTED; lower basic REDACTED; upper BASIC REDACTED; bare REDACTED"
        );
    }

    #[tokio::test]
    async fn probe_bmc_vendor_resolves_from_service_root() {
        let sim = RedfishSim::default();
        let vendor = probe_bmc_vendor(&sim, "127.0.0.1", Some(443), Credentials::new("root", "pw"))
            .await
            .unwrap();
        // The sim's service root reports Nvidia / "GB200 NVL".
        assert_eq!(vendor, RedfishVendor::NvidiaGBx00);
    }

    #[tokio::test]
    async fn probe_bmc_vendor_falls_back_to_chassis_manufacturer() {
        for (manufacturer, expected) in [
            ("Lite-On Technology Corp.", RedfishVendor::LiteOnPowerShelf),
            ("Delta Electronics", RedfishVendor::DeltaPowerShelf),
        ] {
            let sim = RedfishSim::default();
            // Force the anonymous service-root probe to yield an unrecognized
            // vendor so probing falls through to the Chassis Manufacturer.
            sim.set_service_root_vendor(Some("Unrecognized Vendor".to_string()));
            sim.set_chassis_manufacturer(Some(manufacturer.to_string()));

            let vendor =
                probe_bmc_vendor(&sim, "127.0.0.1", Some(443), Credentials::new("root", "pw"))
                    .await
                    .unwrap();
            assert_eq!(
                vendor, expected,
                "chassis manufacturer {manufacturer} should resolve to {expected}",
            );
        }
    }

    #[tokio::test]
    async fn probe_bmc_vendor_errors_when_vendor_unresolvable() {
        let sim = RedfishSim::default();
        sim.set_service_root_vendor(Some("Unrecognized Vendor".to_string()));
        sim.set_chassis_manufacturer(Some("Acme".to_string()));

        let err = probe_bmc_vendor(&sim, "127.0.0.1", Some(443), Credentials::new("root", "pw"))
            .await
            .expect_err("an unrecognized vendor and chassis must error");

        assert!(
            matches!(
                err,
                RedfishClientCreationError::RedfishError(libredfish::RedfishError::MissingVendor)
            ),
            "expected MissingVendor, got {err:?}",
        );
    }

    /// A pool that implements only the required methods, delegating to a sim.
    struct UnadaptedPool(RedfishSim);

    #[async_trait]
    impl RedfishClientPool for UnadaptedPool {
        async fn create_client(
            &self,
            host: &str,
            port: Option<u16>,
            auth: RedfishAuth,
            vendor: Option<RedfishVendor>,
        ) -> Result<Box<dyn Redfish>, RedfishClientCreationError> {
            self.0.create_client(host, port, auth, vendor).await
        }

        fn credential_reader(&self) -> &dyn CredentialReader {
            self.0.credential_reader()
        }
    }

    /// An unadapted pool must reject the capability before creating an ordinary client.
    #[tokio::test]
    async fn mutation_retry_capability_defaults_to_unsupported() {
        let pool = UnadaptedPool(RedfishSim::default());
        let result = pool
            .create_client_without_mutation_retries("192.0.2.1", None, RedfishAuth::Anonymous, None)
            .await;
        assert!(matches!(
            result,
            Err(RedfishClientCreationError::Unsupported(_))
        ));
        assert!(pool.0.create_client_calls().is_empty());
    }
}
