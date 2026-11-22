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

use bmc_drivers::CatalogError;
use bmc_platform::{Accounts, AuthError, DriverOutcome, PlatformError};
use bmc_runtime::{BoundDriver, ConnectedBmc};
use carbide_redfish::libredfish::{
    RedfishAuth, RedfishClientCreationError, RedfishClientPool, probe_bmc_vendor, redact_password,
    redact_passwords,
};
use carbide_redfish::nv_redfish::RedfishBmc;
use carbide_secrets::credentials::Credentials;
use libredfish::RoleId;
use libredfish::model::service_root::RedfishVendor;
use model::site_explorer::EndpointExplorationReport;
use nv_redfish::account::ManagerAccountCreate;
use nv_redfish::schema::manager_account::ManagerAccount;

use crate::convert::platform_error;

/// A BMC connection authenticated with explicit caller-supplied credentials,
/// for account operations (`list`, `create`, `delete`, `change_password`,
/// `apply_default_policy`).
pub enum BackendAccountBmc {
    Libredfish(LibredfishAccounts),
    Drivers(Box<ConnectedBmc<RedfishBmc>>),
}

impl BackendAccountBmc {
    /// See [`bmc_runtime::ConnectedBmc::accounts`].
    pub fn accounts(&self) -> Result<BackendAccounts<'_>, CatalogError> {
        match self {
            Self::Libredfish(client) => Ok(BackendAccounts::Libredfish(client)),
            Self::Drivers(bmc) => bmc.accounts().map(BackendAccounts::Drivers),
        }
    }
}

/// Libredfish-backed account operations using explicit credentials on the
/// direct pool. Removed when libredfish is deleted.
pub struct LibredfishAccounts {
    pool: Arc<dyn RedfishClientPool>,
    host: String,
    port: Option<u16>,
    credentials: Credentials,
    report_vendor: Option<RedfishVendor>,
}

impl LibredfishAccounts {
    pub(crate) fn new(
        pool: Arc<dyn RedfishClientPool>,
        access: &carbide_utils::redfish::BmcAccessInfo,
        credentials: Credentials,
        report: Option<&EndpointExplorationReport>,
    ) -> Result<Self, RedfishClientCreationError> {
        let Credentials::UsernamePassword { password, .. } = &credentials;
        if password.is_empty() {
            return Err(RedfishClientCreationError::MissingCredentials {
                key: "password".to_string(),
            });
        }
        let report_vendor = report.and_then(vendor_from_report);
        Ok(Self {
            pool,
            host: access.host.clone(),
            port: access.port,
            credentials,
            report_vendor,
        })
    }

    fn username_password(&self) -> (&str, &str) {
        let Credentials::UsernamePassword { username, password } = &self.credentials;
        (username.as_str(), password.as_str())
    }

    async fn resolve_vendor(&self) -> Result<RedfishVendor, PlatformError> {
        if let Some(vendor) = self.report_vendor {
            return Ok(vendor);
        }
        let (_, curr_password) = self.username_password();
        probe_bmc_vendor(
            self.pool.as_ref(),
            &self.host,
            self.port,
            self.credentials.clone(),
        )
        .await
        .map_err(|err| {
            client_creation_error(err)
                .map_messages(&mut |text| carbide_utils::redfish::mask_all(text, [curr_password]))
        })
    }

    async fn create_client(
        &self,
        vendor: Option<RedfishVendor>,
    ) -> Result<Box<dyn libredfish::Redfish>, PlatformError> {
        let (username, password) = self.username_password();
        self.pool
            .create_client(
                &self.host,
                self.port,
                RedfishAuth::Direct(username.to_string(), password.to_string()),
                vendor,
            )
            .await
            .map_err(|err| {
                client_creation_error(err)
                    .map_messages(&mut |text| carbide_utils::redfish::mask_all(text, [password]))
            })
    }
}

