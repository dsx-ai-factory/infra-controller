// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

use std::sync::Arc;
use std::time::Duration;

use carbide_secrets::SpiffeIdentity;
use carbide_secrets::certificates::{Certificate, machine_spiffe_uri};
use rand::RngExt;
use rcgen::{
    CertificateParams, DistinguishedName, ExtendedKeyUsagePurpose, KeyPair, KeyUsagePurpose,
    PublicKeyData, SanType,
};
use rustls::server::WebPkiClientVerifier;
use rustls::server::danger::ClientCertVerifier;
use rustls_pki_types::{CertificateDer, UnixTime};
use x509_parser::prelude::{FromDer, GeneralName, X509Certificate};

use crate::resources::RequestStatus;
use crate::{Error, MIN_CERTIFICATE_TTL};

pub(crate) struct CertificateRequestMaterial {
    key: KeyPair,
    params: CertificateParams,
    spiffe_uri: String,
    dns_names: Vec<String>,
    pub(crate) lifetime: Duration,
}

impl CertificateRequestMaterial {
    pub(crate) fn new(
        spiffe: &SpiffeIdentity,
        identifier: &str,
        alt_names: Option<&str>,
        ttl: Option<&str>,
        max_ttl: Duration,
    ) -> Result<Self, Error> {
        if identifier.is_empty() || identifier.contains(['/', '?', '#']) {
            return Err(Error::InvalidArgument(
                "certificate identifier must be a non-empty SPIFFE path segment".into(),
            ));
        }
        let spiffe_uri =
            machine_spiffe_uri(&spiffe.trust_domain, &spiffe.machine_base_path, identifier);
        carbide_authn::spiffe_id::SpiffeId::new(&spiffe_uri)
            .map_err(|error| Error::InvalidArgument(error.to_string()))?;
        let mut params = CertificateParams::default();
        params.distinguished_name = DistinguishedName::new();
        params.subject_alt_names =
            vec![SanType::URI(spiffe_uri.clone().try_into().map_err(
                |_| Error::InvalidArgument("SPIFFE URI must be ASCII".into()),
            )?)];
        let dns_names: Vec<String> = alt_names.map_or_else(Vec::new, |names| {
            names
                .split(',')
                .map(|name| name.trim().to_string())
                .collect()
        });
        for name in &dns_names {
            params.subject_alt_names.push(SanType::DnsName(
                name.clone()
                    .try_into()
                    .map_err(|_| Error::InvalidArgument("DNS SAN must be ASCII".into()))?,
            ));
            if name.is_empty() {
                return Err(Error::InvalidArgument("DNS SAN must be non-empty".into()));
            }
            rustls_pki_types::ServerName::try_from(name.as_str())
                .map_err(|_| Error::InvalidArgument(format!("invalid DNS SAN: {name}")))?;
        }
        let lifetime = if let Some(ttl) = ttl {
            let ttl = humantime::parse_duration(ttl).map_err(|error| {
                Error::InvalidArgument(format!("invalid certificate TTL: {error}"))
            })?;
            if ttl < MIN_CERTIFICATE_TTL || ttl.subsec_nanos() != 0 {
                return Err(Error::InvalidArgument(format!(
                    "certificate TTL must be a whole-second duration of at least {}",
                    humantime::format_duration(MIN_CERTIFICATE_TTL),
                )));
            }
            ttl
        } else {
            // Match Vault's random 60..100% of a thirty-day lifetime.
            Duration::from_secs(rand::rng().random_range(432..720) * 3600)
        }
        .min(max_ttl);
        params.key_usages = vec![
            KeyUsagePurpose::DigitalSignature,
            KeyUsagePurpose::KeyAgreement,
            KeyUsagePurpose::KeyEncipherment,
        ];
        params.extended_key_usages = vec![
            ExtendedKeyUsagePurpose::ClientAuth,
            ExtendedKeyUsagePurpose::ServerAuth,
        ];
        Ok(Self {
            key: KeyPair::generate_for(&rcgen::PKCS_ECDSA_P256_SHA256)?,
            params,
            spiffe_uri,
            dns_names,
            lifetime,
        })
    }

    pub(crate) fn csr_pem(&self) -> Result<Vec<u8>, Error> {
        Ok(self
            .params
            .serialize_request(&self.key)?
            .pem()?
            .into_bytes())
    }

