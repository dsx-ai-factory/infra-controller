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
use std::path::Path;
use std::sync::Arc;

use carbide_authn::middleware::{ExternalUserInfo, Principal};
use carbide_uuid::machine::MachineId;

use crate::{CarbideError, CarbideResult};

mod casbin_engine;
mod internal_rbac_rules;
pub(crate) mod middleware;
pub(crate) mod mqtt_auth;
#[cfg(test)]
mod test_certs;

pub type AuthContext = carbide_authn::middleware::AuthContext<Authorization>;

/// `PrincipalClass` keeps the identity labels on authorization decisions
/// bounded. Both a normal denial and a permissive-mode override use the
/// strongest principal, so the two counters group callers the same way.
/// Variants stay weakest-first because the derived ordering selects that
/// strongest principal with `max`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, carbide_instrument::LabelValue)]
enum PrincipalClass {
    Anonymous,
    TrustedCertificate,
    SpiffeMachine,
    SpiffeService,
    ExternalUser,
}

impl PrincipalClass {
    fn classify(principals: &[Principal]) -> Self {
        principals
            .iter()
            .map(|principal| match principal {
                Principal::ExternalUser(_) => PrincipalClass::ExternalUser,
                Principal::SpiffeServiceIdentifier(_) => PrincipalClass::SpiffeService,
                Principal::SpiffeMachineIdentifier(_) => PrincipalClass::SpiffeMachine,
                Principal::TrustedCertificate => PrincipalClass::TrustedCertificate,
                Principal::Anonymous => PrincipalClass::Anonymous,
            })
            .max()
            // A request that presented no principals at all is anonymous.
            .unwrap_or(PrincipalClass::Anonymous)
    }
}

// An Authorization is sort of like a ticket that says we're allowed to do the
// thing we're trying to do, and specifically which Principal was permitted to
// do it.
#[derive(Clone, Debug)]
pub struct Authorization {
    _principal: Principal, // Currently unused
    _predicate: Predicate, // Currently unused
}

impl carbide_authn::middleware::Authorization for Authorization {}

/// Returns whether authentication resolved exactly one kind of caller identity.
///
/// A trusted client credential contributes [`Principal::TrustedCertificate`]
/// in addition to the concrete identity derived from that credential. That
/// marker is authentication evidence rather than a second caller for this
/// check. Every other principal must match `expected`, so combining an allowed
/// identity with a different service, machine, external user, or anonymous
/// principal cannot inherit the allowed identity's handler-level authority.
pub(crate) fn has_exclusive_identity(
    principals: &[Principal],
    expected: impl Fn(&Principal) -> bool,
) -> bool {
    let mut identity = None;
    for principal in principals {
        if matches!(principal, Principal::TrustedCertificate) {
            continue;
        }
        if !expected(principal) || identity.is_some_and(|identity| identity != principal) {
            return false;
        }
        identity = Some(principal);
    }
    identity.is_some()
}

#[derive(thiserror::Error, Debug, Clone)]
enum AuthorizationError {
    #[error("unauthorized: CasbinEngine: all auth principals denied by enforcer")]
    Unauthorized,
}

impl From<AuthorizationError> for tonic::Status {
    fn from(e: AuthorizationError) -> Self {
        tracing::info!(error = %e, "Request denied");
        tonic::Status::permission_denied("not authorized")
    }
}

/// `authenticated_machine_id` reads the machine identity from the request's
/// `AuthContext`, not its payload. Returns `None` when no machine identity is
/// available and `PermissionDeniedError` when the identity is malformed.
/// Callers that require a machine identity must reject `None`.
pub(crate) fn authenticated_machine_id<T>(
    request: &tonic::Request<T>,
) -> CarbideResult<Option<MachineId>> {
    request
        .extensions()
        .get::<AuthContext>()
        .and_then(AuthContext::get_spiffe_machine_id)
        .map(|machine_id| {
            machine_id.parse().map_err(|error| {
                CarbideError::PermissionDeniedError(format!(
                    "invalid authenticated machine identity: {error}"
                ))
            })
        })
        .transpose()
}

pub(crate) fn external_user_info<T>(
    request: &tonic::Request<T>,
) -> Result<ExternalUserInfo, CarbideError> {
    if let Some(external_user_info) = request
        .extensions()
        .get::<AuthContext>()
        .and_then(|auth_context| auth_context.get_external_user_info())
    {
        Ok(external_user_info.clone())
    } else {
        Err(CarbideError::ClientCertificateMissingInformation(
            "external user info".to_string(),
        ))
    }
}

