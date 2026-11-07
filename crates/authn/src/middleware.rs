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
use std::net::SocketAddr;
use std::sync::Arc;

use hyper::Request;
use tonic::transport::CertificateDer;
use tower::{Layer, Service};
use x509_parser::asn1_rs::{Oid, PrintableString};
use x509_parser::oid_registry;
use x509_parser::prelude::{FromDer, X509Certificate, X509Name};

use crate::config::{AllowedCertCriteria, CertComponent};
use crate::{SpiffeContext, SpiffeError};

// A middleware layer to deal with per-request authentication.
// This might mean extracting a service identifier from a SPIFFE x509
// certificate (in which case most of the heavy lifting has already been done by
// the TLS verifier), validating a JWT, validating a TPM signature, or any other
// similar mechanism.
//
// This middleware is not expected to enforce anything on its own, so anything
// that an access control policy might need to do its work should be passed
// along in the request extensions.
/// Verifies a bearer token (a node-auth JWT) and, on success, returns the
/// validated SPIFFE ID URI string from the token's `sub` claim. The
/// implementation lives outside this crate (api-core's `node_auth`) so the
/// authn layer stays free of JWT signing/key-management concerns.
///
/// Returning the raw SPIFFE URI — rather than a `Principal` — lets the
/// middleware map it through the SAME [`SpiffeContext`] used for client certs,
/// so a JWT and an mTLS cert for the same machine yield identical principals.
pub trait BearerTokenAuthenticator: Send + Sync {
    /// Returns the validated SPIFFE ID URI if `token` is a valid node-auth JWT,
    /// or `None` if it is invalid/expired/unrecognized.
    fn spiffe_id_from_bearer(&self, token: &str) -> Option<String>;
}

#[derive(Clone)]
pub struct CertDescriptionMiddleware<AZ: Authorization> {
    pub spiffe_context: Arc<SpiffeContext>,
    pub extra_allowed_certs: Option<AllowedCertCriteria>,
    /// Optional bearer-token (JWT) authenticator. When set, requests carrying an
    /// `Authorization: Bearer <jwt>` header gain principals alongside any from
    /// mTLS client certs (dual-support during the mTLS→JWT migration).
    pub bearer_authenticator: Option<Arc<dyn BearerTokenAuthenticator>>,
    /// Whether machine mTLS client certificates mint machine principals. On by
    /// default; disabled via `[node_auth] mtls_enabled = false` once a fleet has
    /// migrated to bearer tokens. Scoped to machine certs — service and admin
    /// client certs are unaffected.
    pub machine_certs_enabled: bool,
    _authorization: std::marker::PhantomData<AZ>,
}

impl<AZ: Authorization> CertDescriptionMiddleware<AZ> {
    pub fn new(
        extra_allowed_certs: Option<AllowedCertCriteria>,
        spiffe_context: SpiffeContext,
    ) -> Self {
        CertDescriptionMiddleware {
            spiffe_context: Arc::new(spiffe_context),
            extra_allowed_certs,
            bearer_authenticator: None,
            machine_certs_enabled: true,
            _authorization: std::marker::PhantomData,
        }
    }

    /// Enables bearer-token (JWT) authentication using the given authenticator.
    #[must_use]
    pub fn with_bearer_authenticator(
        mut self,
        authenticator: Arc<dyn BearerTokenAuthenticator>,
    ) -> Self {
        self.bearer_authenticator = Some(authenticator);
        self
    }

    /// Controls whether machine mTLS client certificates are accepted as node
    /// identity (`[node_auth] mtls_enabled`).
    #[must_use]
    pub fn with_machine_certs_enabled(mut self, enabled: bool) -> Self {
        self.machine_certs_enabled = enabled;
        self
    }
}

impl<S, AZ: Authorization> Layer<S> for CertDescriptionMiddleware<AZ> {
    type Service = CertDescriptionService<S, AZ>;

    fn layer(&self, inner: S) -> Self::Service {
        CertDescriptionService {
            inner,
            authorization_context: Arc::new(self.clone()),
        }
    }
}

#[derive(Clone)]
pub struct CertDescriptionService<S, AZ: Authorization> {
    inner: S,
    authorization_context: Arc<CertDescriptionMiddleware<AZ>>,
}

// This is added to the extensions of a request. The authentication (authn)
// middleware populates the `principals` field, and the authorization (authz)
// middleware sets the `authorization` field.
#[derive(Clone)]
pub struct AuthContext<AZ: Authorization> {
    pub principals: Vec<Principal>,
    pub authorization: Option<AZ>,
}

/// Clients may want to include Authorization information (distinct from "authentication" which this
/// crate provides) in the AuthContext. This trait allows clients to tag a type as containing
/// Authorization info, so that it can work as the generic type for AuthContext<T>.
pub trait Authorization: Clone + Send + Sync + 'static {}

pub type NoAuthorization = ();
impl Authorization for NoAuthorization {}

// Various properties of a user gleaned from the presented certificate
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct ExternalUserInfo {
    // Organization of the user, currently unused except for reporting
    pub org: Option<String>,
    // Group of the user, which determines their permissions
    pub group: String,
    // Name of the user, used as identifier in applying redfish changes.
    pub user: Option<String>,
}

impl ExternalUserInfo {
    pub fn new(org: Option<String>, group: String, user: Option<String>) -> Self {
        Self { org, group, user }
    }
}

// Principal: something like an account, service, address, or other
// identity that we can treat as the "subject" in a subject-action-object
// construction.
#[derive(Clone, Debug, PartialEq)]
pub enum Principal {
    // A SPIFFE ID after the trust domain and base path have been removed.
    SpiffeServiceIdentifier(String),
    SpiffeMachineIdentifier(String),

    // Certificate based authentication from outside of the cluster
    ExternalUser(ExternalUserInfo),