    pub(crate) fn validate(
        &self,
        status: &RequestStatus,
        trust_bundle: &[u8],
    ) -> Result<Certificate, Error> {
        let public_key = status
            .certificate
            .as_ref()
            .ok_or_else(|| Error::Certificate("Ready request has no certificate".into()))?;
        let issuing_ca = status.ca.as_ref();
        let chain = parse_certs(&public_key.0)?;
        let ca_chain = issuing_ca
            .map(|ca| parse_certs(&ca.0))
            .transpose()?
            .unwrap_or_default();
        let leaf = &chain[0];
        let (rest, cert) = X509Certificate::from_der(leaf.as_ref())
            .map_err(|error| Error::Certificate(error.to_string()))?;
        if !rest.is_empty() || cert.public_key().raw != self.key.subject_public_key_info() {
            return Err(Error::Certificate(
                "certificate does not match the locally generated key".into(),
            ));
        }
        let spiffe = carbide_authn::validate_x509_certificate(leaf.as_ref())
            .map_err(|error| Error::Certificate(error.to_string()))?;
        if spiffe.to_string() != self.spiffe_uri {
            return Err(Error::Certificate(
                "certificate SPIFFE URI does not match the requested identity".into(),
            ));
        }
        let san = cert
            .subject_alternative_name()
            .map_err(|error| Error::Certificate(error.to_string()))?
            .ok_or_else(|| {
                Error::Certificate("certificate has no subject alternative names".into())
            })?;
        let mut actual_dns: Vec<_> = san
            .value
            .general_names
            .iter()
            .filter_map(|name| {
                if let GeneralName::DNSName(name) = name {
                    Some(*name)
                } else {
                    None
                }
            })
            .collect();
        let mut expected_dns: Vec<_> = self.dns_names.iter().map(String::as_str).collect();
        actual_dns.sort_unstable();
        expected_dns.sort_unstable();
        if actual_dns != expected_dns {
            return Err(Error::Certificate(
                "certificate DNS SANs do not match the request".into(),
            ));
        }
        let usages = cert
            .key_usage()
            .map_err(|error| Error::Certificate(error.to_string()))?
            .ok_or_else(|| Error::Certificate("certificate has no key usages".into()))?;
        if !usages.value.digital_signature()
            || !usages.value.key_agreement()
            || !usages.value.key_encipherment()
        {
            return Err(Error::Certificate(
                "certificate is missing required key usages".into(),
            ));
        }
        let extended = cert
            .extended_key_usage()
            .map_err(|error| Error::Certificate(error.to_string()))?
            .ok_or_else(|| Error::Certificate("certificate has no extended key usages".into()))?;
        if !extended.value.client_auth || !extended.value.server_auth {
            return Err(Error::Certificate(
                "certificate requires client and server authentication usages".into(),
            ));
        }
        let now = UnixTime::now();
        if cert.validity().not_after.timestamp() > (now.as_secs() + self.lifetime.as_secs()) as i64
        {
            return Err(Error::Certificate(
                "certificate exceeds the requested lifetime".into(),
            ));
        }
        if cert.validity().not_after.timestamp()
            < (now.as_secs() + MIN_CERTIFICATE_TTL.as_secs()) as i64
        {
            return Err(Error::Certificate(format!(
                "certificate must remain valid for at least {}",
                humantime::format_duration(MIN_CERTIFICATE_TTL),
            )));
        }
        let intermediates: Vec<_> = chain.iter().skip(1).chain(&ca_chain).cloned().collect();
        verifier(trust_bundle)?
            .verify_client_cert(leaf, &intermediates, now)
            .map_err(|error| Error::Certificate(error.to_string()))?;
        for issuer in &intermediates {
            let (_, issuer) = X509Certificate::from_der(issuer.as_ref())
                .map_err(|error| Error::Certificate(error.to_string()))?;
            if cert.validity().not_after > issuer.validity().not_after {
                return Err(Error::Certificate(
                    "certificate would outlive its issuing CA".into(),
                ));
            }
        }
        Ok(Certificate {
            public_key: public_key.0.clone(),
            private_key: self.key.serialize_pem().into_bytes(),
            issuing_ca: issuing_ca.map_or_else(Vec::new, |ca| ca.0.clone()),
        })
    }
}

pub(crate) fn verifier(trust_bundle: &[u8]) -> Result<Arc<dyn ClientCertVerifier>, Error> {
    let mut roots = rustls::RootCertStore::empty();
    for certificate in parse_certs(trust_bundle)? {
        roots
            .add(certificate)
            .map_err(|error| Error::Certificate(error.to_string()))?;
    }
    WebPkiClientVerifier::builder_with_provider(
        Arc::new(roots),
        Arc::new(rustls::crypto::aws_lc_rs::default_provider()),
    )
    .build()
    .map_err(|error| Error::Certificate(error.to_string()))
}

fn parse_certs(pem: &[u8]) -> Result<Vec<CertificateDer<'static>>, Error> {
    let certificates =
        rustls_pemfile::certs(&mut std::io::Cursor::new(pem)).collect::<Result<Vec<_>, _>>()?;
    if certificates.is_empty() {
        return Err(Error::Certificate(
            "PEM bundle contains no certificates".into(),
        ));
    }
    Ok(certificates)
}