// This is a "predicate" in the grammar sense of the word, so it's some sort of
// action that may or may not specify an object it's acting on.
#[derive(Clone, Debug)]
enum Predicate {
    // A call to a Forge-owned gRPC method. The string is the gRPC method name,
    // relative to the Forge service that contains it (i.e. without any slash
    // delimiters).
    ForgeCall(String),
}

trait PrincipalExtractor {
    // Extract all useful principals from a request.
    fn principals(&self) -> Vec<Principal>;
}

impl<T> PrincipalExtractor for tonic::Request<T> {
    fn principals(&self) -> Vec<Principal> {
        let _certs = self.peer_certs();
        // TODO: extract 1 or more Principal::CertIdentity from certs
        Vec::default()
    }
}

impl PrincipalExtractor for &[Principal] {
    fn principals(&self) -> Vec<Principal> {
        self.to_vec()
    }
}

// A PolicyEngine is anything that can enforce whether a request is allowed.
trait PolicyEngine {
    fn authorize(
        &self,
        principals: &[Principal],
        predicate: Predicate,
    ) -> Result<Authorization, AuthorizationError>;
}

type PolicyEngineObject = dyn PolicyEngine + Send + Sync;

#[derive(Clone)]
pub(crate) struct CasbinAuthorizer {
    policy_engine: Arc<PolicyEngineObject>,
}

impl CasbinAuthorizer {
    fn new(policy_engine: Arc<PolicyEngineObject>) -> Self {
        Self { policy_engine }
    }

    fn authorize<R: PrincipalExtractor>(
        &self,
        req: &R,
        predicate: Predicate,
    ) -> Result<Authorization, AuthorizationError> {
        let mut principals = req.principals();

        // We will also explicitly check anonymous to make the policy easier
        // to express.
        principals.push(Principal::Anonymous);

        let engine = self.policy_engine.clone();
        tracing::debug!(?principals, ?predicate, "Checking authorization");
        engine.authorize(&principals, predicate)
    }

    // TODO: config this out in release mode?
    fn enable_permissive(&mut self) {
        let inner_engine = self.policy_engine.clone();
        let permissive_engine: Arc<PolicyEngineObject> =
            Arc::new(PermissiveWrapper::new(inner_engine));
        self.policy_engine = permissive_engine;
    }

    pub(crate) async fn build_casbin(
        policy_path: &Path,
        permissive_mode: bool,
    ) -> Result<Self, CasbinAuthorizerError> {
        use casbin_engine::{CasbinEngine, ModelType};
        let engine = CasbinEngine::new(ModelType::Rbac, policy_path)
            .await
            .map_err(|e| CasbinAuthorizerError::InitializationError(e.to_string()))?;
        let engine_object: Arc<PolicyEngineObject> = Arc::new(engine);
        let mut authorizer = Self::new(engine_object);
        // TODO: config this out in release mode?
        if permissive_mode {
            authorizer.enable_permissive();
        }
        Ok(authorizer)
    }
}

#[derive(thiserror::Error, Clone, Debug)]
pub(crate) enum CasbinAuthorizerError {
    #[error("initialization error: {0}")]
    InitializationError(String),
}

/// `AuthorizationPermissiveOverride` records a policy denial that permissive
/// mode turns into a successful authorization. Concrete identities and the
/// requested method stay on the warning; only the bounded principal class
/// becomes a metric label.
#[derive(carbide_instrument::Event)]
#[event(
    event_name = "authorization_permissive_override",
    metric_name = "carbide_auth_permissive_overrides_total",
    component = "nico-api",
    log = warn,
    metric = counter,
    message = "The policy engine denied this request, but --auth-permissive-mode overrides it.",
    describe = "Number of policy denials overridden by authorization permissive mode, by principal class"
)]
struct AuthorizationPermissiveOverride {
    #[label]
    principal_class: PrincipalClass,
    #[context]
    principals: String,
    #[context]
    predicate: String,
    #[context]
    error: String,
}

struct PermissiveWrapper {
    inner: Arc<PolicyEngineObject>,
}

impl PermissiveWrapper {
    fn new(inner: Arc<PolicyEngineObject>) -> Self {
        Self { inner }
    }
}

impl PolicyEngine for PermissiveWrapper {
    fn authorize(
        &self,
        principals: &[Principal],
        predicate: Predicate,
    ) -> Result<Authorization, AuthorizationError> {
        let result = self.inner.authorize(principals, predicate.clone());
        result.or_else(|e| {
            carbide_instrument::emit(AuthorizationPermissiveOverride {
                principal_class: PrincipalClass::classify(principals),
                principals: principals
                    .iter()
                    .map(Principal::audit_identity)
                    .collect::<Vec<_>>()
                    .join(","),
                predicate: format!("{predicate:?}"),
                error: e.to_string(),
            });

            // FIXME: Strictly speaking, it's not true that Anonymous is
            // authorized to do this. Maybe define a different principal
            // to use here? "Development"?
            let authorization = Authorization {
                _principal: Principal::Anonymous,
                _predicate: predicate,
            };
            Ok(authorization)
        })
    }
}