    // Any certificate that was trusted by the TLS acceptor. This is a superset
    // of what gets mapped into the SPIFFE or external principals, so any request
    // with one of those will also have one of these (but not necessarily the
    // other way around).
    TrustedCertificate,

    // JWT(Claims),
    // ClientAddress(IPAddr),

    // Anonymous is more like the absence of any principal, but it's convenient
    // to be able to represent it explicitly.
    Anonymous,
}

impl Principal {
    pub fn as_identifier(&self) -> String {
        match self {
            Principal::SpiffeServiceIdentifier(identifier) => {
                format!("spiffe-service-id/{identifier}")
            }
            Principal::SpiffeMachineIdentifier(_identifier) => {
                // We don't care so much about the specific machine id, but we
                // do want to grant permissions to machines as a class.
                "spiffe-machine-id".into()
            }
            Principal::ExternalUser(info) => {
                format!("external-role/{}", info.group)
            }
            Principal::TrustedCertificate => "trusted-certificate".into(),
            Principal::Anonymous => "anonymous".into(),
        }
    }

    /// The audit-log rendering of this principal: like [`Self::as_identifier`]
    /// but keeping the machine's concrete SPIFFE identity -- an audit line
    /// must say which machine was denied, while authorization only cares that
    /// it is a machine. External-user payloads stay redacted to the group.
    pub fn audit_identity(&self) -> String {
        match self {
            Principal::SpiffeMachineIdentifier(identifier) => {
                format!("spiffe-machine-id/{identifier}")
            }
            other => other.as_identifier(),
        }
    }

    // Note: no certificate verification is performed here!
    pub fn try_from_client_certificate<AZ: Authorization>(
        certificate: &CertificateDer,
        auth_context: &CertDescriptionMiddleware<AZ>,
    ) -> Result<Principal, SpiffeError> {
        match crate::validate_x509_certificate(certificate.as_ref()) {
            Ok(spiffe_id) => {
                let service_id = auth_context
                    .spiffe_context
                    .extract_service_identifier(&spiffe_id)?;
                Ok(match service_id {
                    crate::SpiffeIdClass::Service(service_id) => {
                        Principal::SpiffeServiceIdentifier(service_id)
                    }
                    crate::SpiffeIdClass::Machine(machine_id) => {
                        Principal::SpiffeMachineIdentifier(machine_id)
                    }
                })
            }
            Err(e) => {
                // external certs do not include a SPIFFE ID, check if we might be one of them
                if let Some(external_cert) = try_external_cert(certificate.as_ref(), auth_context) {
                    return Ok(external_cert);
                }
                Err(SpiffeError::Validation(e))
            }
        }
    }

    pub fn is_proper_subset_of(&self, other: &Self) -> bool {
        match other {
            Principal::SpiffeServiceIdentifier(id_other) => match self {
                Principal::SpiffeServiceIdentifier(id_self) => id_self == id_other,
                _ => false,
            },
            Principal::SpiffeMachineIdentifier(_) => {
                matches!(self, Principal::SpiffeMachineIdentifier(_))
            }
            Principal::ExternalUser(_) => {
                matches!(self, Principal::ExternalUser(_))
            }
            Principal::TrustedCertificate => {
                matches!(self, Principal::TrustedCertificate)
            }
            Principal::Anonymous => true,
        }
    }

    pub fn from_web_cookie(user: String, group: String) -> Self {
        Principal::ExternalUser(ExternalUserInfo::new(None, group, Some(user)))
    }
}

/// Extracts the token from an `Authorization: Bearer <token>` header, if present.
///
/// RFC 6750 spells the scheme with an ABNF quoted literal, which RFC 5234
/// makes case-insensitive, and allows `1*SP` before the token — so `bearer`
/// and extra spaces are both legal and must be accepted. Our own clients only
/// ever send `Bearer <jwt>`, but a third-party one that doesn't would
/// otherwise be turned away with nothing written anywhere, so the reject path
/// logs. Neither branch logs the header value: it is a live credential.
fn bearer_token_from_headers(headers: &hyper::HeaderMap) -> Option<&str> {
    let header = headers.get(hyper::header::AUTHORIZATION)?;
    let Ok(value) = header.to_str() else {
        tracing::debug!(
            target: "node_auth",
            "node-auth: ignoring an Authorization header that is not valid UTF-8"
        );
        return None;
    };

    match value.split_once(' ') {
        Some((scheme, token)) if scheme.eq_ignore_ascii_case("Bearer") => Some(token.trim()),
        _ => {
            tracing::debug!(
                target: "node_auth",
                "node-auth: ignoring an Authorization header that is not a Bearer credential"
            );
            None
        }
    }
}

// try_external_cert will return a Pricipal::ExternalUser if this looks like some external cert
fn try_external_cert<AZ: Authorization>(
    der_certificate: &[u8],
    auth_context: &CertDescriptionMiddleware<AZ>,
) -> Option<Principal> {
    if let Ok((_remainder, x509_cert)) = X509Certificate::from_der(der_certificate) {
        // Looks through the issuer relative distinguished names for a CN matching what we expect for external certs.
        // Other options may be available in the future, but just this for now.
        for rdn in x509_cert.issuer().iter() {
            if rdn
                .iter()
                .filter(|attribute| attribute.attr_type() == &oid_registry::OID_X509_COMMON_NAME) // CN=  see https://www.rfc-editor.org/rfc/rfc4519.html
                .filter_map(|attribute| attribute.attr_value().as_printablestring().ok())
                .any(|value| {
                    auth_context
                        .spiffe_context
                        .additional_issuer_cns
                        .contains(value.as_ref())
                })
            {
                // This CN is what we expect from external certs
                return Some(Principal::ExternalUser(parse_org_group_user_from_subject(
                    x509_cert.subject(),
                )));
            }
        }

        if let Some(allowed_certs) = &auth_context.extra_allowed_certs {
            return site_allowed_cert(&x509_cert, allowed_certs);
        }
    }
    None
}

