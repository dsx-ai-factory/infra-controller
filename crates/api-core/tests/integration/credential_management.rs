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

use std::sync::Arc;

use carbide_api_core::AuthContext;
use carbide_api_core::cfg::file::{CarbideConfig, UfmCredentialSource};
use carbide_api_core::test_support::{
    MAX_BGP_PASSWORD_LENGTH, default_config, default_credential_key,
};
use carbide_secrets::chained_reader::UFM_LOCAL_CREDENTIAL_REMEDIATION;
use carbide_secrets::credentials::{
    BgpCredentialType, BmcCredentialType, CompositeCredentialManager, CredentialKey,
    CredentialManager, CredentialReader, CredentialType, CredentialWriter, Credentials,
    UfmCredentialMutationBlocker,
};
use carbide_secrets::test_support::credentials::TestCredentialManager;
use carbide_test_harness::prelude::*;
use mac_address::MacAddress;
use rpc::forge::{
    CredentialCreationRequest, CredentialDeletionRequest, CredentialType as RpcCredentialType,
    GetBmcCredentialsRequest,
};
use tonic::Code;

async fn init(pool: PgPool) -> (TestHarness, Arc<TestCredentialManager>) {
    init_with_runtime_config(pool, default_config::get()).await
}

async fn init_with_runtime_config(
    pool: PgPool,
    runtime_config: CarbideConfig,
) -> (TestHarness, Arc<TestCredentialManager>) {
    let credential_manager = Arc::new(TestCredentialManager::default());
    let writer: Arc<dyn CredentialWriter> = if runtime_config
        .credentials
        .uses_authoritative_local_ufm_credentials()
    {
        Arc::new(UfmCredentialMutationBlocker::new(
            credential_manager.clone(),
        ))
    } else {
        credential_manager.clone()
    };
    let api_credential_manager: Arc<dyn CredentialManager> = Arc::new(
        CompositeCredentialManager::new(credential_manager.clone(), writer),
    );
    let env = TestHarness::builder(pool)
        .with_api_builder_fn(move |builder| {
            builder
                .with_credential_manager(api_credential_manager)
                .with_runtime_config(Arc::new(runtime_config))
        })
        .build()
        .await;
    (env, credential_manager)
}

fn config_with_ufm_source(ufm_source: UfmCredentialSource) -> CarbideConfig {
    let mut config = default_config::get();
    config.credentials.ufm_source = ufm_source;
    config
}

fn ufm_create_request() -> CredentialCreationRequest {
    CredentialCreationRequest {
        credential_type: RpcCredentialType::Ufm.into(),
        username: Some("https://ufm.example.com".to_string()),
        password: "test-ufm-token".to_string(),
        vendor: None,
        mac_address: None,
        credential_name: None,
    }
}

fn ufm_delete_request() -> CredentialDeletionRequest {
    CredentialDeletionRequest {
        credential_type: RpcCredentialType::Ufm.into(),
        username: Some("https://ufm.example.com".to_string()),
        mac_address: None,
        credential_name: None,
    }
}

fn firmware_artifact_token_create_request(name: &str, token: &str) -> CredentialCreationRequest {
    CredentialCreationRequest {
        credential_type: RpcCredentialType::FirmwareArtifactAccessToken.into(),
        username: None,
        password: token.to_string(),
        vendor: None,
        mac_address: None,
        credential_name: Some(name.to_string()),
    }
}

#[sqlx_test]
async fn firmware_artifact_access_token_can_be_replaced_and_deleted(pool: PgPool) {
    let (env, credential_manager) = init(pool).await;
    let name = "repository-a".to_string();
    let key = CredentialKey::FirmwareArtifactAccessToken { name: name.clone() };

    for token in ["first-token", " rotated-token \r\n"] {
        env.api()
            .create_credential(tonic::Request::new(firmware_artifact_token_create_request(
                &name, token,
            )))
            .await
            .expect("set firmware artifact access token");
    }

    assert_eq!(
        credential_manager
            .get_credentials_from_writer(&key)
            .await
            .expect("read stored token"),
        Some(Credentials::new("", " rotated-token \r\n"))
    );

    env.api()
        .delete_credential(tonic::Request::new(CredentialDeletionRequest {
            credential_type: RpcCredentialType::FirmwareArtifactAccessToken.into(),
            username: None,
            mac_address: None,
            credential_name: Some(name.to_string()),
        }))
        .await
        .expect("delete firmware artifact access token");

    assert_eq!(
        credential_manager
            .get_credentials_from_writer(&key)
            .await
            .expect("read deleted token"),
        None
    );
}

