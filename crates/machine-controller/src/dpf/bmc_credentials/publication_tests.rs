/*
 * SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
 * SPDX-License-Identifier: Apache-2.0
 */

use std::collections::BTreeMap;
use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use carbide_dpf::crds::dpfoperatorconfigs_generated::DPFOperatorConfig;
use carbide_dpf::repository::{
    BmcCredentialRepository, DpfOperatorConfigRepository, K8sConfigRepository,
};
use carbide_dpf::{DpfDeletePreconditions, DpfError, DpfSdkBuilder};
use carbide_secrets::MemoryCredentialStore;
use carbide_secrets::credentials::{
    BmcCredentialType, CredentialKey, CredentialWriter, Credentials,
};
use carbide_test_support::Outcome::Yields;
use carbide_test_support::{Case, check_cases_async};
use k8s_openapi::api::core::v1::Secret;

use super::ensure_device_bmc_credential;

#[derive(Clone, Default)]
struct PublicationRepository {
    secret: Arc<Mutex<Option<Secret>>>,
    create_error: Arc<Mutex<Option<DpfError>>>,
}

#[async_trait]
impl BmcCredentialRepository for PublicationRepository {
    async fn get_bmc_credential_secret(
        &self,
        name: &str,
        namespace: &str,
    ) -> Result<Option<Secret>, DpfError> {
        Ok(self.secret.lock().unwrap().clone().filter(|secret| {
            secret.metadata.name.as_deref() == Some(name)
                && secret.metadata.namespace.as_deref() == Some(namespace)
        }))
    }

    async fn create_bmc_credential_secret(&self, secret: &Secret) -> Result<(), DpfError> {
        if let Some(error) = self.create_error.lock().unwrap().take() {
            return Err(error);
        }
        let previous = self.secret.lock().unwrap().replace(secret.clone());
        assert!(
            previous.is_none(),
            "publication must create only one Secret"
        );
        Ok(())
    }

    async fn list_bmc_credential_secrets(
        &self,
        _device_id: &str,
        namespace: &str,
    ) -> Result<Vec<Secret>, DpfError> {
        Ok(self
            .secret
            .lock()
            .unwrap()
            .iter()
            .filter(|secret| secret.metadata.namespace.as_deref() == Some(namespace))
            .cloned()
            .collect())
    }

    async fn delete_bmc_credential_secret(
        &self,
        _name: &str,
        _namespace: &str,
        _preconditions: &DpfDeletePreconditions,
    ) -> Result<(), DpfError> {
        unreachable!("publication does not delete credentials")
    }
}

#[async_trait]
impl K8sConfigRepository for PublicationRepository {
    async fn get_configmap(
        &self,
        _name: &str,
        _namespace: &str,
    ) -> Result<Option<BTreeMap<String, String>>, DpfError> {
        unreachable!("credential publication does not read ConfigMaps")
    }

    async fn create_configmap(
        &self,
        _name: &str,
        _namespace: &str,
        _data: BTreeMap<String, String>,
    ) -> Result<bool, DpfError> {
        unreachable!("credential publication does not create ConfigMaps")
    }

    async fn apply_configmap(
        &self,
        _name: &str,
        _namespace: &str,
        _data: BTreeMap<String, String>,
    ) -> Result<(), DpfError> {
        unreachable!("credential publication does not update ConfigMaps")
    }

    async fn get_secret(
        &self,
        _name: &str,
        _namespace: &str,
    ) -> Result<Option<BTreeMap<String, Vec<u8>>>, DpfError> {
        unreachable!("publication reads credentials through BmcCredentialRepository")
    }

    async fn apply_secret(
        &self,
        name: &str,
        namespace: &str,
        data: BTreeMap<String, Vec<u8>>,
    ) -> Result<(), DpfError> {
        assert_eq!((name, namespace), ("bmc-shared-password", "dpf-test"));
        assert_eq!(data["password"], b"new-password");
        Ok(())
    }
}