// Checks if the given cert is an acceptable user based on per site criteria
pub fn site_allowed_cert(
    cert: &X509Certificate,
    criteria: &AllowedCertCriteria,
) -> Option<Principal> {
    for rdn in cert.issuer().iter() {
        if rdn.iter().any(|attribute| {
            if let Some(component) = cert_component_from_oid_issuer(attribute.attr_type().clone()) {
                if let Some(required_value) = criteria.required_equals.get(&component) {
                    attribute
                        .attr_value()
                        .as_printablestring()
                        .ok()
                        .unwrap_or(PrintableString::new(""))
                        .string()
                        != required_value.clone()
                } else {
                    false
                }
            } else {
                false
            }
        }) {
            // Something didn't match
            return None;
        }
    }
    let mut group = "".to_string();
    let mut username_from_cert = None;
    for rdn in cert.subject().iter() {
        if rdn.iter().any(|attribute| {
            if let Some(component) = cert_component_from_oid_subject(attribute.attr_type().clone())
            {
                if criteria.group_from == Some(component.clone()) {
                    group = attribute
                        .attr_value()
                        .as_printablestring()
                        .ok()
                        .unwrap_or(PrintableString::new(""))
                        .string();
                }
                if criteria.username_from == Some(component.clone()) {
                    username_from_cert = Some(
                        attribute
                            .attr_value()
                            .as_printablestring()
                            .ok()
                            .unwrap_or(PrintableString::new(""))
                            .string(),
                    );
                }
                if let Some(required_value) = criteria.required_equals.get(&component) {
                    attribute
                        .attr_value()
                        .as_printablestring()
                        .ok()
                        .unwrap_or(PrintableString::new(""))
                        .string()
                        != required_value.clone()
                } else {
                    false
                }
            } else {
                false
            }
        }) {
            // Something didn't match
            return None;
        }
    }
    if criteria.username_from.is_some() && username_from_cert.is_some() {
        Some(Principal::ExternalUser(ExternalUserInfo {
            org: None,
            group,
            user: username_from_cert,
        }))
    } else if let Some(username) = &criteria.username {
        Some(Principal::ExternalUser(ExternalUserInfo {
            org: None,
            group,
            user: Some(username.clone()),
        }))
    } else {
        Some(Principal::ExternalUser(ExternalUserInfo {
            org: None,
            group,
            user: None,
        }))
    }
}

// Get the O=, OU=, and CN= values from a certificate subject
fn parse_org_group_user_from_subject(subject: &X509Name) -> ExternalUserInfo {
    let mut org = None;
    let mut group = "".to_string();
    let mut user = None;

    for rdn in subject.iter() {
        for attribute in rdn.iter() {
            match attribute.attr_type() {
                x if x == &oid_registry::OID_X509_ORGANIZATION_NAME => {
                    if let Ok(value) = attribute.attr_value().as_printablestring() {
                        org = Some(value.string());
                    }
                }
                x if x == &oid_registry::OID_X509_ORGANIZATIONAL_UNIT => {
                    if let Ok(value) = attribute.attr_value().as_printablestring() {
                        group = value.string();
                    }
                }
                x if x == &oid_registry::OID_X509_COMMON_NAME => {
                    if let Ok(value) = attribute.attr_value().as_printablestring() {
                        user = Some(value.string());
                    }
                }
                _ => {}
            };
        }
    }

    ExternalUserInfo::new(org, group, user)
}

// Finds the CertComponent for the given ASN1 OID, given that this is coming from the issuer.
fn cert_component_from_oid_issuer(oid: Oid) -> Option<CertComponent> {
    // Lack of implementation in oid_registry means we can't use match here
    if oid == oid_registry::OID_X509_ORGANIZATION_NAME {
        Some(CertComponent::IssuerO)
    } else if oid == oid_registry::OID_X509_ORGANIZATIONAL_UNIT {
        Some(CertComponent::IssuerOU)
    } else if oid == oid_registry::OID_X509_COMMON_NAME {
        Some(CertComponent::IssuerCN)
    } else {
        None
    }
}

// Finds the CertComponent for the given ASN1 OID, given that this is coming from the subject.
fn cert_component_from_oid_subject(oid: Oid) -> Option<CertComponent> {
    // Lack of implementation in oid_registry means we can't use match here
    if oid == oid_registry::OID_X509_ORGANIZATION_NAME {
        Some(CertComponent::SubjectO)
    } else if oid == oid_registry::OID_X509_ORGANIZATIONAL_UNIT {
        Some(CertComponent::SubjectOU)
    } else if oid == oid_registry::OID_X509_COMMON_NAME {
        Some(CertComponent::SubjectCN)
    } else {
        None
    }
}

impl<T: Authorization> AuthContext<T> {
    pub fn get_spiffe_machine_id(&self) -> Option<&str> {
        self.principals.iter().find_map(|p| match p {
            Principal::SpiffeMachineIdentifier(identifier) => Some(identifier.as_str()),
            _ => None,
        })
    }

    pub fn get_spiffe_service_id(&self) -> Option<&str> {
        self.principals.iter().find_map(|p| match p {
            Principal::SpiffeServiceIdentifier(identifier) => Some(identifier.as_str()),
            _ => None,
        })
    }

    pub fn get_external_user_info(&self) -> Option<&ExternalUserInfo> {
        self.principals.iter().find_map(|p| match p {
            Principal::ExternalUser(external_user_info)
                if external_user_info
                    .user
                    .as_ref()
                    .is_some_and(|u| !u.is_empty()) =>
            {
                Some(external_user_info)
            }
            _ => None,
        })
    }

    pub fn get_external_user_name(&self) -> Option<&str> {
        self.principals.iter().find_map(|p| match p {
            Principal::ExternalUser(external_user_info) => external_user_info
                .user
                .as_ref()
                .filter(|x| !x.is_empty())
                .map(|x| x.as_str()),
            _ => None,
        })
    }
}