#[sqlx_test]
async fn firmware_artifact_access_token_rejects_missing_name_or_token(pool: PgPool) {
    let (env, _) = init(pool).await;

    let mut missing_name = firmware_artifact_token_create_request("repository-a", "token");
    missing_name.credential_name = None;

    for request in [
        missing_name,
        firmware_artifact_token_create_request("", "token"),
        firmware_artifact_token_create_request("repository-a", ""),
    ] {
        let error = env
            .api()
            .create_credential(tonic::Request::new(request))
            .await
            .expect_err("invalid firmware artifact credential input must fail");

        assert_eq!(error.code(), Code::InvalidArgument);
    }

    for credential_name in [None, Some(String::new())] {
        let error = env
            .api()
            .delete_credential(tonic::Request::new(CredentialDeletionRequest {
                credential_type: RpcCredentialType::FirmwareArtifactAccessToken.into(),
                username: None,
                mac_address: None,
                credential_name,
            }))
            .await
            .expect_err("invalid firmware artifact credential name must fail");

        assert_eq!(error.code(), Code::InvalidArgument);
    }
}

#[sqlx_test]
async fn test_ufm_credential_mutations_reject_local_ownership(pool: PgPool) {
    let (env, credential_manager) =
        init_with_runtime_config(pool, config_with_ufm_source(UfmCredentialSource::Local)).await;
    let key = CredentialKey::UfmAuth {
        fabric: "default".to_string(),
    };

    let create_error = env
        .api()
        .create_credential(tonic::Request::new(ufm_create_request()))
        .await
        .expect_err("local UFM ownership must reject backend create");
    assert_eq!(create_error.code(), Code::FailedPrecondition);
    assert!(
        create_error
            .message()
            .contains("local sources own all UFM credentials")
    );
    assert!(
        create_error
            .message()
            .contains(UFM_LOCAL_CREDENTIAL_REMEDIATION)
    );
    assert_eq!(
        credential_manager
            .get_credentials_from_writer(&key)
            .await
            .expect("read backend after rejected create"),
        None
    );

    let existing = Credentials::UsernamePassword {
        username: "legacy-ufm".to_string(),
        password: "existing-token".to_string(),
    };
    credential_manager
        .set_credentials(&key, &existing)
        .await
        .expect("seed persistent UFM credential");

    let delete_error = env
        .api()
        .delete_credential(tonic::Request::new(ufm_delete_request()))
        .await
        .expect_err("local UFM ownership must reject backend delete");
    assert_eq!(delete_error.code(), Code::FailedPrecondition);
    assert_eq!(delete_error.message(), create_error.message());
    assert_eq!(
        credential_manager
            .get_credentials_from_writer(&key)
            .await
            .expect("read backend after rejected delete"),
        Some(existing)
    );
}

#[sqlx_test]
async fn test_ufm_credential_mutations_allow_backend_ownership(pool: PgPool) {
    let (env, credential_manager) =
        init_with_runtime_config(pool, config_with_ufm_source(UfmCredentialSource::Backend)).await;
    let key = CredentialKey::UfmAuth {
        fabric: "default".to_string(),
    };

    env.api()
        .create_credential(tonic::Request::new(ufm_create_request()))
        .await
        .expect("backend ownership must allow backend create");
    assert_eq!(
        credential_manager
            .get_credentials_from_writer(&key)
            .await
            .expect("read backend after create"),
        Some(Credentials::UsernamePassword {
            username: "https://ufm.example.com".to_string(),
            password: "test-ufm-token".to_string(),
        })
    );

    env.api()
        .delete_credential(tonic::Request::new(ufm_delete_request()))
        .await
        .expect("backend ownership must allow backend delete");
    assert_eq!(
        credential_manager
            .get_credentials_from_writer(&key)
            .await
            .expect("read backend after delete"),
        Some(Credentials::UsernamePassword {
            username: "https://ufm.example.com".to_string(),
            password: String::new(),
        })
    );
}