#[async_trait]
impl DpfOperatorConfigRepository for PublicationRepository {
    async fn get(
        &self,
        _name: &str,
        _namespace: &str,
    ) -> Result<Option<DPFOperatorConfig>, DpfError> {
        unreachable!("SDK construction reads the operator version directly")
    }

    async fn get_version(&self, _name: &str, _namespace: &str) -> Result<String, DpfError> {
        Ok("30.0.0".to_string())
    }

    async fn patch(
        &self,
        _name: &str,
        _namespace: &str,
        _patch: serde_json::Value,
    ) -> Result<(), DpfError> {
        unreachable!("credential publication does not update operator configuration")
    }
}

/// A retained per-device credential is publishable without any site revision record.
#[tokio::test]
async fn publication_uses_retained_device_password_without_site_revision() -> eyre::Result<()> {
    let device_id = "01-02-03-04-05-06";
    let device_mac = device_id.parse()?;
    let store = MemoryCredentialStore::default();
    store
        .set_credentials(
            &CredentialKey::BmcCredentials {
                credential_type: BmcCredentialType::BmcRoot {
                    bmc_mac_address: device_mac,
                },
            },
            &Credentials::new("root", "old-password"),
        )
        .await
        .expect("store the retained device credential");

    let repo = PublicationRepository::default();
    let sdk = DpfSdkBuilder::new(repo.clone(), "dpf-test", "new-password".to_string())
        .build_without_resources()
        .await?;
    let name = ensure_device_bmc_credential(&sdk, &store, device_id).await?;

    assert_eq!(name, "01-02-03-04-05-06-v0");
    let published = repo.secret.lock().unwrap();
    let published = published
        .as_ref()
        .expect("per-device Secret must be created");
    assert_eq!(published.metadata.name.as_deref(), Some(name.as_str()));
    assert_eq!(
        published.data.as_ref().unwrap()["password"].0,
        b"old-password"
    );
    Ok(())
}

#[tokio::test]
async fn publication_marks_only_allocation_races_as_retryable() -> eyre::Result<()> {
    let device_id = "01-02-03-04-05-06";
    let store = MemoryCredentialStore::default();
    store
        .set_credentials(
            &CredentialKey::BmcCredentials {
                credential_type: BmcCredentialType::BmcRoot {
                    bmc_mac_address: device_id.parse()?,
                },
            },
            &Credentials::new("root", "old-password"),
        )
        .await?;

    let cases = [
        (
            "allocation conflict retains its cause in the retry marker",
            kube::core::Status::failure("allocation conflicted", "Conflict").with_code(409),
            true,
        ),
        (
            "missing create permission remains a permanent API failure",
            kube::core::Status::failure("cannot create secrets", "Forbidden").with_code(403),
            false,
        ),
        (
            "missing namespace is not a disappearing credential Secret",
            kube::core::Status::failure("namespaces \"dpf-test\" not found", "NotFound")
                .with_code(404),
            false,
        ),
    ]
    .map(|(scenario, status, retry)| Case {
        scenario,
        input: status.clone(),
        expect: Yields((retry, status)),
    });
    check_cases_async(cases, |status| {
        let store = &store;
        async move {
            let repo = PublicationRepository::default();
            *repo.create_error.lock().unwrap() =
                Some(DpfError::KubeError(kube::Error::Api(Box::new(status))));
            let sdk = DpfSdkBuilder::new(repo.clone(), "dpf-test", "new-password".to_string())
                .build_without_resources()
                .await
                .map_err(|error| error.to_string())?;
            let error = ensure_device_bmc_credential(&sdk, store, device_id)
                .await
                .expect_err("the repository rejects Secret creation");
            assert!(repo.secret.lock().unwrap().is_none());
            let (retry, source) = match error {
                DpfError::BmcCredentialPublicationRetry(source) => (true, *source),
                error => (false, error),
            };
            let DpfError::KubeError(kube::Error::Api(status)) = source else {
                panic!("publication must preserve the original API error: {source:?}");
            };
            Ok::<_, String>((retry, *status))
        }
    })
    .await;
    Ok(())
}