impl<T: Authorization> Default for AuthContext<T> {
    fn default() -> Self {
        // We'll probably only ever see 1-2 principals associated with a request.
        let principals = Vec::with_capacity(4);
        let authorization = None;
        AuthContext {
            principals,
            authorization,
        }
    }
}

// This is used as an extension to requests for anything that is an attribute of
// the connection the request came in on, as opposed to the HTTP request itself.
// Note that if you're trying to retrieve it, it's probably inside an Arc in the
// extensions typemap, so .get::<Arc<ConnectionAttributes>>() is what you want.
pub struct ConnectionAttributes {
    pub peer_address: SocketAddr,
    /// As the client sent them: TLS verified only the first.
    pub peer_certificates: Vec<CertificateDer<'static>>,
}

/// `AuthenticationConnectionAttributesMissing` records a request that reached
/// authentication without the connection metadata installed by the listener.
/// We still insert an empty `AuthContext` and delegate, leaving authorization
/// to fail closed according to its existing rules.
#[derive(carbide_instrument::Event)]
#[event(
    event_name = "authentication_connection_attributes_missing",
    metric_name = "carbide_authn_connection_attributes_missing_total",
    component = "authn",
    log = warn,
    metric = counter,
    message = "No ConnectionAttributes in request extensions!",
    describe = "Number of requests authentication could not inspect because connection attributes were missing"
)]
struct AuthenticationConnectionAttributesMissing;

/// A request whose end-entity certificate failed to map to a principal. The
/// peer and the error ride the DEBUG log line.
#[derive(carbide_instrument::Event)]
#[event(
    event_name = "client_cert_rejected",
    metric_name = "carbide_authn_client_cert_rejected_total",
    component = "authn",
    log = debug,
    metric = counter,
    message = "Rejected a client certificate",
    describe = "Number of client certificates rejected during authentication"
)]
struct ClientCertRejected {
    #[label]
    reason: RejectReason,
    #[context]
    peer_address: SocketAddr,
    #[context]
    error: String,
}

/// Which stage of certificate-to-principal mapping rejected the certificate,
/// mirroring [`SpiffeError`]'s variants.
#[derive(Debug, Clone, Copy, PartialEq, Eq, carbide_instrument::LabelValue)]
enum RejectReason {
    Validation,
    Recognition,
}

impl From<&SpiffeError> for RejectReason {
    fn from(error: &SpiffeError) -> Self {
        match error {
            SpiffeError::Validation(_) => RejectReason::Validation,
            SpiffeError::Recognition(_) => RejectReason::Recognition,
        }
    }
}

impl<S, B, AZ> Service<Request<B>> for CertDescriptionService<S, AZ>
where
    B: tonic::codegen::Body,
    S: Service<Request<B>>,
    AZ: Authorization,
{
    type Response = S::Response;
    type Error = S::Error;
    type Future = S::Future;

    fn poll_ready(
        &mut self,
        cx: &mut std::task::Context<'_>,
    ) -> std::task::Poll<Result<(), Self::Error>> {
        self.inner.poll_ready(cx)
    }

    fn call(&mut self, mut request: Request<B>) -> Self::Future {
        let mut auth_context = AuthContext::<AZ>::default();

        // Bearer-token (node-auth JWT) authentication. Validated tokens map
        // through the same SpiffeContext as client certs. A recognized bearer
        // credential also represents a trusted identity for authorization: it
        // was signed by the node's certificate and validated by the configured
        // authenticator, so it needs the same Casbin grant as mTLS. This runs
        // in addition to (not instead of) cert auth to support the migration.
        if let Some(authenticator) = &self.authorization_context.bearer_authenticator
            && let Some(token) = bearer_token_from_headers(request.headers())
            && let Some(spiffe_uri) = authenticator.spiffe_id_from_bearer(token)
        {
            let principal = match crate::spiffe_id::SpiffeId::new(&spiffe_uri) {
                Ok(spiffe_id) => match self
                    .authorization_context
                    .spiffe_context
                    .extract_service_identifier(&spiffe_id)
                {
                    Ok(crate::SpiffeIdClass::Service(id)) => {
                        Some(Principal::SpiffeServiceIdentifier(id))
                    }
                    Ok(crate::SpiffeIdClass::Machine(id)) => {
                        Some(Principal::SpiffeMachineIdentifier(id))
                    }
                    Err(e) => {
                        tracing::debug!(
                            target: "node_auth",
                            error = %e,
                            "node-auth: bearer token SPIFFE id not recognized"
                        );
                        None
                    }
                },
                Err(e) => {
                    tracing::debug!(
                        target: "node_auth",
                        error = %e,
                        "node-auth: bearer token contained an unparsable SPIFFE URI"
                    );
                    None
                }
            };
            if let Some(principal) = principal {
                auth_context.principals.push(principal);
                auth_context.principals.push(Principal::TrustedCertificate);
            }
        }

        let extensions = request.extensions_mut();
        if let Some(conn_attrs) = extensions.get::<Arc<ConnectionAttributes>>() {
            // TLS verified only the end-entity certificate; the client can
            // append anything after it.
            if let Some(end_entity) = conn_attrs.peer_certificates.first() {
                let refused_machine_cert = match Principal::try_from_client_certificate(
                    end_entity,
                    &self.authorization_context,
                ) {
                    // `[node_auth] mtls_enabled = false`: machine certs no
                    // longer grant node identity (bearer JWTs are the only node
                    // auth path); service/admin cert principals pass through.
                    Ok(Principal::SpiffeMachineIdentifier(_))
                        if !self.authorization_context.machine_certs_enabled =>
                    {
                        tracing::debug!(
                            target: "node_auth",
                            "node-auth: machine mTLS authentication disabled; ignoring machine client certificate"
                        );
                        true
                    }
                    Ok(principal) => {
                        auth_context.principals.push(principal);
                        false
                    }
                    Err(error) => {
                        carbide_instrument::emit(ClientCertRejected {
                            reason: RejectReason::from(&error),
                            peer_address: conn_attrs.peer_address,
                            error: error.to_string(),
                        });
                        false
                    }
                };
                // Not for a refused machine certificate: the shipped Casbin
                // policy grants `TrustedCertificate` `forge/*` and `nico/*`,
                // which would undo the refusal.
                if !refused_machine_cert {
                    auth_context.principals.push(Principal::TrustedCertificate);
                }
            }
        } else {
            carbide_instrument::emit(AuthenticationConnectionAttributesMissing);
        }

        extensions.insert(auth_context);
        self.inner.call(request)
    }
}