#[sqlx_test]
async fn test_create_host_uefi_credential_when_missing(pool: PgPool) {
    let (env, credential_manager) = init(pool).await;

    let response = env
        .api()
        .create_credential(tonic::Request::new(CredentialCreationRequest {
            credential_type: RpcCredentialType::HostUefi.into(),
            username: None,
            password: "test-host-uefi-password".to_string(),
            vendor: None,
            mac_address: None,
            credential_name: None,
        }))
        .await;
    assert!(response.is_ok());

    let stored = credential_manager
        .get_credentials(&CredentialKey::HostUefi {
            credential_type: CredentialType::SiteDefault,
        })
        .await
        .unwrap();
    assert_eq!(
        stored,
        Some(Credentials::UsernamePassword {
            username: "".to_string(),
            password: "test-host-uefi-password".to_string(),
        })
    );

    // A second create should fail because the credential now exists.
    let second = env
        .api()
        .create_credential(tonic::Request::new(CredentialCreationRequest {
            credential_type: RpcCredentialType::HostUefi.into(),
            username: None,
            password: "another-password".to_string(),
            vendor: None,
            mac_address: None,
            credential_name: None,
        }))
        .await;
    assert!(second.is_err());
    assert_eq!(second.unwrap_err().code(), Code::AlreadyExists);
}

#[sqlx_test]
async fn test_create_dpu_uefi_credential_when_missing(pool: PgPool) {
    let (env, credential_manager) = init(pool).await;

    let response = env
        .api()
        .create_credential(tonic::Request::new(CredentialCreationRequest {
            credential_type: RpcCredentialType::DpuUefi.into(),
            username: None,
            password: "test-dpu-uefi-password".to_string(),
            vendor: None,
            mac_address: None,
            credential_name: None,
        }))
        .await;
    assert!(response.is_ok());

    let stored = credential_manager
        .get_credentials(&CredentialKey::DpuUefi {
            credential_type: CredentialType::SiteDefault,
        })
        .await
        .unwrap();
    assert_eq!(
        stored,
        Some(Credentials::UsernamePassword {
            username: "".to_string(),
            password: "test-dpu-uefi-password".to_string(),
        })
    );

    // A second create should fail because the credential now exists.
    let second = env
        .api()
        .create_credential(tonic::Request::new(CredentialCreationRequest {
            credential_type: RpcCredentialType::DpuUefi.into(),
            username: None,
            password: "another-password".to_string(),
            vendor: None,
            mac_address: None,
            credential_name: None,
        }))
        .await;
    assert!(second.is_err());
    assert_eq!(second.unwrap_err().code(), Code::AlreadyExists);
}

#[sqlx_test]
async fn test_create_and_delete_bgp_credential(pool: PgPool) {
    let (env, credential_manager) = init(pool).await;

    // Create the site-wide DPU BGP credential.
    let response = env
        .api()
        .create_credential(tonic::Request::new(CredentialCreationRequest {
            credential_type: RpcCredentialType::BgpSiteWideLeafPassword.into(),
            username: None,
            password: "test-dpu-bgp-password".to_string(),
            vendor: None,
            mac_address: None,
            credential_name: None,
        }))
        .await;
    assert!(response.is_ok());

    // Verify the credential was stored in the credential manager.
    let stored = credential_manager
        .get_credentials(&CredentialKey::Bgp {
            credential_type: BgpCredentialType::SiteWideLeafPassword,
        })
        .await
        .unwrap();
    assert_eq!(
        stored,
        Some(Credentials::UsernamePassword {
            username: "".to_string(),
            password: "test-dpu-bgp-password".to_string(),
        })
    );

    // Delete the site-wide DPU BGP credential.
    let delete_response = env
        .api()
        .delete_credential(tonic::Request::new(CredentialDeletionRequest {
            credential_type: RpcCredentialType::BgpSiteWideLeafPassword.into(),
            username: None,
            mac_address: None,
            credential_name: None,
        }))
        .await;
    assert!(delete_response.is_ok());

    // Verify the credential was removed from the credential manager.
    let deleted = credential_manager
        .get_credentials(&CredentialKey::Bgp {
            credential_type: BgpCredentialType::SiteWideLeafPassword,
        })
        .await
        .unwrap();
    assert_eq!(deleted, None);
}