/// The account operations of a [`BackendAccountBmc`].
pub enum BackendAccounts<'a> {
    Libredfish(&'a LibredfishAccounts),
    Drivers(BoundDriver<'a, dyn Accounts<RedfishBmc>, RedfishBmc>),
}

impl BackendAccounts<'_> {
    /// See [`Accounts::list`].
    pub async fn list(&self) -> Result<Vec<Arc<ManagerAccount>>, PlatformError> {
        match self {
            Self::Libredfish(client) => {
                let (_, curr_password) = client.username_password();
                let rf = client.create_client(Some(RedfishVendor::Unknown)).await?;
                let accounts = rf
                    .get_accounts()
                    .await
                    .map_err(|err| platform_error(redact_password(err, curr_password)))?;
                let mut converted = Vec::with_capacity(accounts.len());
                for account in accounts {
                    let value = serde_json::json!({
                        "@odata.id": account.odata.odata_id,
                        "@odata.type": account.odata.odata_type,
                        "Id": account.id.unwrap_or_default(),
                        "Name": account.name.unwrap_or_default(),
                        "UserName": account.username,
                        "RoleId": account.role_id,
                        "Enabled": account.enabled,
                        "AccountTypes": [],
                    });
                    if let Ok(parsed) = serde_json::from_value::<ManagerAccount>(value) {
                        converted.push(Arc::new(parsed));
                    }
                }
                Ok(converted)
            }
            Self::Drivers(accounts) => accounts.list().await,
        }
    }

    /// See [`Accounts::create`].
    pub async fn create(
        &self,
        request: ManagerAccountCreate,
    ) -> Result<DriverOutcome, PlatformError> {
        let new_password = request.password.clone();
        match self {
            Self::Libredfish(client) => {
                let (_, curr_password) = client.username_password();
                let role_id = parse_role_id(&request.role_id)?;
                let rf = client.create_client(None).await?;
                rf.create_user(&request.user_name, &request.password, role_id)
                    .await
                    .map(|_| DriverOutcome::complete())
                    .map_err(|err| {
                        platform_error(redact_passwords(err, &[&new_password, curr_password]))
                    })
            }
            Self::Drivers(accounts) => accounts.create(request).await,
        }
    }

    /// See [`Accounts::delete`].
    pub async fn delete(&self, username: &str) -> Result<DriverOutcome, PlatformError> {
        match self {
            Self::Libredfish(client) => {
                let (_, curr_password) = client.username_password();
                let rf = client.create_client(None).await?;
                rf.delete_user(username)
                    .await
                    .map(|_| DriverOutcome::complete())
                    .map_err(|err| platform_error(redact_password(err, curr_password)))
            }
            Self::Drivers(accounts) => accounts.delete(username).await,
        }
    }

    /// See [`Accounts::change_password`]. Redacts passwords from any returned error.
    pub async fn change_password(
        &self,
        account_username: &str,
        password: &str,
    ) -> Result<DriverOutcome, PlatformError> {
        match self {
            Self::Libredfish(client) => {
                let (curr_user, curr_password) = client.username_password();
                let secrets = [password, curr_password];
                let rf = client.create_client(Some(RedfishVendor::Unknown)).await?;

                // Changing the BF4 `service` account while authenticated as root
                // always patches account id `"service"` directly without a vendor probe.
                if account_username == "service" && curr_user != "service" {
                    rf.change_password_by_id("service", password)
                        .await
                        .map_err(|err| platform_error(redact_passwords(err, &secrets)))?;
                    return Ok(DriverOutcome::complete());
                }

                let vendor = client.resolve_vendor().await?;
                match vendor {
                    RedfishVendor::Lenovo => {
                        rf.change_password_by_id("1", password)
                            .await
                            .map_err(|err| platform_error(redact_passwords(err, &secrets)))?;
                    }
                    RedfishVendor::NvidiaDpu
                    | RedfishVendor::NvidiaGH200
                    | RedfishVendor::NvidiaGBSwitch
                    | RedfishVendor::P3809
                    | RedfishVendor::LiteOnPowerShelf
                    | RedfishVendor::DeltaPowerShelf
                    | RedfishVendor::NvidiaGBx00
                    | RedfishVendor::VeraRubin => {
                        rf.change_password_by_id(account_username, password)
                            .await
                            .map_err(|err| platform_error(redact_passwords(err, &secrets)))?;
                    }
                    RedfishVendor::AMI
                    | RedfishVendor::LenovoAMI
                    | RedfishVendor::LenovoGB300
                    | RedfishVendor::GigaComputingAMI => {
                        match rf.change_password(account_username, password).await {
                            Ok(()) => {}
                            Err(libredfish::RedfishError::PasswordChangeRequired {
                                account_uri,
                            }) => {
                                let account_id = account_uri
                                    .as_deref()
                                    .and_then(|uri| uri.rsplit_once('/').map(|(_, id)| id))
                                    .filter(|id| !id.is_empty())
                                    .unwrap_or("2");
                                rf.change_password_by_id(account_id, password)
                                    .await
                                    .map_err(|err| {
                                        platform_error(redact_passwords(err, &secrets))
                                    })?;
                            }
                            Err(err) => {
                                return Err(platform_error(redact_passwords(err, &secrets)));
                            }
                        }
                    }
                    RedfishVendor::Supermicro | RedfishVendor::Dell | RedfishVendor::Hpe => {
                        rf.change_password(account_username, password)
                            .await
                            .map_err(|err| platform_error(redact_passwords(err, &secrets)))?;
                    }
                    RedfishVendor::Sushy => {}
                    RedfishVendor::Unknown => {
                        return Err(platform_error(libredfish::RedfishError::MissingVendor));
                    }
                }
                Ok(DriverOutcome::complete())
            }
            Self::Drivers(accounts) => accounts.change_password(account_username, password).await,
        }
    }

    /// See [`Accounts::apply_default_policy`]. Redacts `password` from any returned error.
    pub async fn apply_default_policy(
        &self,
        account_username: &str,
        password: &str,
    ) -> Result<DriverOutcome, PlatformError> {
        match self {
            Self::Libredfish(client) => {
                let (_, curr_password) = client.username_password();
                let secrets = [password, curr_password];
                let vendor = client.resolve_vendor().await?;
                if vendor == RedfishVendor::Sushy {
                    return Ok(DriverOutcome::complete());
                }
                let rf = client.create_client(Some(vendor)).await?;
                if vendor == RedfishVendor::Lenovo {
                    rf.enable_ipmi_account_access("1", password)
                        .await
                        .map_err(|err| platform_error(redact_passwords(err, &secrets)))?;
                }
                rf.set_machine_password_policy()
                    .await
                    .map_err(|err| platform_error(redact_passwords(err, &secrets)))?;
                Ok(DriverOutcome::complete())
            }
            Self::Drivers(accounts) => {
                accounts
                    .apply_default_policy(account_username, password)
                    .await
            }
        }
    }
}