#[cfg(test)]
mod tests {
    use std::collections::HashSet;
    use std::convert::Infallible;
    use std::sync::Mutex;
    use std::task::{Context, Poll};

    use carbide_instrument::testing::{MetricsCapture, capture_logs};
    use carbide_test_support::Outcome::Yields;
    use carbide_test_support::{Case, check_cases_async};
    use hyper::header::AUTHORIZATION;
    use tower::{Layer, ServiceExt};

    use super::*;
    use crate::spiffe_id::TrustDomain;
    use crate::{SpiffeContextError, SpiffeValidationError};

    /// A terminal service for driving the middleware. The middleware does all
    /// of its certificate work synchronously inside `call` before delegating
    /// here, so the test never needs to poll the returned future.
    #[derive(Clone)]
    struct Terminal;

    impl<B> Service<Request<B>> for Terminal {
        type Response = ();
        type Error = std::convert::Infallible;
        type Future = std::future::Ready<Result<(), Self::Error>>;

        fn poll_ready(&mut self, _cx: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
            Poll::Ready(Ok(()))
        }

        fn call(&mut self, _request: Request<B>) -> Self::Future {
            std::future::ready(Ok(()))
        }
    }

    /// `AuthContextRecorder` captures what authentication passed to the inner
    /// service, which lets the missing-metadata test verify that the warning
    /// does not stop or bypass the rest of the middleware chain.
    #[derive(Clone, Default)]
    struct AuthContextRecorder {
        principals: Arc<Mutex<Option<Vec<Principal>>>>,
    }