#[cfg(test)]
mod tests {
    use std::borrow::Cow;
    use std::collections::HashMap;
    use std::io::BufRead;

    use carbide_authn::SpiffeContext;
    use carbide_authn::config::{AllowedCertCriteria, CertComponent};
    use carbide_authn::middleware::CertDescriptionMiddleware;
    use carbide_authn::spiffe_id::TrustDomain;
    use carbide_instrument::testing::{MetricsCapture, capture_logs};
    use eyre::Context;

    use super::*;

    #[test]
    fn exclusive_identity_ignores_only_the_trusted_certificate_marker() {
        let site_agent = || Principal::SpiffeServiceIdentifier("elektra-site-agent".to_string());
        let other_service = || Principal::SpiffeServiceIdentifier("nico-dns".to_string());
        let external_user = || {
            Principal::ExternalUser(ExternalUserInfo::new(
                None,
                "nico-cli-client".to_string(),
                None,
            ))
        };
        let is_site_agent = |principal: &Principal| {
            matches!(
                principal,
                Principal::SpiffeServiceIdentifier(identifier)
                    if identifier == "elektra-site-agent"
            )
        };

        for (name, principals, expected) in [
            ("site agent", vec![site_agent()], true),
            (
                "site agent mTLS",
                vec![site_agent(), Principal::TrustedCertificate],
                true,
            ),
            (
                "same identity from bearer and mTLS",
                vec![
                    site_agent(),
                    Principal::TrustedCertificate,
                    site_agent(),
                    Principal::TrustedCertificate,
                ],
                true,
            ),
            (
                "trusted certificate only",
                vec![Principal::TrustedCertificate],
                false,
            ),
            (
                "other service mTLS",
                vec![other_service(), Principal::TrustedCertificate],
                false,
            ),
            (
                "external user mTLS",
                vec![external_user(), Principal::TrustedCertificate],
                false,
            ),
            (
                "site agent plus external user",
                vec![site_agent(), Principal::TrustedCertificate, external_user()],
                false,
            ),
            (
                "machine bearer plus site agent mTLS",
                vec![
                    Principal::SpiffeMachineIdentifier("machine-1".to_string()),
                    Principal::TrustedCertificate,
                    site_agent(),
                    Principal::TrustedCertificate,
                ],
                false,
            ),
            (
                "site agent plus anonymous",
                vec![site_agent(), Principal::Anonymous],
                false,
            ),
            ("no principal", Vec::new(), false),
        ] {
            assert_eq!(
                has_exclusive_identity(&principals, is_site_agent),
                expected,
                "{name}"
            );
        }

        assert!(
            !has_exclusive_identity(
                &[
                    external_user(),
                    Principal::ExternalUser(ExternalUserInfo::new(
                        None,
                        "other-client".to_string(),
                        None,
                    )),
                    Principal::TrustedCertificate,
                ],
                |principal| matches!(principal, Principal::ExternalUser(_)),
            ),
            "different external users are distinct effective identities"
        );
    }

    /// A rejecting policy lets the permissive wrapper exercise the exact
    /// branch that production uses after Casbin denies a request.
    struct DenyAll;

    impl PolicyEngine for DenyAll {
        fn authorize(
            &self,
            _principals: &[Principal],
            _predicate: Predicate,
        ) -> Result<Authorization, AuthorizationError> {
            Err(AuthorizationError::Unauthorized)
        }
    }

    struct ClientCertTable {
        cert: Cow<'static, str>,
        desired: Principal,
    }

    /// A permissive override still authorizes the request, but one emit writes
    /// the existing warning and increments the caller's bounded metric series.
    #[test]
    fn permissive_override_logs_counts_and_authorizes() {
        let metrics = MetricsCapture::start();
        let wrapper = PermissiveWrapper::new(Arc::new(DenyAll));
        let principals = vec![
            Principal::SpiffeServiceIdentifier("scout".to_string()),
            Principal::TrustedCertificate,
        ];
        let mut authorized = false;

        let logs = capture_logs(|| {
            authorized = wrapper
                .authorize(
                    &principals,
                    Predicate::ForgeCall("PowerControl".to_string()),
                )
                .is_ok();
        });

        assert!(authorized, "permissive mode must still allow the request");
        let event = logs
            .iter()
            .find(|log| {
                log.metadata_name == "authorization_permissive_override"
                    && log.message
                        == "The policy engine denied this request, but \
                            --auth-permissive-mode overrides it."
            })
            .expect("the permissive override warning");
        assert_eq!(event.level, tracing::Level::WARN);
        assert_eq!(event.field("principal_class"), Some("spiffe_service"));
        assert_eq!(
            event.field("principals"),
            Some("spiffe-service-id/scout,trusted-certificate")
        );
        assert_eq!(
            event.field("predicate"),
            Some("ForgeCall(\"PowerControl\")")
        );
        assert_eq!(
            event.field("error"),
            Some("unauthorized: CasbinEngine: all auth principals denied by enforcer")
        );
        assert_eq!(
            metrics.counter_delta(
                "carbide_auth_permissive_overrides_total",
                &[("principal_class", "spiffe_service")]
            ),
            1.0
        );
    }