#[sqlx_test]
async fn test_get_bmc_credentials_rejects_caller_without_spiffe_service_id(pool: PgPool) {
    let (env, _credential_manager) = init(pool).await;

    // No `AuthContext` extension attached -> no SPIFFE service identity ->
    // PermissionDenied. We do not need a real BMC, machine record, or
    // populated credentials for this assertion because the SPIFFE check
    // happens before any of those lookups.
    let mut request = tonic::Request::new(GetBmcCredentialsRequest {
        mac_addr: "11:22:33:44:55:66".to_string(),
    });
    request.extensions_mut().insert(AuthContext::default());

    let err = env
        .api()
        .get_bmc_credentials(request)
        .await
        .expect_err("caller without SPIFFE service id should be rejected");
    assert_eq!(err.code(), Code::PermissionDenied);
}

#[sqlx_test]
async fn test_create_bgp_credential_validates_max_password_length(pool: PgPool) {
    let (env, credential_manager) = init(pool).await;

    // Create a site-wide DPU BGP credential using the maximum supported length.
    let max_password = "a".repeat(MAX_BGP_PASSWORD_LENGTH);
    let ok_response = env
        .api()
        .create_credential(tonic::Request::new(CredentialCreationRequest {
            credential_type: RpcCredentialType::BgpSiteWideLeafPassword.into(),
            username: None,
            password: max_password.clone(),
            vendor: None,
            mac_address: None,
            credential_name: None,
        }))
        .await;
    assert!(ok_response.is_ok());

    // Verify the credential was stored unchanged.
    let stored = credential_manager
        .get_credentials(&CredentialKey::Bgp {
            credential_type: BgpCredentialType::SiteWideLeafPassword,
        })
        .await
        .unwrap();
    assert_eq!(
        stored,
        Some(Credentials::UsernamePassword {
            username: "".to_string(),
            password: max_password,
        })
    );

    // Try to create a site-wide DPU BGP credential longer than the supported maximum.
    let response = env
        .api()
        .create_credential(tonic::Request::new(CredentialCreationRequest {
            credential_type: RpcCredentialType::BgpSiteWideLeafPassword.into(),
            username: None,
            password: "a".repeat(MAX_BGP_PASSWORD_LENGTH + 1),
            vendor: None,
            mac_address: None,
            credential_name: None,
        }))
        .await;
    let err = response.expect_err("passwords longer than the max should be rejected");

    // Verify the handler returns a validation error.
    assert_eq!(err.code(), Code::InvalidArgument);
    assert!(err.message().contains(&format!(
        "BGP password length exceeds {MAX_BGP_PASSWORD_LENGTH} characters"
    )));

    // Verify the previously stored credential was left unchanged.
    let stored = credential_manager
        .get_credentials(&CredentialKey::Bgp {
            credential_type: BgpCredentialType::SiteWideLeafPassword,
        })
        .await
        .unwrap();
    assert_eq!(
        stored,
        Some(Credentials::UsernamePassword {
            username: "".to_string(),
            password: "a".repeat(MAX_BGP_PASSWORD_LENGTH),
        })
    );
}