    impl AuthContextRecorder {
        fn principals(&self) -> Option<Vec<Principal>> {
            self.principals
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner())
                .clone()
        }
    }

    impl<B> Service<Request<B>> for AuthContextRecorder {
        type Response = ();
        type Error = std::convert::Infallible;
        type Future = std::future::Ready<Result<(), Self::Error>>;

        fn poll_ready(&mut self, _cx: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
            Poll::Ready(Ok(()))
        }

        fn call(&mut self, request: Request<B>) -> Self::Future {
            let principals = request
                .extensions()
                .get::<AuthContext<NoAuthorization>>()
                .map(|context| context.principals.clone());
            *self
                .principals
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner()) = principals;
            std::future::ready(Ok(()))
        }
    }

    /// Test authenticator: maps the literal token "good" to a fixed SPIFFE URI.
    struct FakeAuth(String);
    impl BearerTokenAuthenticator for FakeAuth {
        fn spiffe_id_from_bearer(&self, token: &str) -> Option<String> {
            (token == "good").then(|| self.0.clone())
        }
    }

    fn spiffe_context() -> SpiffeContext {
        SpiffeContext {
            trust_domain: TrustDomain::new("example.test").expect("trust domain"),
            service_base_paths: vec!["/carbide-system/sa/".to_string()],
            machine_base_path: "/carbide-system/machine/".to_string(),
            additional_issuer_cns: HashSet::new(),
        }
    }

    /// A self-signed leaf whose URI SAN is a service SPIFFE id under the test
    /// trust domain: `try_from_client_certificate` performs no signature
    /// verification, so this maps to a real principal.
    fn spiffe_leaf_certificate(path: &str) -> CertificateDer<'static> {
        let mut params = rcgen::CertificateParams::default();
        params.subject_alt_names = vec![rcgen::SanType::URI(
            rcgen::string::Ia5String::try_from(format!("spiffe://example.test{path}"))
                .expect("uri"),
        )];
        let key = rcgen::KeyPair::generate().expect("key pair");
        params.self_signed(&key).expect("certificate").der().clone()
    }

    /// Missing `ConnectionAttributes` is a wiring error, but authentication
    /// still installs an empty `AuthContext` before calling the inner service.
    /// The Event records that single request without also counting a rejected
    /// certificate.
    #[test]
    fn missing_connection_attributes_logs_counts_and_delegates() {
        let metrics = MetricsCapture::start();
        let recorder = AuthContextRecorder::default();
        let middleware = CertDescriptionMiddleware::<NoAuthorization>::new(None, spiffe_context());
        let mut service = middleware.layer(recorder.clone());

        let logs = capture_logs(|| {
            let request = Request::builder()
                .uri(rpc::service_path!("Anything"))
                .body(String::new())
                .expect("request");
            let _response_future = service.call(request);
        });

        let event = logs
            .iter()
            .find(|log| log.metadata_name == "authentication_connection_attributes_missing")
            .expect("the missing connection attributes warning");
        assert_eq!(event.level, tracing::Level::WARN);
        assert_eq!(
            event.message,
            "No ConnectionAttributes in request extensions!"
        );
        assert_eq!(
            event.field("event_name"),
            Some("authentication_connection_attributes_missing")
        );
        assert_eq!(
            event.field("metric_name"),
            Some("carbide_authn_connection_attributes_missing_total")
        );
        assert_eq!(recorder.principals(), Some(Vec::new()));
        assert_eq!(
            metrics.counter_delta("carbide_authn_connection_attributes_missing_total", &[]),
            1.0
        );
        assert_eq!(
            metrics.counter_delta(
                "carbide_authn_client_cert_rejected_total",
                &[("reason", "validation")]
            ),
            0.0
        );
    }

    /// A healthy chain -- a mapping leaf plus a non-mapping second
    /// certificate -- is not a rejection: the counter must not move.
    #[test]
    fn valid_chain_with_unmappable_intermediate_counts_no_rejection() {
        let metrics = MetricsCapture::start();
        let middleware = CertDescriptionMiddleware::<NoAuthorization>::new(None, spiffe_context());
        let mut service = middleware.layer(Terminal);

        capture_logs(|| {
            let mut request = Request::builder()
                .uri(rpc::service_path!("Anything"))
                .body(String::new())
                .expect("request");
            request
                .extensions_mut()
                .insert(Arc::new(ConnectionAttributes {
                    peer_address: "192.0.2.8:4433".parse().expect("socket address"),
                    peer_certificates: vec![
                        spiffe_leaf_certificate("/carbide-system/sa/test-service"),
                        CertificateDer::from(b"not a certificate".to_vec()),
                    ],
                }));
            let _response_future = service.call(request);
        });

        assert_eq!(
            metrics.counter_delta(
                "carbide_authn_client_cert_rejected_total",
                &[("reason", "validation")]
            ),
            0.0,
            "a chain that minted a principal is not a rejection"
        );
    }

    /// A certificate that maps to no principal is dropped, and the drop is
    /// counted: one emit writes the DEBUG log line (reason, peer, error) AND
    /// moves carbide_authn_client_cert_rejected_total.
    #[test]
    fn rejected_client_cert_logs_and_counts() {
        let metrics = MetricsCapture::start();
        let middleware = CertDescriptionMiddleware::<NoAuthorization>::new(None, spiffe_context());
        let mut service = middleware.layer(Terminal);

        let logs = capture_logs(|| {
            let mut request = Request::builder()
                .uri(rpc::service_path!("Anything"))
                .body(String::new())
                .expect("request");
            request
                .extensions_mut()
                .insert(Arc::new(ConnectionAttributes {
                    peer_address: "192.0.2.7:4433".parse().expect("socket address"),
                    peer_certificates: vec![CertificateDer::from(b"not a certificate".to_vec())],
                }));
            let _response_future = service.call(request);
        });

        let rejection = logs
            .iter()
            .find(|log| log.message == "Rejected a client certificate")
            .expect("the rejection log line");
        assert_eq!(rejection.level, tracing::Level::DEBUG);
        let field = |name: &str| {
            rejection
                .fields
                .iter()
                .find(|(key, _)| key == name)
                .map(|(_, value)| value.as_str())
        };
        assert_eq!(field("reason"), Some("validation"));
        assert_eq!(field("peer_address"), Some("192.0.2.7:4433"));
        assert!(
            field("error").is_some_and(|error| !error.is_empty()),
            "the rejection carries the certificate error"
        );

        assert_eq!(
            metrics.counter_delta(
                "carbide_authn_client_cert_rejected_total",
                &[("reason", "validation")]
            ),
            1.0
        );
    }

    /// Both SpiffeError variants map onto the bounded reason label.
    #[test]
    fn reject_reason_maps_the_spiffe_error_variants() {
        use carbide_instrument::LabelValue as _;

        let validation =
            SpiffeError::Validation(SpiffeValidationError::ValidationError("bad".to_string()));
        let recognition =
            SpiffeError::Recognition(SpiffeContextError::ContextError("unknown".to_string()));

        assert_eq!(RejectReason::from(&validation), RejectReason::Validation);
        assert_eq!(RejectReason::from(&recognition), RejectReason::Recognition);
        assert_eq!(
            RejectReason::Validation.label_value().as_str(),
            "validation"
        );
        assert_eq!(
            RejectReason::Recognition.label_value().as_str(),
            "recognition"
        );
    }

    /// Runs a request through the middleware and returns the principals the authn
    /// layer attached (captured from the request extensions by an inner service).
    ///
    /// The request carries `ConnectionAttributes` with no peer certificates —
    /// a bearer-token client over plain TLS. Omitting them entirely is the
    /// wiring-error path, which logs `authentication_connection_attributes_missing`
    /// and would poison that callsite's cached tracing interest for the
    /// capture-based test of that warning.
    async fn principals_for(
        middleware: CertDescriptionMiddleware<NoAuthorization>,
        auth_header: Option<&str>,
    ) -> Vec<Principal> {
        principals_for_certs(middleware, auth_header, Vec::new()).await
    }

    #[tokio::test]
    async fn valid_bearer_token_yields_machine_principal() {
        let middleware = CertDescriptionMiddleware::<NoAuthorization>::new(None, spiffe_context())
            .with_bearer_authenticator(Arc::new(FakeAuth(
                "spiffe://example.test/carbide-system/machine/m1".to_string(),
            )));
        let principals = principals_for(middleware, Some("Bearer good")).await;
        assert!(
            principals.contains(&Principal::SpiffeMachineIdentifier("m1".to_string())),
            "expected machine principal, got {principals:?}"
        );
        assert!(
            principals.contains(&Principal::TrustedCertificate),
            "a validated bearer token must authorize as a trusted identity, got {principals:?}"
        );
    }

    /// RFC 6750's scheme is case-insensitive and allows more than one space
    /// before the token. Our own clients send exactly `Bearer <jwt>`, but a
    /// conforming third-party client must not be turned away.
    #[tokio::test]
    async fn bearer_scheme_is_matched_case_insensitively() {
        for header in ["bearer good", "BEARER good", "BeArEr good", "Bearer   good"] {
            let middleware =
                CertDescriptionMiddleware::<NoAuthorization>::new(None, spiffe_context())
                    .with_bearer_authenticator(Arc::new(FakeAuth(
                        "spiffe://example.test/carbide-system/machine/m1".to_string(),
                    )));
            let principals = principals_for(middleware, Some(header)).await;
            assert!(
                principals.contains(&Principal::SpiffeMachineIdentifier("m1".to_string())),
                "{header:?} is a valid bearer credential, got {principals:?}"
            );
        }
    }

    /// A different scheme must not be read as a bearer token, and neither must
    /// a bare value with no scheme at all.
    #[tokio::test]
    async fn a_non_bearer_authorization_header_is_ignored() {
        for header in ["Basic good", "good", "Bearer"] {
            let middleware =
                CertDescriptionMiddleware::<NoAuthorization>::new(None, spiffe_context())
                    .with_bearer_authenticator(Arc::new(FakeAuth(
                        "spiffe://example.test/carbide-system/machine/m1".to_string(),
                    )));
            let principals = principals_for(middleware, Some(header)).await;
            assert!(
                !principals
                    .iter()
                    .any(|p| matches!(p, Principal::SpiffeMachineIdentifier(_))),
                "{header:?} must not authenticate, got {principals:?}"
            );
        }
    }

    #[tokio::test]
    async fn invalid_bearer_token_yields_no_machine_principal() {
        let middleware = CertDescriptionMiddleware::<NoAuthorization>::new(None, spiffe_context())
            .with_bearer_authenticator(Arc::new(FakeAuth(
                "spiffe://example.test/carbide-system/machine/m1".to_string(),
            )));
        let principals = principals_for(middleware, Some("Bearer bogus")).await;
        assert!(
            !principals
                .iter()
                .any(|p| matches!(p, Principal::SpiffeMachineIdentifier(_))),
            "rejected token must not yield a machine principal, got {principals:?}"
        );
        assert!(
            !principals.contains(&Principal::TrustedCertificate),
            "rejected token must not earn trusted-certificate, got {principals:?}"
        );
    }

    /// Like [`principals_for`], but the request also presents client
    /// certificates via [`ConnectionAttributes`].
    async fn principals_for_certs(
        middleware: CertDescriptionMiddleware<NoAuthorization>,
        auth_header: Option<&str>,
        peer_certificates: Vec<CertificateDer<'static>>,
    ) -> Vec<Principal> {
        let inner = tower::service_fn(|req: Request<tonic::body::Body>| async move {
            let principals = req
                .extensions()
                .get::<AuthContext<NoAuthorization>>()
                .map(|ctx| ctx.principals.clone())
                .unwrap_or_default();
            Ok::<_, Infallible>(principals)
        });
        let svc = middleware.layer(inner);
        let mut builder = Request::builder();
        if let Some(value) = auth_header {
            builder = builder.header(AUTHORIZATION, value);
        }
        let mut request = builder.body(tonic::body::Body::empty()).unwrap();
        request
            .extensions_mut()
            .insert(Arc::new(ConnectionAttributes {
                peer_address: "192.0.2.9:4433".parse().expect("socket address"),
                peer_certificates,
            }));
        svc.oneshot(request).await.unwrap()
    }

    async fn principals_for_chain(
        (machine_certs_enabled, chain): (bool, &[&str]),
    ) -> Vec<Principal> {
        let middleware = CertDescriptionMiddleware::<NoAuthorization>::new(None, spiffe_context())
            .with_machine_certs_enabled(machine_certs_enabled);
        let chain = chain
            .iter()
            .map(|path| spiffe_leaf_certificate(path))
            .collect();
        principals_for_certs(middleware, None, chain).await
    }

    /// Certificates after the end-entity one yield no principals.
    #[tokio::test]
    async fn only_the_end_entity_certificate_names_the_caller() {
        check_cases_async(
            [
                Case {
                    scenario: "a service appends another service's certificate",
                    input: (
                        true,
                        &[
                            "/carbide-system/sa/some-service",
                            "/carbide-system/sa/nico-api",
                        ][..],
                    ),
                    expect: Yields(vec![
                        Principal::SpiffeServiceIdentifier("some-service".to_string()),
                        Principal::TrustedCertificate,
                    ]),
                },
                Case {
                    scenario: "a client whose own certificate names no one appends a service's",
                    input: (
                        true,
                        &["/elsewhere/unknown", "/carbide-system/sa/nico-api"][..],
                    ),
                    expect: Yields(vec![Principal::TrustedCertificate]),
                },
                Case {
                    scenario: "a refused machine appends a service's certificate",
                    input: (
                        false,
                        &["/carbide-system/machine/m1", "/carbide-system/sa/nico-api"][..],
                    ),
                    expect: Yields(vec![]),
                },
            ],
            |input| async move { Ok::<_, Infallible>(principals_for_chain(input).await) },
        )
        .await;
    }

    #[tokio::test]
    async fn machine_cert_yields_machine_principal_by_default() {
        let middleware = CertDescriptionMiddleware::<NoAuthorization>::new(None, spiffe_context());
        let principals = principals_for_certs(
            middleware,
            None,
            vec![spiffe_leaf_certificate("/carbide-system/machine/m1")],
        )
        .await;
        assert!(
            principals.contains(&Principal::SpiffeMachineIdentifier("m1".to_string())),
            "expected cert-derived machine principal, got {principals:?}"
        );
    }

    #[tokio::test]
    async fn machine_cert_ignored_when_machine_certs_disabled() {
        // `[node_auth] mtls_enabled = false`: a machine client cert mints no
        // machine principal, while a valid bearer token still does — the JWT
        // becomes the only node auth path.
        let middleware = CertDescriptionMiddleware::<NoAuthorization>::new(None, spiffe_context())
            .with_machine_certs_enabled(false)
            .with_bearer_authenticator(Arc::new(FakeAuth(
                "spiffe://example.test/carbide-system/machine/m2".to_string(),
            )));
        let principals = principals_for_certs(
            middleware,
            Some("Bearer good"),
            vec![spiffe_leaf_certificate("/carbide-system/machine/m1")],
        )
        .await;
        assert!(
            !principals.contains(&Principal::SpiffeMachineIdentifier("m1".to_string())),
            "cert-derived machine principal must be dropped, got {principals:?}"
        );
        assert!(
            principals.contains(&Principal::SpiffeMachineIdentifier("m2".to_string())),
            "bearer-derived machine principal must survive, got {principals:?}"
        );
    }

    #[tokio::test]
    async fn service_cert_unaffected_when_machine_certs_disabled() {
        // The gate is scoped to machine certs: service identities on the same
        // listener keep authenticating via mTLS.
        let middleware = CertDescriptionMiddleware::<NoAuthorization>::new(None, spiffe_context())
            .with_machine_certs_enabled(false);
        let principals = principals_for_certs(
            middleware,
            None,
            vec![spiffe_leaf_certificate("/carbide-system/sa/test-service")],
        )
        .await;
        assert!(
            principals
                .iter()
                .any(|p| matches!(p, Principal::SpiffeServiceIdentifier(_))),
            "service cert principal must survive, got {principals:?}"
        );
    }

    /// The point of `mtls_enabled = false` is that a machine certificate stops
    /// authenticating the node. Dropping only the machine principal does not
    /// achieve that: `TrustedCertificate` is granted `forge/*` and `nico/*` by
    /// the shipped Casbin policy, so leaving it in place would keep the caller
    /// fully authorized under a different name and the switch would be
    /// cosmetic. Assert the whole principal set, not the absence of one member.
    #[tokio::test]
    async fn a_machine_cert_alone_authorizes_nothing_when_machine_certs_are_disabled() {
        let middleware = CertDescriptionMiddleware::<NoAuthorization>::new(None, spiffe_context())
            .with_machine_certs_enabled(false);
        let principals = principals_for_certs(
            middleware,
            None,
            vec![spiffe_leaf_certificate("/carbide-system/machine/m1")],
        )
        .await;
        assert!(
            principals.is_empty(),
            "a refused machine cert must leave no authorizing principal, got {principals:?}"
        );
    }

    /// The realistic migration state: the agent presents its machine cert *and*
    /// a bearer token. Refusing the cert does not affect the independently
    /// validated bearer credential, which is the sole authorization path when
    /// machine mTLS is disabled.
    #[tokio::test]
    async fn a_bearer_token_authorizes_when_the_machine_cert_is_refused() {
        let middleware = CertDescriptionMiddleware::<NoAuthorization>::new(None, spiffe_context())
            .with_bearer_authenticator(Arc::new(FakeAuth(
                "spiffe://example.test/carbide-system/machine/m1".to_string(),
            )))
            .with_machine_certs_enabled(false);
        let principals = principals_for_certs(
            middleware,
            Some("Bearer good"),
            vec![spiffe_leaf_certificate("/carbide-system/machine/m1")],
        )
        .await;

        assert!(
            principals.contains(&Principal::SpiffeMachineIdentifier("m1".to_string())),
            "the bearer token is the credential now, and must still authenticate: {principals:?}"
        );
        assert!(
            principals.contains(&Principal::TrustedCertificate),
            "a validated bearer token must authorize even when its mTLS cert is refused: {principals:?}"
        );
    }

    /// The suppression above is scoped to machine certs: a service cert still
    /// earns `TrustedCertificate` alongside its own principal.
    #[tokio::test]
    async fn a_service_cert_keeps_trusted_certificate_when_machine_certs_are_disabled() {
        let middleware = CertDescriptionMiddleware::<NoAuthorization>::new(None, spiffe_context())
            .with_machine_certs_enabled(false);
        let principals = principals_for_certs(
            middleware,
            None,
            vec![spiffe_leaf_certificate("/carbide-system/sa/test-service")],
        )
        .await;
        assert!(
            principals.contains(&Principal::TrustedCertificate),
            "service certs are unaffected by the machine gate, got {principals:?}"
        );
    }

    /// With machine certs enabled the old behavior is unchanged — the gate must
    /// not cost a normal mTLS node its trusted-certificate grant.
    #[tokio::test]
    async fn a_machine_cert_keeps_trusted_certificate_when_machine_certs_are_enabled() {
        let middleware = CertDescriptionMiddleware::<NoAuthorization>::new(None, spiffe_context());
        let principals = principals_for_certs(
            middleware,
            None,
            vec![spiffe_leaf_certificate("/carbide-system/machine/m1")],
        )
        .await;
        assert!(
            principals.contains(&Principal::TrustedCertificate),
            "enabled machine certs keep their trusted-certificate grant, got {principals:?}"
        );
    }

    #[tokio::test]
    async fn bearer_ignored_when_no_authenticator_configured() {
        // Without an authenticator installed, an Authorization header is ignored
        // (nodes keep using mTLS) — no machine principal appears.
        let middleware = CertDescriptionMiddleware::<NoAuthorization>::new(None, spiffe_context());
        let principals = principals_for(middleware, Some("Bearer good")).await;
        assert!(
            !principals
                .iter()
                .any(|p| matches!(p, Principal::SpiffeMachineIdentifier(_))),
            "no authenticator => no bearer principals, got {principals:?}"
        );
    }

    #[test]
    fn bearer_token_from_headers_parses_scheme() {
        let mut headers = hyper::HeaderMap::new();
        assert_eq!(bearer_token_from_headers(&headers), None);
        headers.insert(AUTHORIZATION, "Bearer abc.def".parse().unwrap());
        assert_eq!(bearer_token_from_headers(&headers), Some("abc.def"));
        headers.insert(AUTHORIZATION, "Basic xyz".parse().unwrap());
        assert_eq!(bearer_token_from_headers(&headers), None);
    }
}