    #[test]
    fn test_try_from_client_certificates() -> Result<(), eyre::Error> {
        use super::test_certs::*;

        let mut table = vec![
            // Cert used by carbide-dhcp in local dev
            ClientCertTable {
                cert: CLIENT_CERT_DHCP.into(),
                desired: Principal::SpiffeServiceIdentifier("carbide-dhcp".to_string()),
            },
            // external cert (expired, of course)
            ClientCertTable {
                cert: CLIENT_CERT_EXTERNAL.into(),
                desired: Principal::ExternalUser(ExternalUserInfo::new(
                    Some("ExampleCo".to_string()),
                    "admins".to_string(),
                    Some("testuser".to_string()),
                )),
            },
            ClientCertTable {
                cert: CLIENT_CERT_MACHINEATRON.into(),
                desired: Principal::SpiffeServiceIdentifier("machine-a-tron".to_string()),
            },
            // Other app cert (signed by intermediate CA)
            ClientCertTable {
                cert: CLIENT_CERT_OTHER_APP.into(),
                desired: Principal::SpiffeServiceIdentifier("other-app".to_string()),
            },
            // Cert that gets used in CI/CD testing
            ClientCertTable {
                cert: CLIENT_CERT_CI.into(),
                desired: Principal::ExternalUser(ExternalUserInfo::new(
                    None,
                    "generic ci/cd".to_string(),
                    Some("ci-host.example.com".to_string()),
                )),
            },
        ];
        if let Some(extra) = extra_test_cert() {
            // Pull in an additional cert that would be a security problem to check in
            println!("Extra test cert: {:?}", extra.desired);
            table.push(extra);
        }
        let context: CertDescriptionMiddleware<Authorization> = CertDescriptionMiddleware::new(
            Some(AllowedCertCriteria {
                required_equals: HashMap::from([
                    (CertComponent::IssuerO, "ExampleCo".to_string()),
                    (
                        CertComponent::IssuerCN,
                        "Example Root Certificate Authority".to_string(),
                    ),
                ]),
                group_from: Some(CertComponent::SubjectOU),
                username_from: Some(CertComponent::SubjectCN),
                username: None,
            }),
            SpiffeContext {
                trust_domain: TrustDomain::new("example.test").unwrap(),
                service_base_paths: vec![
                    String::from("/carbide-system/sa/"),
                    String::from("/default/sa/"),
                    String::from("/other-namespace/sa/"),
                ],
                machine_base_path: String::from("/carbide-system/machine/"),
                additional_issuer_cns: ["usercert-ca.example.com".to_string()].into(),
            },
        );

        for test in table {
            let certs =
                rustls_pemfile::certs(&mut test.cert.as_bytes()).collect::<Result<Vec<_>, _>>()?;
            let certificate = certs.first().unwrap();
            assert_eq!(
                Principal::try_from_client_certificate(certificate, &context)
                    .wrap_err(format!("Bad certificate {}", test.cert))?,
                test.desired
            );
        }
        Ok(())
    }

    fn extra_test_cert() -> Option<ClientCertTable> {
        let cert = std::fs::read_to_string("/tmp/extra_test_cert.crt").ok()?;
        let principal_file = std::fs::File::open("/tmp/extra_test_cert.principal").ok()?;
        let mut principal_file = std::io::BufReader::new(principal_file);
        let mut line = String::new();
        principal_file.read_line(&mut line).ok()?;
        match line.as_str() {
            "SpiffeServiceIdentifier\n" => {
                let mut line = String::new();
                principal_file.read_line(&mut line).ok()?;
                if let Some(stripped) = line.strip_suffix("\n") {
                    line = stripped.to_string();
                }
                Some(ClientCertTable {
                    cert: cert.into(),
                    desired: Principal::SpiffeServiceIdentifier(line),
                })
            }
            _ => None,
        }
    }
}