fn parse_role_id(role_id: &str) -> Result<RoleId, PlatformError> {
    match role_id.to_ascii_lowercase().as_str() {
        "administrator" => Ok(RoleId::Administrator),
        "operator" => Ok(RoleId::Operator),
        "readonly" => Ok(RoleId::ReadOnly),
        "noaccess" => Ok(RoleId::NoAccess),
        other => Err(PlatformError::InvalidResponse {
            message: format!("unsupported BMC account role: {other}"),
        }),
    }
}

/// Maps a libredfish [`RedfishClientCreationError`] into [`PlatformError`] so
/// callers handle one error type on both backends.
pub(crate) fn client_creation_error(error: RedfishClientCreationError) -> PlatformError {
    match error {
        RedfishClientCreationError::RedfishError(err) => platform_error(err),
        RedfishClientCreationError::MissingCredentials { .. } => {
            PlatformError::Auth(AuthError::CredentialsUnavailable)
        }
        other => PlatformError::InvalidResponse {
            message: other.to_string(),
        },
    }
}

/// Projects a [`RedfishVendor`] from a stored exploration report when its
/// service root or power-shelf chassis identifies one.
fn vendor_from_report(report: &EndpointExplorationReport) -> Option<RedfishVendor> {
    if let Some(root) = &report.service_root {
        let oem = (!root.oem_keys.is_empty()).then(|| {
            root.oem_keys
                .iter()
                .map(|key| (key.clone(), serde_json::json!({})))
                .collect()
        });
        let sr = libredfish::model::service_root::ServiceRoot {
            vendor: root.vendor.clone(),
            product: root.product.clone(),
            oem,
            ..Default::default()
        };
        if let Some(vendor) = sr.vendor()
            && vendor != RedfishVendor::Unknown
        {
            return Some(vendor);
        }
    }
    for chassis in &report.chassis {
        if let Some(manufacturer) = &chassis.manufacturer {
            let lc = manufacturer.to_lowercase();
            if lc.contains("lite-on") {
                return Some(RedfishVendor::LiteOnPowerShelf);
            } else if lc.contains("delta") {
                return Some(RedfishVendor::DeltaPowerShelf);
            }
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use bmc_platform::{AuthError, PlatformError};
    use carbide_redfish::libredfish::test_support::RedfishSim;
    use carbide_secrets::credentials::Credentials;
    use carbide_utils::redfish::BmcAccessInfo;
    use libredfish::model::service_root::RedfishVendor;
    use mac_address::MacAddress;
    use model::site_explorer::{Chassis, EndpointExplorationReport, ServiceRoot};

    use super::*;
    use crate::BmcConnector;

    fn access() -> BmcAccessInfo {
        BmcAccessInfo {
            host: "127.0.0.1".to_string(),
            port: Some(443),
            mac_address: MacAddress::new([2, 0, 0, 0, 0, 1]),
        }
    }

    #[tokio::test]
    async fn libredfish_accounts_change_password_and_policy_redacts_errors() {
        let sim = Arc::new(RedfishSim::default());
        sim.set_enforce_auth(true);
        sim.seed_user("1", "factory-secret");
        sim.set_service_root_vendor(Some("Lenovo".to_string()));
        let connector = BmcConnector::Libredfish(sim.clone());

        let report = EndpointExplorationReport {
            service_root: Some(ServiceRoot {
                vendor: Some("Lenovo".to_string()),
                ..Default::default()
            }),
            ..Default::default()
        };
        let bmc = connector
            .connect_with_credentials(&access(), &report, Credentials::new("1", "factory-secret"))
            .await
            .expect("connects");
        bmc.accounts()
            .expect("accounts")
            .change_password("1", "new-secret")
            .await
            .expect("changes Lenovo account 1 password");

        let new_bmc = connector
            .connect_with_credentials(&access(), &report, Credentials::new("1", "new-secret"))
            .await
            .expect("connects with new password");
        new_bmc
            .accounts()
            .expect("accounts")
            .apply_default_policy("1", "new-secret")
            .await
            .expect("applies policy and enables Lenovo IPMI");

        assert_eq!(sim.user_password("1").as_deref(), Some("new-secret"));
        assert!(sim.ipmi_account_access_enabled("1"));

        // Errors redact both current and new passwords.
        sim.set_change_password_error("rejected factory-secret and super-secret");
        let err = new_bmc
            .accounts()
            .expect("accounts")
            .change_password("1", "super-secret")
            .await
            .expect_err("change fails");
        assert!(!err.to_string().contains("super-secret"));
        assert!(!err.to_string().contains("new-secret"));
    }

    #[tokio::test]
    async fn libredfish_accounts_list_classifies_auth_failures() {
        let sim = Arc::new(RedfishSim::default());
        sim.set_enforce_auth(true);
        sim.seed_user("root", "good");
        let connector = BmcConnector::Libredfish(sim);

        let ok_bmc = connector
            .connect_unexplored_with_credentials(&access(), Credentials::new("root", "good"))
            .await
            .expect("connects");
        assert!(ok_bmc.accounts().expect("accounts").list().await.is_ok());

        let bad_bmc = connector
            .connect_unexplored_with_credentials(&access(), Credentials::new("root", "wrong"))
            .await
            .expect("connects lazily");
        let err = bad_bmc
            .accounts()
            .expect("accounts")
            .list()
            .await
            .expect_err("wrong credentials fail");
        assert!(matches!(
            err,
            PlatformError::Auth(AuthError::InvalidCredentials)
        ));
    }

    #[test]
    fn vendor_from_report_resolves_service_root_and_power_shelf_chassis() {
        let dell = EndpointExplorationReport {
            service_root: Some(ServiceRoot {
                vendor: Some("Dell".to_string()),
                ..Default::default()
            }),
            ..Default::default()
        };
        assert_eq!(vendor_from_report(&dell), Some(RedfishVendor::Dell));

        let liteon = EndpointExplorationReport {
            chassis: vec![Chassis {
                manufacturer: Some("LITE-ON TECHNOLOGY CORP.".to_string()),
                ..Default::default()
            }],
            ..Default::default()
        };
        assert_eq!(
            vendor_from_report(&liteon),
            Some(RedfishVendor::LiteOnPowerShelf)
        );
    }

    #[test]
    fn parse_role_id_rejects_unknown_roles() {
        assert!(matches!(
            parse_role_id("Administrator"),
            Ok(RoleId::Administrator)
        ));
        assert!(matches!(parse_role_id("Operator"), Ok(RoleId::Operator)));
        assert!(matches!(parse_role_id("ReadOnly"), Ok(RoleId::ReadOnly)));
        assert!(matches!(parse_role_id("NoAccess"), Ok(RoleId::NoAccess)));
        assert!(parse_role_id("Auditor").is_err());
    }
}
