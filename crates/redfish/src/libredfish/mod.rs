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
use carbide_instrument::{Event, LabelValue, emit};
use carbide_secrets::credentials::{CredentialKey, CredentialReader, CredentialType, Credentials};
use carbide_utils::HostPortPair;
use carbide_utils::redfish::BmcAccessInfo;
pub use error::{CredentialOpError, RedfishClientCreationError};
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
fn emit_dpu_uefi_password_setup_skipped_if_needed(
    bios_attrs: &std::collections::HashMap<String, serde_json::Value>,
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

    emit(skipped);
    true
}

/// The direct pool and its credential-operations handle, backed by one
/// object. Only this constructor yields a [`BmcCredentialOps`]: it is built
/// from the raw `libredfish` pool, which an intermediary-routing pool does
/// not have -- credential operations always authenticate to the BMC itself.
pub fn new_pool_with_credential_ops(
    credential_reader: Arc<dyn CredentialReader>,
    pool: libredfish::RedfishClientPool,
    proxy_address: Arc<ArcSwap<Option<HostPortPair>>>,
) -> (Arc<dyn RedfishClientPool>, Arc<dyn BmcCredentialOps>) {
    let inner = Arc::new(implementation::RedfishClientPoolImpl::new(
        credential_reader,
        pool,
        proxy_address,
    ));
    (inner.clone(), inner)
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

// Seals `BmcCredentialOps`: implementations outside this crate would defeat
// the wrong-pool guard the trait exists to provide.
mod sealed {
    pub trait Sealed {}
}

/// Credential-lifecycle operations against BMCs and UEFI firmware: setting,
/// rotating, validating, and clearing passwords, plus the vendor probing the
/// rotation engine needs.
///
/// Split off [`RedfishClientPool`] so the type system, not call-site
/// discipline, decides who may perform them: the trait is sealed, and only
/// the direct (BMC-authenticating) pool implements it. Handing these
/// operations to a pool that routes through an intermediary such as
/// nico-bmc-proxy is then a compile error rather than a runtime guard.
/// (The clients the direct pool builds can still be *redirected* -- e.g. the
/// dynamic `site_explorer.bmc_proxy` dev redirect applies to it too -- so
/// this is a wrong-pool guard, not a wire-path guarantee.)
///
/// Every operation creates its own client from `self` (the direct pool), so
/// a caller cannot accidentally pair a credential operation with a client
/// built by some other pool.
#[async_trait]
pub trait BmcCredentialOps: RedfishClientPool + sealed::Sealed {
    /// Updates the host UEFI password from Forge's sitewide password to an
    /// empty string. Assumes the machine has already updated its UEFI
    /// password to match the Forge sitewide password.
    ///
    /// `current_device_credentials` is the credential the device currently
    /// carries (the host UEFI password to authenticate the clear with). The
    /// caller resolves it -- this low-level crate intentionally knows nothing
    /// about credential versions or the rotation table; it just applies the
    /// password it is handed.
    ///
    /// Builds its own Redfish client from `access`; [`CredentialOpError`]
    /// separates client-creation failures from device-level ones.
    async fn clear_host_uefi_password(
        &self,
        access: &BmcAccessInfo,
        current_device_credentials: Credentials,
    ) -> Result<Option<String>, CredentialOpError> {
        let client = self
            .client_by_info(access)
            .await
            .map_err(CredentialOpError::ClientCreation)?;
        let client = client.as_ref();
        let Credentials::UsernamePassword {
            password: current_password,
            ..
        } = current_device_credentials;

        client
            .clear_uefi_password(current_password.as_str())
            .await
            .map_err(|err| redact_password(err, current_password.as_str()))
            .map_err(RedfishClientCreationError::RedfishError)
            .map_err(CredentialOpError::Operation)
    }

    /// Sets the device's UEFI (BIOS setup) password to the site-wide value.
    ///
    /// `sitewide_uefi_credentials` is the site-wide UEFI credential to set on
    /// the device (host_uefi when `dpu` is false, dpu_uefi when true). The
    /// caller resolves it -- this crate knows nothing about credential
    /// versions or the rotation table. The DPU's factory-default password
    /// (the credential the device still carries before this runs) is a
    /// hardware constant, so it is still read here.
    ///
    /// Builds its own Redfish client from `access`; [`CredentialOpError`]
    /// separates client-creation failures from device-level ones.
    async fn uefi_setup(
        &self,
        access: &BmcAccessInfo,
        dpu: bool,
        sitewide_uefi_credentials: Credentials,
    ) -> Result<Option<String>, CredentialOpError> {
        let client = self
            .client_by_info(access)
            .await
            .map_err(CredentialOpError::ClientCreation)?;
        let client = client.as_ref();
        let Credentials::UsernamePassword {
            password: new_password,
            ..
        } = sitewide_uefi_credentials;
        let mut current_password = String::new();
        if dpu {
            let bios_attrs = client.bios().await.map_err(|err| {
                CredentialOpError::Operation(RedfishClientCreationError::RedfishError(err))
            })?;

            // Preserve the non-fatal return for now, but this should become a hard
            // failure once callers can reject DPUs that retain factory credentials.
            if emit_dpu_uefi_password_setup_skipped_if_needed(&bios_attrs) {
                return Ok(None);
            }
            tracing::info!(
                "BIOS Attributes found, and contains CurrentUefiPassword, continuing with UEFI password setting"
            );

            // Replace the DPU UEFI default password with the site default.
            // The current (factory) password is taken from the DpuUefi factory
            // default key -- a hardware constant, not a versioned/site credential
            // -- so it is read here; the new (site) password was handed in.
            let credentials = self
                .credential_reader()
                .get_credentials(&CredentialKey::DpuUefi {
                    credential_type: CredentialType::DpuHardwareDefault {
                        model: bmc_vendor::DpuModel::Unknown,
                    },
                })
                .await
                // A store read failure is the transient class: the
                // operation never reached the device.
                .map_err(|err| CredentialOpError::ClientCreation(err.into()))?
                .unwrap_or(Credentials::UsernamePassword {
                    username: "".to_string(),
                    password: "bluefield".to_string(),
                });

            (_, current_password) = match credentials {
                Credentials::UsernamePassword { username, password } => (username, password),
            };

            // DPU's change_uefi_password always returns Ok(None) on success (no async job).
            // Return Ok(Some("")) here so callers can distinguish "updated" from the
            // early-exit Ok(None) skip paths above. The host path below must NOT get
            // this mapping — hosts may return Ok(None) to mean "no job to poll".
            return client
                .change_uefi_password(current_password.as_str(), new_password.as_str())
                .await
                .map_err(|err| redact_password(err, new_password.as_str()))
                .map_err(|err| redact_password(err, current_password.as_str()))
                .map_err(RedfishClientCreationError::RedfishError)
                .map_err(CredentialOpError::Operation)
                .map(|job_id| Some(job_id.unwrap_or_default()));
        } else {
            // For hosts, first try with empty current password (assuming no
            // password is set), using the site default handed in by the caller.
            match client
                .change_uefi_password(current_password.as_str(), new_password.as_str())
                .await
            {
                Ok(jid) => return Ok(jid),
                Err(e) => {
                    // If the first attempt fails (likely because a password is already set),
                    // retry using the site default password as the current password.
                    // This handles the case where a host was force-deleted without clearing
                    // its UEFI password.
                    let redacted_error = redact_password(e, new_password.as_str());
                    tracing::warn!(
                        error = %redacted_error,
                        "Failed to set UEFI password with empty current password, retrying with site default password"
                    );
                    current_password = new_password.clone();
                }
            }
        }

        // Host fallback: second attempt using site default as current password.
        client
            .change_uefi_password(current_password.as_str(), new_password.as_str())
            .await
            .map_err(|err| redact_password(err, new_password.as_str()))
            .map_err(|err| redact_password(err, current_password.as_str()))
            .map_err(RedfishClientCreationError::RedfishError)
            .map_err(CredentialOpError::Operation)
    }

    /// Rotate a UEFI (BIOS setup) password to the site-wide target,
    /// authenticating with the current-version credential. Family-agnostic: the
    /// same candidate walk serves a host (via a host power-cycle) and a DPU (via
    /// a DPU restart) -- the caller owns the reboot and the bookkeeping.
    ///
    /// `current_password_candidates` is an ordered, bounded, non-empty list of
    /// plausible current passwords (the caller resolves them: the device's
    /// tracked current site version, then the rotating-to version, then the
    /// terminal fallback -- empty for a never-set host, the factory default for
    /// a never-set DPU). The first candidate that authenticates the change wins;
    /// the returned `Option<String>` is the vendor BIOS job id to poll (when the
    /// vendor schedules one) or `None` (DPUs never schedule one).
    ///
    /// Unlike [`Self::uefi_setup`], this never assumes the empty/factory case
    /// first, so a mid-rotation device at v1 rotating to v2 authenticates with
    /// the v1 secret rather than failing. This crate still knows nothing about
    /// credential versions -- it just applies the ordered candidates it is
    /// handed. All errors are password-redacted before they leave this method.
    async fn rotate_uefi_password(
        &self,
        access: &BmcAccessInfo,
        current_password_candidates: &[String],
        new_password: String,
    ) -> Result<Option<String>, CredentialOpError> {
        let client = self
            .client_by_info(access)
            .await
            .map_err(CredentialOpError::ClientCreation)?;
        let client = client.as_ref();
        let mut last_err = None;
        for candidate in current_password_candidates {
            match client
                .change_uefi_password(candidate.as_str(), new_password.as_str())
                .await
            {
                Ok(job_id) => return Ok(job_id),
                Err(e) => {
                    let redacted =
                        redact_passwords(e, &[new_password.as_str(), candidate.as_str()]);
                    tracing::warn!(
                        error = %redacted,
                        "UEFI password change failed for a current-password candidate; trying the next"
                    );
                    last_err = Some(redacted);
                }
            }
        }
        // The caller contract guarantees at least one candidate (empty for a
        // never-set host, the factory default for a never-set DPU), so `last_err`
        // is populated whenever the loop fell through without an Ok.
        Err(CredentialOpError::Operation(
            RedfishClientCreationError::RedfishError(
                last_err.expect(
                    "rotate_uefi_password requires at least one current-password candidate",
                ),
            ),
        ))
    }

    /// Rotate a BMC's root password in place, then apply the vendor-specific
    /// password policy.
    ///
    /// `current_bmc_root_credentials` is the credential the BMC currently
    /// carries (used to authenticate the change); `new_password` is the value
    /// to rotate to. The caller resolves both -- this crate knows nothing
    /// about credential versions or the rotation table, it just applies what
    /// it is handed.
    ///
    /// The rotation `PATCH` to `/AccountService` goes through an uninitialized
    /// `Unknown` client on purpose, so libredfish does not eagerly fetch
    /// `/Systems`, `/Managers`, `/Chassis` up front. Those fetches are
    /// unnecessary just to change the password, and they actively break
    /// rotation on factory BMCs that refuse reads until the password has been
    /// changed. Notably, NVIDIA GBx00 in factory state authenticates the
    /// supplied creds just fine but returns HTTP 403
    /// `Base.1.18.1.PasswordChangeRequired` on `/Systems` -- so letting
    /// libredfish initialize first never reaches the `PATCH` that would unblock
    /// it. Only the follow-up policy client (created after the rotation
    /// succeeds) is vendor-specific, so `set_machine_password_policy` gets the
    /// right impl (e.g. Lite-On's, which omits `AccountLockoutCounterResetAfter`).
    async fn set_bmc_root_password(
        &self,
        host: &str,
        port: Option<u16>,
        vendor: RedfishVendor,
        current_bmc_root_credentials: Credentials,
        new_password: String,
    ) -> Result<(), RedfishClientCreationError> {
        let (curr_user, curr_password) = match &current_bmc_root_credentials {
            Credentials::UsernamePassword { username, password } => (username, password),
        };

        let client = self
            .create_client(
                host,
                port,
                RedfishAuth::Direct(curr_user.clone(), curr_password.clone()),
                Some(RedfishVendor::Unknown),
            )
            .await?;

        match vendor {
            RedfishVendor::Lenovo => {
                // Change (factory_user, factory_pass) to (factory_user, site_pass)
                client
                    .change_password_by_id("1", new_password.as_str())
                    .await
                    .map_err(|err| redact_password(err, new_password.as_str()))
                    .map_err(|err| redact_password(err, curr_password.as_str()))
                    .map_err(RedfishClientCreationError::RedfishError)?;
            }
            RedfishVendor::NvidiaDpu
            | RedfishVendor::NvidiaGH200
            | RedfishVendor::NvidiaGBSwitch
            | RedfishVendor::P3809
            | RedfishVendor::LiteOnPowerShelf
            | RedfishVendor::DeltaPowerShelf
            | RedfishVendor::NvidiaGBx00
            | RedfishVendor::VeraRubin => {
                // change_password does things that require a password and DPUs need a first
                // password use to be change, so just change it directly
                //
                // GH200 doesn't require change-on-first-use, but it's good practice. GB200
                // probably will.
                client
                    .change_password_by_id(curr_user.as_str(), new_password.as_str())
                    .await
                    .map_err(|err| redact_password(err, new_password.as_str()))
                    .map_err(|err| redact_password(err, curr_password.as_str()))
                    .map_err(RedfishClientCreationError::RedfishError)?;
            }
            // AMI-based BMCs, including Vikings and Lenovo GB300s.
            // Resolve the admin account by username. If reads are blocked by
            // `PasswordChangeRequired`, use its account URI or fall back to id "2".
            // Any other error propagates.
            //
            // https://docs.nvidia.com/dgx/dgxh100-user-guide/redfish-api-supp.html
            RedfishVendor::AMI | RedfishVendor::LenovoAMI | RedfishVendor::LenovoGB300 => {
                match client
                    .change_password(curr_user.as_str(), new_password.as_str())
                    .await
                {
                    Ok(()) => {}
                    Err(libredfish::RedfishError::PasswordChangeRequired { account_uri }) => {
                        // For example: /redfish/v1/AccountService/Accounts/4
                        let account_id = account_uri
                            .as_deref()
                            .and_then(|uri| uri.rsplit_once('/').map(|(_, id)| id))
                            .filter(|id| !id.is_empty())
                            .unwrap_or("2");
                        client
                            .change_password_by_id(account_id, new_password.as_str())
                            .await
                            .map_err(|err| redact_password(err, new_password.as_str()))
                            .map_err(|err| redact_password(err, curr_password.as_str()))
                            .map_err(RedfishClientCreationError::RedfishError)?;
                    }
                    Err(err) => {
                        return Err(RedfishClientCreationError::RedfishError(redact_password(
                            redact_password(err, new_password.as_str()),
                            curr_password.as_str(),
                        )));
                    }
                }
            }
            RedfishVendor::Supermicro | RedfishVendor::Dell | RedfishVendor::Hpe => {
                client
                    .change_password(curr_user.as_str(), new_password.as_str())
                    .await
                    .map_err(|err| redact_password(err, new_password.as_str()))
                    .map_err(|err| redact_password(err, curr_password.as_str()))
                    .map_err(RedfishClientCreationError::RedfishError)?;
            }
            // Sushy is a development emulator without an AccountService.
            RedfishVendor::Sushy => {}
            RedfishVendor::Unknown => {
                // Defensive guard: callers resolve the vendor via
                // `probe_bmc_vendor` (or site-explorer's `get_redfish_vendor`),
                // both of which reject `Unknown` before we ever get here, so
                // this arm is not reachable from the live path.
                return Err(RedfishClientCreationError::RedfishError(
                    libredfish::RedfishError::MissingVendor,
                ));
            }
        };

        // Log in using the new credentials and set the vendor-specific password policy.
        let vendored_client = self
            .create_client(
                host,
                port,
                RedfishAuth::Direct(curr_user.to_string(), new_password),
                Some(vendor),
            )
            .await?;

        vendored_client
            .set_machine_password_policy()
            .await
            .map_err(RedfishClientCreationError::RedfishError)?;

        Ok(())
    }

    /// Rotate a BF4 DPU BMC's `service` account password using root credentials.
    async fn set_bf4_dpu_service_password(
        &self,
        host: &str,
        port: Option<u16>,
        root_credentials: Credentials,
        new_password: String,
    ) -> Result<(), RedfishClientCreationError> {
        let (root_user, root_password) = match &root_credentials {
            Credentials::UsernamePassword { username, password } => (username, password),
        };

        let client = self
            .create_client(
                host,
                port,
                RedfishAuth::Direct(root_user.clone(), root_password.clone()),
                Some(RedfishVendor::Unknown),
            )
            .await?;

        client
            .change_password_by_id("service", new_password.as_str())
            .await
            .map_err(|err| redact_password(err, new_password.as_str()))
            .map_err(|err| redact_password(err, root_password.as_str()))
            .map_err(RedfishClientCreationError::RedfishError)?;

        Ok(())
    }

    /// Whether `credentials` currently authenticate against the BMC, without
    /// changing anything.
    ///
    /// Credential rotation uses this for crash recovery: [`set_bmc_root_password`]
    /// is a single synchronous change, so a crash after the hardware password
    /// changed but before the rotation was recorded leaves the device already at
    /// the rotate-TO value. Re-issuing the change would authenticate fine but set
    /// the password to the value it already holds -- a same-value change some
    /// BMCs reject with a password-reuse policy error, which would falsely
    /// quarantine a device that is in fact converged. This probe lets the engine
    /// detect "already at target" and converge without that same-value change.
    ///
    /// Reads the account collection (`AccountService/Accounts`) through an
    /// uninitialized `Unknown` client. That endpoint is gated behind login on
    /// every vendor, and -- unlike `/Systems` -- is served even by factory BMCs
    /// that answer `403 PasswordChangeRequired` on other resources (the
    /// [`set_bmc_root_password`] path already reads it to resolve the account id),
    /// so it verifies authentication without tripping that trap.
    ///
    /// Returns `Ok(true)` when the credentials authenticate, `Ok(false)` when the
    /// BMC rejects them as unauthorized (401/403), and `Err` for a transport or
    /// other failure the caller should treat as a transient tick error.
    ///
    /// [`set_bmc_root_password`]: BmcCredentialOps::set_bmc_root_password
    async fn bmc_credentials_valid(
        &self,
        host: &str,
        port: Option<u16>,
        credentials: Credentials,
    ) -> Result<bool, RedfishClientCreationError> {
        let Credentials::UsernamePassword { username, password } = credentials;
        let client = self
            .create_client(
                host,
                port,
                RedfishAuth::Direct(username, password),
                Some(RedfishVendor::Unknown),
            )
            .await?;

        match client.get_accounts().await {
            Ok(_) => Ok(true),
            Err(err) if err.is_unauthorized() => Ok(false),
            Err(err) => Err(RedfishClientCreationError::RedfishError(err)),
        }
    }

    /// Whether the BF4 DPU `service` account authenticates with
    /// `service_password`, without changing anything.
    ///
    /// [`set_bf4_dpu_service_password`] is a single root-authenticated change, so
    /// a crash after the hardware password changed but before the rotation was
    /// recorded leaves the `service` account already at the rotate-to value.
    /// Re-applying that change is a same-value change some BMCs reject with a
    /// password-reuse policy error, which would falsely quarantine a device that
    /// is in fact converged. The `dpu_bmc_service` engine calls this on the
    /// PATCH-failure path to tell that benign rejection from a real failure:
    /// authenticating as `service` with the rotate-to password proves the
    /// hardware already carries it.
    ///
    /// Reads the account collection (`AccountService/Accounts`) through an
    /// uninitialized `Unknown` client. Unlike [`bmc_credentials_valid`] -- which
    /// probes the fully privileged `root` account and so treats any 401/403 as a
    /// failed login -- a `403` here counts as **authenticated**: the `service`
    /// role may be forbidden from listing accounts, but a `403` still proves the
    /// password was accepted. Only a `401` (or a non-`Direct` credential) means
    /// the password was rejected.
    ///
    /// Returns `Ok(true)` when the password authenticates (`2xx` or `403`),
    /// `Ok(false)` on `401`, and `Err` for a transport or other failure the
    /// caller should treat as a transient tick error.
    ///
    /// [`set_bf4_dpu_service_password`]: BmcCredentialOps::set_bf4_dpu_service_password
    /// [`bmc_credentials_valid`]: BmcCredentialOps::bmc_credentials_valid
    async fn bf4_dpu_service_credentials_valid(
        &self,
        host: &str,
        port: Option<u16>,
        service_password: String,
    ) -> Result<bool, RedfishClientCreationError> {
        let client = self
            .create_client(
                host,
                port,
                RedfishAuth::Direct("service".to_string(), service_password),
                Some(RedfishVendor::Unknown),
            )
            .await?;

        match client.get_accounts().await {
            Ok(_) => Ok(true),
            // Authenticated but not authorized to list accounts: the password is
            // correct, which is all this probe asks. Must precede the 401 arm
            // because `is_unauthorized` also matches 403.
            Err(libredfish::RedfishError::HTTPErrorCode { status_code, .. })
                if status_code == http::StatusCode::FORBIDDEN =>
            {
                Ok(true)
            }
            Err(err) if err.is_unauthorized() => Ok(false),
            Err(err) => Err(RedfishClientCreationError::RedfishError(err)),
        }
    }

    /// Resolve the precise `RedfishVendor` of a BMC, for callers (e.g.
    /// credential rotation) that need the exact dispatch vendor
    /// `set_bmc_root_password` branches on but have nowhere to read it from.
    ///
    /// First tries the anonymous service-root probe, consulting the `Oem` key
    /// as a fallback (some BMCs leave `ServiceRoot.Vendor` null but still
    /// identify via `Oem`). If that does not yield a recognized vendor, falls
    /// back to reading the `Manufacturer` across `Chassis` entries with the
    /// supplied credentials -- the workaround Lite-On/Delta power-shelf BMCs
    /// need, since they don't populate the service-root vendor. Returns
    /// `RedfishError::MissingVendor` when neither path recognizes the vendor.
    async fn probe_bmc_vendor(
        &self,
        host: &str,
        port: Option<u16>,
        credentials: Credentials,
    ) -> Result<RedfishVendor, RedfishClientCreationError> {
        // Anonymous service-root probe. An uninitialized `Unknown` client is
        // enough to read `/redfish/v1`, and works on factory BMCs that would
        // otherwise block reads until the password is rotated.
        let anon_client = self
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
        let client = self
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
}

// Some BMC implementation may return passwords in response body and
// we can display them to user. This function is helper to remove
// password leak for password-related refish functions.
pub fn redact_password(err: libredfish::RedfishError, password: &str) -> libredfish::RedfishError {
    redact_passwords(err, &[password])
}

/// Replaces every occurrence of every non-empty needle with `REDACTED`,
/// masking the union of all matches: where two needles' matches touch or
/// overlap in the text (one password containing the other, or sharing a
/// boundary), the merged span redacts as one, so no fragment of either
/// survives.
fn mask_all(text: &str, needles: &[&str]) -> String {
    const REDACTED: &str = "REDACTED";
    let mut ranges: Vec<(usize, usize)> = needles
        .iter()
        .filter(|needle| !needle.is_empty())
        .flat_map(|needle| {
            // A manual scan over every starting position, not
            // `match_indices`: that skips overlapping matches of the same
            // needle, and a self-repetitive password (`aa` in `xaaay`) would
            // leave a fragment beside the mask. Byte-wise matching of one
            // valid UTF-8 string inside another can only land on character
            // boundaries, so the collected ranges slice cleanly.
            let needle = needle.as_bytes();
            (0..=text.len().saturating_sub(needle.len()))
                .filter(move |&start| text.as_bytes()[start..].starts_with(needle))
                .map(move |start| (start, start + needle.len()))
        })
        .collect();
    ranges.sort_unstable();

    let mut out = String::with_capacity(text.len());
    let mut cursor = 0;
    let mut i = 0;
    while i < ranges.len() {
        let (start, mut end) = ranges[i];
        i += 1;
        while i < ranges.len() && ranges[i].0 <= end {
            end = end.max(ranges[i].1);
            i += 1;
        }
        out.push_str(&text[cursor..start]);
        out.push_str(REDACTED);
        cursor = end;
    }
    out.push_str(&text[cursor..]);
    out
}

/// [`redact_password`] over several passwords at once, with union masking
/// (see [`mask_all`]) so overlapping matches cannot leave fragments of one
/// password behind after another is replaced.
pub fn redact_passwords(
    err: libredfish::RedfishError,
    passwords: &[&str],
) -> libredfish::RedfishError {
    type RfError = libredfish::RedfishError;
    let redact = |v: String| mask_all(&v, passwords);
    match err {
        RfError::HTTPErrorCode {
            url,
            status_code,
            response_body,
        } => RfError::HTTPErrorCode {
            url,
            status_code,
            response_body: redact(response_body),
        },
        RfError::JsonDeserializeError { url, body, source } => RfError::JsonDeserializeError {
            url,
            body: redact(body),
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
                let metrics = MetricsCapture::start();
                let mut skipped = false;
                let logs = capture_logs(|| {
                    skipped = emit_dpu_uefi_password_setup_skipped_if_needed(&bios_attrs);
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
                mask_all(case.text, case.needles),
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

    /// Rotate a BMC root password against the sim and report the vendor each
    /// `create_client` call was made with, in order. The contract:
    /// the FIRST client (which makes the `/AccountService` PATCH) must be
    /// uninitialized (`Unknown`), and only the SECOND client (which sets the
    /// password policy afterward) should carry the real vendor.
    async fn rotate_and_collect_client_vendors(
        vendor: RedfishVendor,
    ) -> Vec<Option<RedfishVendor>> {
        let sim = RedfishSim::default();
        sim.seed_user("root", "factory_pass");
        sim.set_bmc_root_password(
            "127.0.0.1",
            Some(443),
            vendor,
            Credentials::new("root", "factory_pass"),
            "site_pass".to_string(),
        )
        .await
        .unwrap();

        sim.create_client_calls()
            .into_iter()
            .map(|call| call.vendor)
            .collect()
    }

    #[tokio::test]
    async fn set_bf4_dpu_service_password_changes_service_account() {
        let sim = RedfishSim::default();
        sim.seed_user("root", "root_pass");
        sim.seed_user("service", "Nvidia_12345!");

        sim.set_bf4_dpu_service_password(
            "127.0.0.1",
            Some(443),
            Credentials::new("root", "root_pass"),
            "site_service_pass".to_string(),
        )
        .await
        .unwrap();

        assert_eq!(
            sim.user_password("service").as_deref(),
            Some("site_service_pass")
        );
        assert_eq!(sim.user_password("root").as_deref(), Some("root_pass"));
    }

    #[tokio::test]
    async fn set_bmc_root_password_rotates_with_unknown_then_real_vendor() {
        // Vendors whose root account is the seeded `root` user, so both the
        // by-username and by-id (curr_user) dispatch paths succeed against the
        // sim. Each must produce exactly two `create_client` calls: `Unknown`
        // for the rotation PATCH, then the real vendor for the policy client.
        for vendor in [
            RedfishVendor::LiteOnPowerShelf,
            RedfishVendor::DeltaPowerShelf,
            RedfishVendor::NvidiaDpu,
            RedfishVendor::NvidiaGBx00,
            RedfishVendor::Dell,
            RedfishVendor::Hpe,
            RedfishVendor::AMI,
        ] {
            assert_eq!(
                rotate_and_collect_client_vendors(vendor).await,
                vec![Some(RedfishVendor::Unknown), Some(vendor)],
                "vendor {vendor} must rotate via an Unknown client then a vendor-specific policy client",
            );
        }
    }

    #[tokio::test]
    async fn set_bmc_root_password_lenovo_changes_account_id_one() {
        // Lenovo dispatches to account id "1"; seed it so the rotation succeeds.
        let sim = RedfishSim::default();
        sim.seed_user("1", "factory_pass");
        sim.set_bmc_root_password(
            "127.0.0.1",
            Some(443),
            RedfishVendor::Lenovo,
            Credentials::new("root", "factory_pass"),
            "site_pass".to_string(),
        )
        .await
        .expect("Lenovo rotation against account id 1 should succeed");
    }

    #[tokio::test]
    async fn set_bmc_root_password_ami_falls_back_to_account_id_two() {
        // Factory Viking (AMI) refuses the by-username change with
        // `PasswordChangeRequired`; the rotation must then retry against
        // account id "2".
        let sim = RedfishSim::default();
        sim.seed_user("root", "factory_pass");
        sim.seed_user("2", "factory_pass");
        sim.set_password_change_required(true, None);

        sim.set_bmc_root_password(
            "127.0.0.1",
            Some(443),
            RedfishVendor::AMI,
            Credentials::new("root", "factory_pass"),
            "site_pass".to_string(),
        )
        .await
        .expect("AMI rotation should fall back to account id 2");
    }

    #[tokio::test]
    async fn set_bmc_root_password_ami_uses_account_uri() {
        let sim = RedfishSim::default();
        sim.seed_user("root", "factory_pass");
        sim.seed_user("4", "factory_pass");
        sim.set_password_change_required(true, Some("/redfish/v1/AccountService/Accounts/4"));

        sim.set_bmc_root_password(
            "127.0.0.1",
            Some(443),
            RedfishVendor::AMI,
            Credentials::new("root", "factory_pass"),
            "site_pass".to_string(),
        )
        .await
        .expect("AMI rotation should use the account URI");

        assert_eq!(sim.user_password("4").as_deref(), Some("site_pass"));
    }

    #[tokio::test]
    async fn set_bmc_root_password_ami_dispatches_to_id_two_after_password_change_required() {
        // Same factory state, but account id "2" is absent: the fallback's
        // `change_password_by_id("2")` fails with `UserNotFound("2")`, proving
        // the AMI path dispatches to id "2" after `PasswordChangeRequired`
        // (rather than swallowing the error or dispatching elsewhere).
        let sim = RedfishSim::default();
        sim.seed_user("root", "factory_pass");
        sim.set_password_change_required(true, None);

        let err = sim
            .set_bmc_root_password(
                "127.0.0.1",
                Some(443),
                RedfishVendor::AMI,
                Credentials::new("root", "factory_pass"),
                "site_pass".to_string(),
            )
            .await
            .expect_err("missing account id 2 must surface the fallback error");

        assert!(
            matches!(
                &err,
                RedfishClientCreationError::RedfishError(libredfish::RedfishError::UserNotFound(id))
                    if id == "2"
            ),
            "expected UserNotFound(\"2\"), got {err:?}",
        );
    }

    #[tokio::test]
    async fn set_bmc_root_password_rejects_unknown_vendor() {
        let sim = RedfishSim::default();
        sim.seed_user("root", "factory_pass");

        let err = sim
            .set_bmc_root_password(
                "127.0.0.1",
                Some(443),
                RedfishVendor::Unknown,
                Credentials::new("root", "factory_pass"),
                "site_pass".to_string(),
            )
            .await
            .expect_err("Unknown vendor must be rejected");

        assert!(
            matches!(
                err,
                RedfishClientCreationError::RedfishError(libredfish::RedfishError::MissingVendor)
            ),
            "expected MissingVendor, got {err:?}",
        );
    }

    #[tokio::test]
    async fn probe_bmc_vendor_resolves_from_service_root() {
        let sim = RedfishSim::default();
        let vendor = sim
            .probe_bmc_vendor("127.0.0.1", Some(443), Credentials::new("root", "pw"))
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

            let vendor = sim
                .probe_bmc_vendor("127.0.0.1", Some(443), Credentials::new("root", "pw"))
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

        let err = sim
            .probe_bmc_vendor("127.0.0.1", Some(443), Credentials::new("root", "pw"))
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

    #[tokio::test]
    async fn bmc_credentials_valid_true_when_password_matches() {
        let sim = RedfishSim::default();
        sim.set_enforce_auth(true);
        sim.seed_user("root", "correct");

        let valid = sim
            .bmc_credentials_valid("127.0.0.1", Some(443), Credentials::new("root", "correct"))
            .await
            .expect("a matching credential must not raise a transport error");

        assert!(valid, "the seeded password must authenticate");
    }

    #[tokio::test]
    async fn bmc_credentials_valid_false_when_password_wrong() {
        let sim = RedfishSim::default();
        sim.set_enforce_auth(true);
        sim.seed_user("root", "correct");

        let valid = sim
            .bmc_credentials_valid("127.0.0.1", Some(443), Credentials::new("root", "stale"))
            .await
            .expect("an unauthorized rejection must be reported as Ok(false), not an error");

        assert!(!valid, "a wrong password must be reported as not valid");
    }

    #[tokio::test]
    async fn bmc_credentials_valid_propagates_transport_error() {
        let sim = RedfishSim::default();
        sim.seed_user("root", "correct");
        // A non-authentication failure (503) must surface as an error rather than
        // being misread as a definitive "credentials are invalid" (Ok(false)).
        sim.set_get_accounts_error(true);

        let err = sim
            .bmc_credentials_valid("127.0.0.1", Some(443), Credentials::new("root", "correct"))
            .await
            .expect_err("a non-auth transport failure must propagate as an error");

        assert!(
            matches!(
                err,
                RedfishClientCreationError::RedfishError(ref e) if !e.is_unauthorized()
            ),
            "expected a non-unauthorized RedfishError, got {err:?}",
        );
    }

    /// Minimal in-crate pool double for driving the `BmcCredentialOps`
    /// *default* bodies (which `RedfishSim` overrides): client creation
    /// delegates to an inner sim, and the credential store always fails, so
    /// each test isolates one failure source and asserts its
    /// [`CredentialOpError`] class.
    struct ClassificationDouble {
        sim: RedfishSim,
        reader: FailingCredentialReader,
    }

    struct FailingCredentialReader;

    #[async_trait]
    impl CredentialReader for FailingCredentialReader {
        async fn get_credentials(
            &self,
            _key: &CredentialKey,
        ) -> Result<Option<Credentials>, carbide_secrets::SecretsError> {
            Err(carbide_secrets::SecretsError::UfmCredentialReadBlocked {
                fabric: "store unavailable".to_string(),
            })
        }
    }

    #[async_trait]
    impl RedfishClientPool for ClassificationDouble {
        async fn create_client(
            &self,
            host: &str,
            port: Option<u16>,
            auth: RedfishAuth,
            vendor: Option<RedfishVendor>,
        ) -> Result<Box<dyn Redfish>, RedfishClientCreationError> {
            self.sim.create_client(host, port, auth, vendor).await
        }

        fn credential_reader(&self) -> &dyn CredentialReader {
            &self.reader
        }
    }

    impl sealed::Sealed for ClassificationDouble {}

    #[async_trait]
    impl BmcCredentialOps for ClassificationDouble {}

    fn double_access() -> BmcAccessInfo {
        BmcAccessInfo {
            host: "192.0.2.1".to_string(),
            port: None,
            mac_address: "00:11:22:33:44:55".parse().unwrap(),
        }
    }

    fn site_creds() -> Credentials {
        Credentials::UsernamePassword {
            username: String::new(),
            password: "site-secret".to_string(),
        }
    }

    /// A failed client build inside a credential op is the transient
    /// ([`CredentialOpError::ClientCreation`]) class: call sites retry it
    /// instead of quarantining the device.
    #[tokio::test]
    async fn uefi_setup_client_creation_failure_is_client_creation_class() {
        let double = ClassificationDouble {
            sim: RedfishSim::default(),
            reader: FailingCredentialReader,
        };
        double.sim.set_create_client_error("bmc probe timed out");
        let err = double
            .uefi_setup(&double_access(), true, site_creds())
            .await
            .expect_err("client creation failure must fail the op");
        assert!(
            matches!(err, CredentialOpError::ClientCreation(_)),
            "a failed client build must be the transient class, got {err:?}"
        );
    }

    /// The DPU factory-default store read inside `uefi_setup` fails before
    /// the operation reaches the device, so it is also the transient
    /// ([`CredentialOpError::ClientCreation`]) class, not a device-level
    /// `Operation` failure.
    #[tokio::test]
    async fn uefi_setup_dpu_store_read_failure_is_client_creation_class() {
        let double = ClassificationDouble {
            sim: RedfishSim::default(),
            reader: FailingCredentialReader,
        };
        // Attributes expose a UEFI password field, so the DPU body proceeds
        // past its probe to the factory-default store read, which fails.
        double
            .sim
            .set_bios_attributes(std::collections::HashMap::from([(
                "Attributes".to_string(),
                serde_json::json!({ "CurrentUefiPassword": "" }),
            )]));
        let err = double
            .uefi_setup(&double_access(), true, site_creds())
            .await
            .expect_err("store read failure must fail the op");
        assert!(
            matches!(err, CredentialOpError::ClientCreation(_)),
            "a store read failure never reached the device, got {err:?}"
        );
    }
}
