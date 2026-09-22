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
use std::fmt::Display;
use std::sync::Arc;

pub use crate::chained_reader::ChainedCredentialReader;
/// Direct vault access for the narrow cases that need it: `CertificateProvider`
/// (PKI), and the Transit KMS provider, which builds its own raw vault client
/// via `create_raw_vault_client_settings`. Credential operations should go
/// through `create_credential_manager` instead of using the vault client directly.
pub use crate::forge_vault::{
    DedicatedVaultConfig, ForgeVaultClient, SpiffeIdentity, VaultConfig,
    create_dedicated_vault_client, create_raw_vault_client_settings, create_vault_client,
};
pub use crate::local_credentials::{
    CredentialSnapshot, EnvCredentialsConfig, FileCredentialsConfig, MachineIdentityConfig,
    UsernamePassword,
};

pub mod certificates;
pub mod chained_reader;
pub mod credentials;
pub mod forge_vault;
pub mod key_encryption;
pub mod local_credentials;
pub mod memory_credentials;

#[cfg(any(test, feature = "test-support"))]
pub mod test_support;

use credentials::{
    CompositeCredentialManager, CredentialManager, CredentialReader, CredentialWriter,
};
use local_credentials::{EnvCredentials, FileCredentialsWatcher};
pub use memory_credentials::MemoryCredentialStore;

use crate::certificates::CertificateProvider;

#[derive(Default, Debug, Clone)]
pub struct CredentialConfig {
    pub vault: VaultConfig,
    pub env: EnvCredentialsConfig,
    pub file: FileCredentialsConfig,
}

/// Selects and configures the backend that vends machine/service certificates.
///
/// Certificate vending is independent of the credential store: this lets the
/// API issue PKI certificates from a different Vault than the one backing
/// credentials — or, in future, from a non-Vault CA — without disturbing
/// credential storage.
#[derive(Default, Debug, Clone)]
pub struct CertificateConfig {
    pub backend: CertBackend,
}

/// Backend used to issue certificates.
///
/// Today both variants are Vault-backed. The enum exists so additional backends
/// (e.g. an in-process CA whose key lives in a Kubernetes Secret) can be added
/// without touching the call sites that consume [`CertificateProvider`].
// The shared `Vault` suffix is intentional: both current variants are Vault
// backends, distinguished by whether the client is shared with the credential
// store. The lint resolves once a non-Vault backend is added.
#[allow(clippy::enum_variant_names)]
#[derive(Default, Debug, Clone)]
pub enum CertBackend {
    /// Reuse the credential store's Vault client — one client, one token lease.
    /// This is the default and matches historical behavior.
    #[default]
    SharedVault,
    /// Issue certificates from a dedicated Vault, decoupled from the credential
    /// store. [`DedicatedVaultConfig`] is fully explicit: its connection fields
    /// never fall back to the process-global `VAULT_*` env vars, so a partial
    /// config fails fast instead of silently re-pointing at the credential
    /// Vault.
    DedicatedVault(DedicatedVaultConfig),
}

/// Builds the certificate provider selected by `config`.
///
/// `shared_vault` is the already-constructed credential Vault client, reused
/// for [`CertBackend::SharedVault`] so no second client or token lease is
/// created in the common case. `spiffe` is the site's SPIFFE identity; a
/// dedicated Vault issues certs under the same identity namespace as the rest
/// of the deployment.
pub fn create_certificate_provider(
    config: &CertificateConfig,
    shared_vault: &Arc<ForgeVaultClient>,
    spiffe: SpiffeIdentity,
) -> eyre::Result<Arc<dyn CertificateProvider>> {
    match &config.backend {
        CertBackend::SharedVault => {
            let provider: Arc<dyn CertificateProvider> = shared_vault.clone();
            Ok(provider)
        }
        CertBackend::DedicatedVault(dedicated) => {
            let provider: Arc<dyn CertificateProvider> =
                create_dedicated_vault_client(dedicated, spiffe)?;
            Ok(provider)
        }
    }
}

/// create_credential_manager builds the default credential chain: env -> file -> vault.
pub async fn create_credential_manager(
    config: &CredentialConfig,
) -> eyre::Result<Arc<dyn CredentialManager>> {
    let mut readers: Vec<Box<dyn CredentialReader>> = Vec::new();

    if config.env.enabled() {
        readers.push(Box::new(EnvCredentials::new(config.env.clone())?));
    }

    if config.file.enabled() {
        readers.push(Box::new(
            FileCredentialsWatcher::new(config.file.clone()).await?,
        ));
    }

    let vault_client = create_vault_client(&config.vault)?;
    readers.push(Box::new(vault_client.clone()));

    let chained = ChainedCredentialReader::from(readers);
    let composite = CompositeCredentialManager::new(chained, vault_client);
    Ok(Arc::new(composite))
}

/// create_credential_manager_from builds a
/// credential manager from a caller-defined chain.
/// The caller fully controls the reader order and
/// writer selection.
pub fn create_credential_manager_from(
    writer: Arc<dyn CredentialWriter>,
    readers: Vec<Box<dyn CredentialReader>>,
) -> Arc<dyn CredentialManager> {
    let chained = ChainedCredentialReader::from(readers);
    Arc::new(CompositeCredentialManager::new(chained, writer))
}

#[derive(Debug)]
pub enum SecretsError {
    GenericError(eyre::Report),
    /// A local UFM source owns credentials but has no entry for the requested
    /// fabric; callers should surface the configuration remediation.
    UfmCredentialReadBlocked {
        fabric: String,
    },
    /// A caller attempted to mutate a persistent UFM credential while local
    /// sources own UFM credentials; callers should direct the operator to the
    /// configured local source.
    UfmCredentialMutationBlocked {
        fabric: String,
    },
}

impl Display for SecretsError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            SecretsError::GenericError(report) => {
                write!(f, "Secrets operation failed: {}", report)
            }
            SecretsError::UfmCredentialReadBlocked { fabric } => write!(
                f,
                "secrets operation failed: credential for UFM fabric {fabric:?} is absent from \
                 the configured local sources; {}; {}",
                crate::chained_reader::UFM_LOCAL_CREDENTIAL_REMEDIATION,
                crate::chained_reader::UFM_BACKEND_SOURCE_REMEDIATION
            ),
            SecretsError::UfmCredentialMutationBlocked { fabric } => write!(
                f,
                "persistent backend credential mutation is disabled for UFM fabric {fabric:?} \
                 because local sources own all UFM credentials; {}; {}",
                crate::chained_reader::UFM_LOCAL_CREDENTIAL_REMEDIATION,
                crate::chained_reader::UFM_BACKEND_SOURCE_REMEDIATION
            ),
        }
    }
}

impl From<eyre::Report> for SecretsError {
    fn from(value: eyre::Report) -> Self {
        SecretsError::GenericError(value)
    }
}

impl From<SecretsError> for eyre::Report {
    fn from(value: SecretsError) -> Self {
        match value {
            SecretsError::GenericError(report) => report,
            value @ SecretsError::UfmCredentialReadBlocked { .. } => eyre::eyre!("{value}"),
            value @ SecretsError::UfmCredentialMutationBlocked { .. } => eyre::eyre!("{value}"),
        }
    }
}