#[sqlx_test]
async fn test_get_switch_nvos_credentials(pool: PgPool) -> eyre::Result<()> {
    let (env, credential_manager) = init(pool).await;
    let bmc_mac_address: MacAddress = "6A:6B:6C:6D:6E:A1".parse()?;
    let switch = env
        .create_expected_switch(rpc::forge::ExpectedSwitch {
            bmc_mac_address: bmc_mac_address.to_string(),
            nvos_mac_addresses: vec!["7A:7B:7C:7D:7E:A1".to_string()],
            bmc_username: "ADMIN".to_string(),
            bmc_password: "PASS".to_string(),
            switch_serial_number: "CREDENTIAL-SWITCH-001".to_string(),
            metadata: Some(rpc::forge::Metadata {
                name: "Switch1".to_string(),
                ..Default::default()
            }),
            ..Default::default()
        })
        .await
        .create_switch(0, 0)
        .await;

    credential_manager
        .set_credentials(
            &CredentialKey::SwitchNvosAdmin { bmc_mac_address },
            &Credentials::UsernamePassword {
                username: "nvos-admin".to_string(),
                password: "nvos-secret".to_string(),
            },
        )
        .await?;

    let response = env
        .api()
        .get_switch_nvos_credentials(tonic::Request::new(
            rpc::forge::GetSwitchNvosCredentialsRequest {
                switch_id: Some(switch.id),
            },
        ))
        .await?
        .into_inner();

    let credentials = response.credentials.expect("credentials");
    let Some(rpc::forge::bmc_credentials::Type::UsernamePassword(username_password)) =
        credentials.r#type
    else {
        panic!("expected username/password credentials");
    };

    assert_eq!(username_password.username, "nvos-admin");
    assert_eq!(username_password.password, "nvos-secret");

    Ok(())
}

#[sqlx_test]
async fn test_missing_default_credentials(pool: PgPool) {
    let (env, credential_manager) = init(pool).await;

    let bmc_root = CredentialKey::BmcCredentials {
        credential_type: BmcCredentialType::SiteWideRoot,
    };
    let host_uefi = CredentialKey::HostUefi {
        credential_type: CredentialType::SiteDefault,
    };
    let dpu_uefi = CredentialKey::DpuUefi {
        credential_type: CredentialType::SiteDefault,
    };
    let bmc_root_key = bmc_root.to_key_str().to_string();
    let host_uefi_key = host_uefi.to_key_str().to_string();
    let dpu_uefi_key = dpu_uefi.to_key_str().to_string();

    let creds = |password: &str| Credentials::UsernamePassword {
        username: String::new(),
        password: password.to_string(),
    };

    // A fresh environment has none of the site-wide defaults configured.
    let keys: Vec<String> = env
        .api()
        .missing_default_credentials()
        .await
        .into_iter()
        .map(|c| default_credential_key(&c).to_owned())
        .collect();
    assert_eq!(keys.len(), 3, "expected all defaults missing, got {keys:?}");
    assert!(keys.contains(&bmc_root_key));
    assert!(keys.contains(&host_uefi_key));
    assert!(keys.contains(&dpu_uefi_key));

    // Configuring one credential removes it from the missing set.
    credential_manager
        .set_credentials(&bmc_root, &creds("bmc-root-pw"))
        .await
        .unwrap();
    let keys: Vec<String> = env
        .api()
        .missing_default_credentials()
        .await
        .into_iter()
        .map(|c| default_credential_key(&c).to_owned())
        .collect();
    assert_eq!(keys.len(), 2, "got {keys:?}");
    assert!(!keys.contains(&bmc_root_key));

    // An empty password still counts as "not configured".
    credential_manager
        .set_credentials(&host_uefi, &creds(""))
        .await
        .unwrap();
    let keys: Vec<String> = env
        .api()
        .missing_default_credentials()
        .await
        .into_iter()
        .map(|c| default_credential_key(&c).to_owned())
        .collect();
    assert!(
        keys.contains(&host_uefi_key),
        "empty password must count as missing, got {keys:?}"
    );

    // Configuring the remaining two with real passwords clears the warning.
    credential_manager
        .set_credentials(&host_uefi, &creds("host-uefi-pw"))
        .await
        .unwrap();
    credential_manager
        .set_credentials(&dpu_uefi, &creds("dpu-uefi-pw"))
        .await
        .unwrap();
    let missing = env.api().missing_default_credentials().await;
    assert!(
        missing.is_empty(),
        "expected no missing defaults, got {missing:?}"
    );
}
