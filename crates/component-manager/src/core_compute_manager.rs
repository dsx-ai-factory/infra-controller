// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

use std::net::IpAddr;
use std::sync::Arc;
use std::time::Duration;

use bmc_backend::BackendBmc;
use bmc_platform::{PlatformError, PowerState as BmcPowerState, ResetType};
use bmc_runtime::DriverConnector;
use carbide_redfish::libredfish::RedfishClientPool;
use carbide_secrets::credentials::Credentials;
use carbide_utils::redfish::BmcAccessInfo;
use futures_util::{StreamExt, stream};
use libredfish::model::ODataId;
use libredfish::model::software_inventory::SoftwareInventory;
use mac_address::MacAddress;
use model::component_manager::{ComputeTrayComponent, PowerAction};
use model::machine::PowerState as MachinePowerState;
use sqlx::PgPool;
use tokio::time::{Instant, timeout_at};

use crate::component_common::ComponentPowerStateResult;
use crate::compute_tray_manager::{
    Backend, ComputeTrayEndpoint, ComputeTrayFirmwareUpdateStatus, ComputeTrayResult,
    ComputeTrayVendor,
};
use crate::error::ComponentManagerError;
use crate::firmware_inventory::{
    ComponentFirmwareInventoryResult, FirmwareComponent, FirmwareInventoryEntry,
    FirmwareInventorySource, FirmwareReadError,
};

/// `CoreComputeTrayManager` uses NICo-core's Redfish stack for power control
/// and observations. Each power-read request allows at most eight active BMCs
/// and shares a ten-second budget across queueing, client creation, and reads.
/// Queued endpoints expire without being read; completed observations remain
/// in input order, and duplicate endpoints are read separately. The request
/// owns its futures, so dropping it cancels unfinished setup and reads.
/// Firmware inventory reads use the same pool and direct endpoint credentials;
/// the pool must support direct authentication. Firmware updates, update status,
/// and bundle listing remain unsupported. See `get_firmware_inventory` for its bounds.
pub struct CoreComputeTrayManager {
    redfish_pool: Arc<dyn RedfishClientPool>,
    /// Connects moved BMC operations through their drivers when the site
    /// sets `bmc_backend = "drivers"`; `None` runs them through libredfish.
    drivers: Option<ComputeTrayDrivers>,
}

/// The driver connector and the database each tray's explored report is
/// read from.
struct ComputeTrayDrivers {
    connector: Arc<DriverConnector>,
    db_pool: PgPool,
}

// Bound request latency even when BMCs are unavailable. Waiting devices
// share the same deadline as client creation and power reads.
const POWER_READ_TIMEOUT: Duration = Duration::from_secs(10);
// Bound simultaneous BMC connections per request while allowing slow devices
// to be read independently. Remaining endpoints stay in the borrowed slice.
const POWER_READ_CONCURRENCY: usize = 8;
const POWER_READ_TIMEOUT_ERROR: &str = "redfish power read deadline exceeded";

// Firmware inventories require one collection read plus one read per member.
// Four endpoint reads with four members each cap this request at 16 simultaneous
// member reads. Repeated endpoints have independent admission slots.
const FIRMWARE_ENDPOINT_CONCURRENCY: usize = 4;
const FIRMWARE_MEMBER_CONCURRENCY: usize = 4;
// Member fan-out needs more time than a power read, but must still finish before
// the transport's own timeout. This is a partial-observation budget: slow or
// queued work expires while completed members remain useful.
const FIRMWARE_READ_TIMEOUT: Duration = Duration::from_secs(30);
const FIRMWARE_ENDPOINT_LIMIT: usize = 64;
// Pinned libredfish hardware fixtures contain up to 106 entries and 136-byte
// text fields. Leave growth room while bounding retained data and follow-up I/O.
const FIRMWARE_MEMBER_LIMIT: usize = 256;
const FIRMWARE_TEXT_LIMIT: usize = 1024;
const FIRMWARE_INVENTORY_TEXT_LIMIT: usize = 64 * 1024;
const FIRMWARE_INVENTORY_URI: &str = "/redfish/v1/UpdateService/FirmwareInventory";

impl std::fmt::Debug for CoreComputeTrayManager {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("CoreComputeTrayManager").finish()
    }
}

impl CoreComputeTrayManager {
    pub fn new(redfish_pool: Arc<dyn RedfishClientPool>) -> Self {
        Self {
            redfish_pool,
            drivers: None,
        }
    }

    /// Runs moved BMC operations through `driver_connector`, reading each
    /// tray's explored report from `db_pool`; `None` keeps them on libredfish.
    pub fn with_driver_connector(
        mut self,
        driver_connector: Option<Arc<DriverConnector>>,
        db_pool: PgPool,
    ) -> Self {
        self.drivers = driver_connector.map(|connector| ComputeTrayDrivers { connector, db_pool });
        self
    }

    /// Connects to `endpoint`'s BMC with its credentials through the
    /// configured `bmc_backend`.
    async fn connect_bmc(&self, endpoint: &ComputeTrayEndpoint) -> Result<BackendBmc, String> {
        let Some(drivers) = &self.drivers else {
            let Credentials::UsernamePassword { username, password } = &endpoint.bmc_credentials;
            return self
                .redfish_pool
                .create_client(
                    &endpoint.bmc_ip.to_string(),
                    Some(443),
                    carbide_redfish::libredfish::RedfishAuth::Direct(
                        username.clone(),
                        password.clone(),
                    ),
                    map_vendor(endpoint.vendor),
                )
                .await
                .map(BackendBmc::Libredfish)
                .map_err(|error| format!("failed to create Redfish client: {error}"));
        };
        let report = db::explored_endpoints::find_by_ips(&drivers.db_pool, vec![endpoint.bmc_ip])
            .await
            .map_err(|error| format!("failed to read the explored endpoint: {error}"))?
            .into_iter()
            .next()
            .ok_or_else(|| {
                format!(
                    "BMC {} has no explored endpoint to select drivers from",
                    endpoint.bmc_ip
                )
            })?
            .report;
        let access = BmcAccessInfo {
            host: endpoint.bmc_ip.to_string(),
            port: Some(443),
            mac_address: endpoint.bmc_mac,
        };
        drivers
            .connector
            .connect_with_credentials(&access, &report, endpoint.bmc_credentials.clone())
            .await
            .map(BackendBmc::from)
            .map_err(|error| format!("failed to connect through drivers: {error}"))
    }

    /// Reads one BMC's collection before admitting bounded member requests.
    /// Keeping a result for each member makes partial failures visible without
    /// replacing healthy observations or inventing a successful empty inventory.
    #[tracing::instrument(skip_all, fields(
        bmc_ip_address = %endpoint.bmc_ip,
        bmc_mac_address = %endpoint.bmc_mac,
    ))]
    async fn read_firmware_inventory(
        &self,
        endpoint: &ComputeTrayEndpoint,
        deadline: Instant,
    ) -> Result<Vec<FirmwareInventoryEntry>, FirmwareReadError> {
        let Credentials::UsernamePassword { username, password } = &endpoint.bmc_credentials;
        let client = firmware_read_before_deadline(
            deadline,
            self.redfish_pool.create_client(
                &endpoint.bmc_ip.to_string(),
                Some(443),
                carbide_redfish::libredfish::RedfishAuth::Direct(
                    username.clone(),
                    password.clone(),
                ),
                map_vendor(endpoint.vendor),
            ),
        )
        .await?;
        // get_software_inventories discards pagination/count metadata and skips
        // malformed links. The decorated raw GET retains that evidence so we
        // can reject an incomplete collection before requesting its members.
        let collection = ODataId {
            odata_id: FIRMWARE_INVENTORY_URI.into(),
        };
        let ids = {
            let resource =
                firmware_read_before_deadline(deadline, client.get_resource(collection)).await?;
            firmware_inventory_ids(resource.raw.get(), endpoint.bmc_ip, endpoint.bmc_mac)?
        };
        if Instant::now() >= deadline {
            return Err(FirmwareReadError::DeadlineExceeded);
        }
        let mut text_bytes: usize = ids.iter().map(String::len).sum();

        let reads = stream::iter(0..ids.len())
            .map(|index| {
                let client = client.as_ref();
                let id = &ids[index];
                async move {
                    let firmware = firmware_read_before_deadline(deadline, client.get_firmware(id))
                        .await
                        .and_then(firmware_component);
                    (index, firmware)
                }
            })
            .buffer_unordered(FIRMWARE_MEMBER_CONCURRENCY);
        futures_util::pin_mut!(reads);
        let mut entries: Vec<_> = ids
            .iter()
            .map(|id| FirmwareInventoryEntry {
                inventory_id: Some(id.clone()),
                firmware: Err(FirmwareReadError::DeadlineExceeded),
            })
            .collect();
        while let Some((index, firmware)) = reads.next().await {
            entries[index].firmware = firmware;
        }

        // Apply the total in collection order, so completion timing never decides
        // which metadata fits. An oversized member keeps its ID and an error.
        for entry in &mut entries {
            if let Ok(firmware) = &entry.firmware {
                let size = firmware.id.as_ref().map_or(0, String::len)
                    + firmware.resource_uri.as_ref().map_or(0, String::len)
                    + firmware.name.as_ref().map_or(0, String::len)
                    + firmware.update_target.as_ref().map_or(0, String::len)
                    + firmware.description.as_ref().map_or(0, String::len)
                    + firmware.version.as_ref().map_or(0, String::len)
                    + firmware.release_date.as_ref().map_or(0, String::len);
                if text_bytes + size > FIRMWARE_INVENTORY_TEXT_LIMIT {
                    entry.firmware = Err(FirmwareReadError::LimitExceeded);
                } else {
                    text_bytes += size;
                }
            }
        }
        Ok(entries)
    }
}

/// Accepts one complete collection, retaining lookup order and repeated members.
/// Continuation or inconsistent metadata is unavailable evidence, not an empty
/// inventory or a successful prefix. Only canonical child URIs that libredfish's
/// member lookup can reproduce are accepted; other links must not select a
/// different resource merely because their last segment matches.
/// Rejections log a fixed reason and endpoint, never response or parser text.
fn firmware_inventory_ids(
    raw: &str,
    bmc_ip: IpAddr,
    bmc_mac: MacAddress,
) -> Result<Vec<String>, FirmwareReadError> {
    let reject = |reason: &'static str, error: FirmwareReadError| {
        tracing::warn!(
            bmc_ip_address = %bmc_ip,
            bmc_mac_address = %bmc_mac,
            reason,
            "firmware inventory collection rejected"
        );
        error
    };
    // The resource already contains valid JSON, but materializing its object can
    // still fail (for example at the decoder's recursion limit).
    let collection: serde_json::Value = serde_json::from_str(raw)
        .map_err(|_| reject("collection_decode_failed", FirmwareReadError::ReadFailed))?;
    let collection = collection
        .as_object()
        .ok_or_else(|| reject("collection_not_object", FirmwareReadError::ReadFailed))?;
    if collection.contains_key("Members@odata.nextLink")
        || collection.contains_key("@odata.nextLink")
    {
        return Err(reject(
            "continuation_unsupported",
            FirmwareReadError::ReadFailed,
        ));
    }
    let members = collection
        .get("Members")
        .and_then(serde_json::Value::as_array)
        .ok_or_else(|| reject("members_missing", FirmwareReadError::ReadFailed))?;
    if collection
        .get("Members@odata.count")
        .and_then(serde_json::Value::as_u64)
        != Some(members.len() as u64)
    {
        return Err(reject(
            "member_count_invalid",
            FirmwareReadError::ReadFailed,
        ));
    }
    if members.len() > FIRMWARE_MEMBER_LIMIT {
        return Err(reject(
            "member_count_limit",
            FirmwareReadError::LimitExceeded,
        ));
    }
    let mut lookup_bytes = 0;
    let mut ids = Vec::with_capacity(members.len());
    for member in members {
        let id = member
            .get("@odata.id")
            .and_then(serde_json::Value::as_str)
            .and_then(|uri| uri.strip_prefix(FIRMWARE_INVENTORY_URI))
            .and_then(|suffix| suffix.strip_prefix('/'))
            .filter(|id| {
                // Accept literal path-segment characters, including Dell's
                // colon-bearing IDs. Encoded or navigational forms cannot be
                // faithfully reconstructed by the string-based member API.
                !id.is_empty()
                    && *id != "."
                    && *id != ".."
                    && id.bytes().all(|byte| {
                        byte.is_ascii_alphanumeric() || b"-._~!$&'()*+,;=:@".contains(&byte)
                    })
            })
            .ok_or_else(|| reject("member_uri_invalid", FirmwareReadError::ReadFailed))?;
        if id.len() > FIRMWARE_TEXT_LIMIT {
            return Err(reject("member_id_limit", FirmwareReadError::LimitExceeded));
        }
        lookup_bytes += id.len();
        if lookup_bytes > FIRMWARE_INVENTORY_TEXT_LIMIT {
            return Err(reject(
                "lookup_text_limit",
                FirmwareReadError::LimitExceeded,
            ));
        }
        ids.push(id.to_owned());
    }
    Ok(ids)
}

/// Checks both sides of a firmware I/O attempt because Tokio may poll a ready
/// result before its expired timer. Queued attempts must never start after expiry.
async fn firmware_read_before_deadline<T, E>(
    deadline: Instant,
    read: impl std::future::Future<Output = Result<T, E>>,
) -> Result<T, FirmwareReadError> {
    if Instant::now() >= deadline {
        return Err(FirmwareReadError::DeadlineExceeded);
    }
    let result = timeout_at(deadline, read).await;
    if Instant::now() >= deadline {
        return Err(FirmwareReadError::DeadlineExceeded);
    }
    match result {
        Ok(Ok(value)) => Ok(value),
        // Keep dependency error bodies out of observations. The decorated
        // Redfish client retains its existing diagnostic handling.
        Ok(Err(_)) => Err(FirmwareReadError::ReadFailed),
        Err(_) => Err(FirmwareReadError::DeadlineExceeded),
    }
}

/// Retains the backend's metadata, including any vendor version normalization.
/// Oversized text makes this member unavailable instead of clipping its values.
fn firmware_component(value: SoftwareInventory) -> Result<FirmwareComponent, FirmwareReadError> {
    if [
        Some(value.id.as_str()),
        Some(value.odata.odata_id.as_str()),
        value.description.as_deref(),
        value.version.as_deref(),
        value.release_date.as_deref(),
    ]
    .into_iter()
    .flatten()
    .any(|text| text.len() > FIRMWARE_TEXT_LIMIT)
    {
        return Err(FirmwareReadError::LimitExceeded);
    }
    Ok(FirmwareComponent {
        id: Some(value.id),
        resource_uri: Some(value.odata.odata_id),
        name: None,
        update_target: None,
        description: value.description,
        version: value.version,
        release_date: value.release_date,
    })
}

fn map_vendor(vendor: ComputeTrayVendor) -> Option<libredfish::model::service_root::RedfishVendor> {
    use libredfish::model::service_root::RedfishVendor;
    match vendor {
        ComputeTrayVendor::Dell => Some(RedfishVendor::Dell),
        ComputeTrayVendor::Hpe => Some(RedfishVendor::Hpe),
        ComputeTrayVendor::Lenovo => Some(RedfishVendor::Lenovo),
        ComputeTrayVendor::Supermicro => Some(RedfishVendor::Supermicro),
        ComputeTrayVendor::Nvidia | ComputeTrayVendor::Unknown => None,
    }
}

fn map_power_action(action: PowerAction) -> ResetType {
    match action {
        PowerAction::On => ResetType::On,
        PowerAction::GracefulShutdown => ResetType::GracefulShutdown,
        PowerAction::ForceOff => ResetType::ForceOff,
        PowerAction::GracefulRestart => ResetType::GracefulRestart,
        PowerAction::ForceRestart => ResetType::ForceRestart,
        PowerAction::AcPowercycle => ResetType::FullPowerCycle,
    }
}

/// The model power state for a BMC's reported one, `Unknown` when it reports
/// none NICo classifies.
fn machine_power_state(state: Option<BmcPowerState>) -> MachinePowerState {
    match state {
        Some(BmcPowerState::On) => MachinePowerState::On,
        Some(BmcPowerState::Off) => MachinePowerState::Off,
        Some(BmcPowerState::PoweringOn) => MachinePowerState::PoweringOn,
        Some(BmcPowerState::PoweringOff) => MachinePowerState::PoweringOff,
        Some(BmcPowerState::Paused) => MachinePowerState::Paused,
        Some(
            BmcPowerState::Sleeping | BmcPowerState::Hibernating | BmcPowerState::UnsupportedValue,
        )
        | None => MachinePowerState::Unknown,
    }
}

#[async_trait::async_trait]
impl crate::compute_tray_manager::ComputeTrayManager for CoreComputeTrayManager {
    fn power_state_source(&self) -> Option<crate::component_common::PowerStateSource> {
        Some(crate::component_common::PowerStateSource::Redfish)
    }

    fn firmware_inventory_source(&self) -> Option<FirmwareInventorySource> {
        Some(FirmwareInventorySource::Redfish)
    }

    /// Rejects more than 64 endpoints with `ComponentManagerError::InvalidArgument`
    /// before I/O. Accepted calls allow four concurrent endpoint reads, each with
    /// four member reads (16 simultaneous member reads). Repeated endpoints use
    /// separate slots. Endpoints are admitted in caller order; slow early
    /// endpoints can consume the budget before later ones start, so callers may
    /// split larger batches. One 30-second budget covers queueing, setup, listing
    /// and member reads; it does not guarantee a complete batch.
    /// Each inventory allows 256 members, 1 KiB per field and 64 KiB of retained
    /// text. Oversized collections fail whole; oversized members retain their
    /// lookup ID with `LimitExceeded`. Incomplete or malformed collections fail
    /// with `ReadFailed` before member reads. Collections require an exact member
    /// count and canonical child links, with no continuation. Versions retain
    /// vendor normalization; this reader does not shorten them. Limits apply
    /// after HTTP decoding and do not bound transport buffering.
    ///
    /// Endpoint and member order, including repeats, matches the caller/listing.
    /// Completed members survive peer failures or expiry. Dropping this future
    /// drops all unfinished reads. The reader spawns no tasks of its own and
    /// performs no mutations; the shared HTTP pool retains its own lifecycle.
    async fn get_firmware_inventory(
        &self,
        endpoints: &[ComputeTrayEndpoint],
    ) -> Result<Vec<ComponentFirmwareInventoryResult>, ComponentManagerError> {
        if endpoints.len() > FIRMWARE_ENDPOINT_LIMIT {
            return Err(ComponentManagerError::InvalidArgument(format!(
                "firmware inventory accepts at most {FIRMWARE_ENDPOINT_LIMIT} endpoints"
            )));
        }
        let deadline = Instant::now() + FIRMWARE_READ_TIMEOUT;
        let reads = stream::iter(0..endpoints.len())
            .map(|index| async move {
                (
                    index,
                    self.read_firmware_inventory(&endpoints[index], deadline)
                        .await,
                )
            })
            .buffer_unordered(FIRMWARE_ENDPOINT_CONCURRENCY);
        futures_util::pin_mut!(reads);
        let mut results: Vec<_> = endpoints
            .iter()
            .map(|endpoint| ComponentFirmwareInventoryResult {
                mac_address: endpoint.bmc_mac,
                inventory: Err(FirmwareReadError::DeadlineExceeded),
            })
            .collect();
        while let Some((index, inventory)) = reads.next().await {
            results[index].inventory = inventory;
        }
        Ok(results)
    }

    fn name(&self) -> &str {
        "core"
    }

    fn backend(&self) -> Backend {
        Backend::Core
    }

    /// `get_power_state` reads each BMC with its endpoint's direct credentials.
    /// Each input gets one result in order, including duplicate endpoints.
    /// Setup, read, and deadline errors stay with the affected device, leaving
    /// completed observations available when another device fails or times out.
    async fn get_power_state(
        &self,
        endpoints: &[ComputeTrayEndpoint],
    ) -> Result<Vec<ComponentPowerStateResult>, ComponentManagerError> {
        let deadline = Instant::now() + POWER_READ_TIMEOUT;
        let mut results: Vec<_> = endpoints
            .iter()
            .map(|endpoint| ComponentPowerStateResult {
                mac_address: endpoint.bmc_mac,
                power_state: Err(POWER_READ_TIMEOUT_ERROR.to_owned()),
            })
            .collect();
        let reads = stream::iter(0..endpoints.len())
            .map(|index| async move {
                // `timeout_at` polls its inner future before its timer. Do not
                // start another device after the shared budget has been spent.
                if Instant::now() >= deadline {
                    return (index, Err(POWER_READ_TIMEOUT_ERROR.to_owned()));
                }
                let endpoint = &endpoints[index];
                let read = async {
                    let bmc = self.connect_bmc(endpoint).await?;
                    // Setup can finish on the deadline poll. Check again so
                    // the inner-first poll cannot begin a late power read.
                    if Instant::now() >= deadline {
                        return Err(POWER_READ_TIMEOUT_ERROR.to_owned());
                    }
                    let state = async { bmc.power()?.state().await }.await.map_err(
                        |error: PlatformError| format!("redfish power read failed: {error}"),
                    )?;
                    Ok(Some(machine_power_state(state)))
                };
                let power_state = timeout_at(deadline, read)
                    .await
                    .unwrap_or_else(|_| Err(POWER_READ_TIMEOUT_ERROR.to_owned()));
                (index, power_state)
            })
            .buffer_unordered(POWER_READ_CONCURRENCY);
        futures_util::pin_mut!(reads);
        while let Some((index, power_state)) = reads.next().await {
            results[index].power_state = power_state;
        }
        Ok(results)
    }

    async fn power_control(
        &self,
        endpoints: &[ComputeTrayEndpoint],
        action: PowerAction,
    ) -> Result<Vec<ComputeTrayResult>, ComponentManagerError> {
        let reset_type = map_power_action(action);
        let mut results = Vec::with_capacity(endpoints.len());

        for ep in endpoints {
            let outcome = async {
                let bmc = self.connect_bmc(ep).await?;
                async { bmc.power()?.set(reset_type).await }
                    .await
                    .map(drop)
                    .map_err(|e: PlatformError| format!("Redfish power control failed: {e}"))
            }
            .await;

            results.push(ComputeTrayResult {
                bmc_ip: ep.bmc_ip,
                bmc_mac: ep.bmc_mac,
                success: outcome.is_ok(),
                error: outcome.err(),
                backend_job_id: None,
            });
        }

        Ok(results)
    }

    async fn update_firmware(
        &self,
        _endpoints: &[ComputeTrayEndpoint],
        _target_version: &str,
        _components: &[ComputeTrayComponent],
        _options: &crate::types::FirmwareUpdateOptions,
    ) -> Result<Vec<ComputeTrayResult>, ComponentManagerError> {
        Err(ComponentManagerError::Internal(
            "firmware update is not supported by the core compute tray backend".into(),
        ))
    }

    async fn get_firmware_status(
        &self,
        _endpoints: &[ComputeTrayEndpoint],
    ) -> Result<Vec<ComputeTrayFirmwareUpdateStatus>, ComponentManagerError> {
        Err(ComponentManagerError::Internal(
            "firmware status is not supported by the core compute tray backend".into(),
        ))
    }

    async fn list_firmware_bundles(&self) -> Result<Vec<String>, ComponentManagerError> {
        Err(ComponentManagerError::Internal(
            "firmware bundles listing is not supported by the core compute tray backend".into(),
        ))
    }
}

#[cfg(test)]
mod tests {
    mod firmware;

    use std::collections::HashMap;
    use std::sync::Mutex;
    use std::sync::atomic::{AtomicUsize, Ordering};

    use carbide_redfish::libredfish::test_support::{RedfishAuthKind, RedfishSim};
    use carbide_redfish::libredfish::{RedfishAuth, RedfishClientCreationError};
    use carbide_secrets::credentials::CredentialReader;
    use carbide_test_support::value_scenarios;
    use libredfish::model::service_root::RedfishVendor;
    use libredfish::{PowerState as RedfishPowerState, Redfish, RedfishError, RedfishFuture};
    use mac_address::MacAddress;
    use model::machine::PowerState;
    use tokio::sync::oneshot;

    use super::*;
    use crate::compute_tray_manager::ComputeTrayManager;

    /// `test_compute_tray_endpoint` gives each index distinct BMC identity and
    /// credentials, cycling vendors so reads also exercise vendor selection.
    fn test_compute_tray_endpoint(index: u8) -> ComputeTrayEndpoint {
        ComputeTrayEndpoint {
            vendor: match index % 3 {
                0 => ComputeTrayVendor::Dell,
                1 => ComputeTrayVendor::Hpe,
                _ => ComputeTrayVendor::Unknown,
            },
            bmc_ip: std::net::Ipv4Addr::new(192, 0, 2, index).into(),
            bmc_mac: MacAddress::new([0, 0, 0, 0, 0, index]),
            bmc_credentials: Credentials::UsernamePassword {
                username: format!("admin-{index}"),
                password: format!("test-password-{index}"),
            },
        }
    }

    /// `PowerReadPhase` selects the failure or cancellation boundary under test.
    #[derive(Clone, Copy, Debug, PartialEq, Eq)]
    enum PowerReadPhase {
        ClientCreation,
        PowerRead,
    }

    /// `ComputeReadTestGates` pauses selected hosts or firmware inventory members.
    /// Each registered receiver is consumed by the first wait for that key.
    /// Once consumed, it belongs to the waiting future, so cancelling that
    /// future closes its sender. Gates never reached remain in `waiters`.
    #[derive(Default)]
    struct ComputeReadTestGates {
        /// Registered gates that no call has reached yet.
        waiters: Mutex<HashMap<String, oneshot::Receiver<()>>>,
        /// Cumulative reached gates; excludes ungated calls and never decrements.
        started: AtomicUsize,
    }

    impl ComputeReadTestGates {
        /// `block` registers a gate for the next wait on this host, replacing
        /// any prior gate. Keep the returned sender alive until releasing the
        /// gate or cancelling the request, or a waiting future will panic.
        fn block(&self, endpoint: &ComputeTrayEndpoint) -> oneshot::Sender<()> {
            self.block_key(endpoint.bmc_ip.to_string())
        }

        /// Registers a host or host/member gate for the next matching read.
        fn block_key(&self, key: String) -> oneshot::Sender<()> {
            let (sender, receiver) = oneshot::channel();
            self.waiters.lock().unwrap().insert(key, receiver);
            sender
        }

        /// `wait` consumes a registered gate and waits for the test to release
        /// it. Keys without a registered gate continue immediately.
        async fn wait(&self, key: &str) {
            let waiter = self.waiters.lock().unwrap().remove(key);
            if let Some(waiter) = waiter {
                self.started.fetch_add(1, Ordering::Relaxed);
                waiter
                    .await
                    .expect("test must keep unreleased senders alive");
            }
        }
    }

    /// `ComputeReadTestPool` checks endpoint credentials, port, and vendor before
    /// forwarding client creation to `RedfishSim`. Separate setup and read
    /// gates let tests stop either phase without relying on wall-clock delays.
    struct ComputeReadTestPool {
        sim: RedfishSim,
        endpoints: HashMap<String, ComputeTrayEndpoint>,
        setup_gates: ComputeReadTestGates,
        read_gates: Arc<ComputeReadTestGates>,
        firmware: Arc<FirmwareReadTestData>,
    }

    /// `compute_read_test_pool` records expected endpoints and starts with no
    /// blocked hosts; each test registers only the gates it needs.
    fn compute_read_test_pool(endpoints: &[ComputeTrayEndpoint]) -> Arc<ComputeReadTestPool> {
        Arc::new(ComputeReadTestPool {
            sim: RedfishSim::default(),
            endpoints: endpoints
                .iter()
                .map(|endpoint| (endpoint.bmc_ip.to_string(), endpoint.clone()))
                .collect(),
            setup_gates: ComputeReadTestGates::default(),
            read_gates: Arc::new(ComputeReadTestGates::default()),
            firmware: Arc::new(FirmwareReadTestData::default()),
        })
    }

    #[async_trait::async_trait]
    impl RedfishClientPool for ComputeReadTestPool {
        /// `create_client` validates the request before waiting at the setup
        /// gate, then wraps the simulator client with its separate read gate.
        async fn create_client(
            &self,
            host: &str,
            port: Option<u16>,
            auth: RedfishAuth,
            vendor: Option<RedfishVendor>,
        ) -> Result<Box<dyn Redfish>, RedfishClientCreationError> {
            let expected = &self.endpoints[host];
            let Credentials::UsernamePassword { username, password } = &expected.bmc_credentials;
            assert_eq!(port, Some(443));
            assert!(matches!(
                &auth,
                RedfishAuth::Direct(actual_username, actual_password)
                    if actual_username == username && actual_password == password
            ));
            assert_eq!(vendor, map_vendor(expected.vendor));

            self.setup_gates.wait(host).await;
            let inner = self.sim.create_client(host, port, auth, vendor).await?;
            Ok(Box::new(ComputeReadTestClient {
                inner,
                gates: self.read_gates.clone(),
                host: host.to_owned(),
                firmware: self.firmware.clone(),
            }))
        }

        fn credential_reader(&self) -> &dyn CredentialReader {
            self.sim.credential_reader()
        }
    }

    /// `ComputeReadTestClient` gates power and firmware reads before they reach
    /// the simulator. Power commands still reach `RedfishSim`, so tests can
    /// detect accidental writes through its recorded actions. Unimplemented
    /// mutation methods reach the simulator's panicking `std_redfish` stub.
    struct ComputeReadTestClient {
        inner: Box<dyn Redfish>,
        gates: Arc<ComputeReadTestGates>,
        host: String,
        firmware: Arc<FirmwareReadTestData>,
    }

    /// Firmware overrides extend the pool that checks endpoint credentials.
    /// Members are keyed by host and lookup ID; member gates use `host/id`.
    /// Raw collections and members without overrides come from the simulator.
    #[derive(Default)]
    struct FirmwareReadTestData {
        members: Mutex<HashMap<(String, String), Result<SoftwareInventory, String>>>,
        list_gates: ComputeReadTestGates,
        member_gates: ComputeReadTestGates,
        list_calls: AtomicUsize,
        member_calls: Mutex<Vec<(String, String)>>,
    }

    impl Redfish for ComputeReadTestClient {
        fn std_redfish(&self) -> &libredfish::standard::RedfishStandard {
            self.inner.std_redfish()
        }

        /// `get_power_state` waits at this host's read gate before consuming a
        /// simulated state or injected read failure.
        fn get_power_state<'a>(
            &'a self,
        ) -> RedfishFuture<'a, Result<RedfishPowerState, RedfishError>> {
            Box::pin(async move {
                self.gates.wait(&self.host).await;
                self.inner.get_power_state().await
            })
        }

        /// Records collection admission and waits at this host's list gate
        /// before reading the simulator's raw resource, including collection metadata.
        fn get_resource<'a>(
            &'a self,
            id: ODataId,
        ) -> RedfishFuture<'a, Result<libredfish::model::resource::Resource, RedfishError>>
        {
            Box::pin(async move {
                self.firmware.list_calls.fetch_add(1, Ordering::Relaxed);
                self.firmware.list_gates.wait(&self.host).await;
                self.inner.get_resource(id).await
            })
        }

        /// Records each requested member, then consumes that member's gate
        /// before returning its override or the simulator metadata.
        fn get_firmware<'a>(
            &'a self,
            id: &'a str,
        ) -> RedfishFuture<'a, Result<SoftwareInventory, RedfishError>> {
            Box::pin(async move {
                self.firmware
                    .member_calls
                    .lock()
                    .unwrap()
                    .push((self.host.clone(), id.to_owned()));
                self.firmware
                    .member_gates
                    .wait(&format!("{}/{id}", self.host))
                    .await;
                let member = self
                    .firmware
                    .members
                    .lock()
                    .unwrap()
                    .get(&(self.host.clone(), id.to_owned()))
                    .cloned();
                match member {
                    Some(result) => result.map_err(|error| RedfishError::GenericError { error }),
                    None => self.inner.get_firmware(id).await,
                }
            })
        }

        /// `power` forwards commands so an unintended write stays observable.
        fn power<'a>(
            &'a self,
            action: libredfish::SystemPowerControl,
        ) -> RedfishFuture<'a, Result<(), RedfishError>> {
            self.inner.power(action)
        }
    }

    /// Each input, including repeats, must retain its identity and reported
    /// state. Observing power must use its endpoint's credentials and vendor
    /// without issuing power commands.
    #[tokio::test]
    async fn power_reads_preserve_states_identity_and_order_without_mutations() {
        // Transitional and unknown states must survive alongside on and off.
        let states = [
            (RedfishPowerState::On, PowerState::On),
            (RedfishPowerState::Off, PowerState::Off),
            (RedfishPowerState::PoweringOn, PowerState::PoweringOn),
            (RedfishPowerState::PoweringOff, PowerState::PoweringOff),
            (RedfishPowerState::Paused, PowerState::Paused),
            // libredfish's non-standard Reset is a restart in progress.
            (RedfishPowerState::Reset, PowerState::PoweringOn),
            (RedfishPowerState::Unknown, PowerState::Unknown),
        ];

        // Repeat one endpoint to catch accidental deduplication of results.
        let mut endpoints: Vec<_> = (1..=7).map(test_compute_tray_endpoint).collect();
        endpoints.push(endpoints[0].clone());
        let pool = compute_read_test_pool(&endpoints);
        let manager = CoreComputeTrayManager::new(pool.clone());
        assert!(manager.get_power_state(&[]).await.unwrap().is_empty());

        for (endpoint, (state, _)) in endpoints.iter().zip(states) {
            pool.sim
                .set_power_state(&endpoint.bmc_ip.to_string(), state);
        }

        let before = pool.sim.timepoint();
        let results = manager.get_power_state(&endpoints).await.unwrap();

        // Every result belongs to its input slot, even the repeated endpoint.
        let expected: Vec<_> = endpoints
            .iter()
            .zip(states.iter().chain(&states[..1]))
            .map(|(endpoint, (_, state))| ComponentPowerStateResult {
                mac_address: endpoint.bmc_mac,
                power_state: Ok(Some(*state)),
            })
            .collect();
        assert_eq!(results, expected);
        assert!(pool.sim.actions_since(&before).all_hosts().is_empty());

        // Each input must create its own client with the supplied connection
        // details; the pool also checks exact credentials and port.
        let calls = pool.sim.create_client_calls();
        assert_eq!(calls.len(), endpoints.len());

        for endpoint in &endpoints {
            let host = endpoint.bmc_ip.to_string();
            let expected_count = endpoints
                .iter()
                .filter(|candidate| candidate.bmc_ip == endpoint.bmc_ip)
                .count();
            assert_eq!(
                calls.iter().filter(|call| call.host == host).count(),
                expected_count
            );
        }

        for call in calls {
            assert_eq!(call.vendor, map_vendor(pool.endpoints[&call.host].vendor));
            assert_eq!(call.auth, RedfishAuthKind::Direct);
        }
    }

    /// Client creation and power-read failures must identify the failed phase
    /// on that device, while a healthy peer still returns its observation.
    #[tokio::test]
    async fn power_reads_report_client_and_read_failures_per_device() {
        for (phase, expected_prefix) in [
            (
                PowerReadPhase::ClientCreation,
                "failed to create Redfish client:",
            ),
            (PowerReadPhase::PowerRead, "redfish power read failed:"),
        ] {
            let endpoints = [test_compute_tray_endpoint(1), test_compute_tray_endpoint(2)];
            let pool = compute_read_test_pool(&endpoints);
            let healthy_setup = pool.setup_gates.block(&endpoints[1]);
            pool.sim
                .set_power_state(&endpoints[1].bmc_ip.to_string(), RedfishPowerState::Off);

            // Setup errors persist until cleared; a read error affects only
            // the next read. Hold the healthy peer so the first device fails.
            match phase {
                PowerReadPhase::ClientCreation => {
                    pool.sim.set_create_client_error("client unavailable")
                }
                PowerReadPhase::PowerRead => {
                    pool.sim.fail_next_power_state_read("read unavailable")
                }
            }

            let manager = CoreComputeTrayManager::new(pool.clone());
            let before = pool.sim.timepoint();
            let mut request = Box::pin(manager.get_power_state(&endpoints));

            // Keep the healthy peer out of the simulator until the first
            // device has consumed the injected setup or read failure.
            assert!(futures_util::poll!(request.as_mut()).is_pending());
            let calls = pool.sim.create_client_calls();
            assert_eq!(calls.len(), 1);
            assert_eq!(calls[0].host, endpoints[0].bmc_ip.to_string());

            // Release the healthy peer only after removing any setup error.
            pool.sim.clear_create_client_error();
            healthy_setup.send(()).unwrap();
            let results = request.await.unwrap();

            // Failure context must stay with the first device, without hiding
            // or changing the second device's state.
            assert_eq!(results.len(), endpoints.len());
            assert_eq!(results[0].mac_address, endpoints[0].bmc_mac);
            let error = results[0].power_state.as_ref().unwrap_err();
            assert!(error.starts_with(expected_prefix), "{phase:?}: {error}");
            assert!(error.contains("unavailable"), "{phase:?}: {error}");
            assert_eq!(
                results[1],
                ComponentPowerStateResult {
                    mac_address: endpoints[1].bmc_mac,
                    power_state: Ok(Some(PowerState::Off)),
                }
            );

            assert_eq!(pool.sim.create_client_calls().len(), endpoints.len());
            assert!(pool.sim.actions_since(&before).all_hosts().is_empty());
        }
    }

    /// Queueing, setup, and reads share one deadline, so slow devices cannot
    /// extend a request or discard completed observations. Using more than
    /// eight blocked devices also exercises the concurrency limit.
    #[tokio::test(start_paused = true)]
    async fn power_read_deadline_bounds_concurrency_and_preserves_completed_observations() {
        // Complete one observation immediately and another after six seconds,
        // while the remaining devices stay blocked or queued.
        let endpoints: Vec<_> = (0..11).map(test_compute_tray_endpoint).collect();
        let pool = compute_read_test_pool(&endpoints);
        pool.sim
            .set_power_state(&endpoints[0].bmc_ip.to_string(), RedfishPowerState::On);
        pool.sim
            .set_power_state(&endpoints[3].bmc_ip.to_string(), RedfishPowerState::Off);
        let mut setup_senders: Vec<_> = endpoints[1..]
            .iter()
            .map(|endpoint| pool.setup_gates.block(endpoint))
            .collect();
        let first_read = pool.read_gates.block(&endpoints[1]);
        let later_read = pool.read_gates.block(&endpoints[3]);

        let manager = CoreComputeTrayManager::new(pool.clone());
        let before = pool.sim.timepoint();
        let mut request = Box::pin(manager.get_power_state(&endpoints));

        // The first device completes immediately, leaving eight setup waits
        // active and the last two devices still queued.
        assert!(futures_util::poll!(request.as_mut()).is_pending());
        assert_eq!(pool.setup_gates.started.load(Ordering::Relaxed), 8);

        // Setup and power reads must spend the same budget. Start two reads
        // after six seconds, leaving one blocked and finishing the later one.
        tokio::time::advance(Duration::from_secs(6)).await;
        setup_senders.remove(0).send(()).unwrap();
        setup_senders.remove(1).send(()).unwrap();
        later_read.send(()).unwrap();
        assert!(futures_util::poll!(request.as_mut()).is_pending());
        assert_eq!(pool.read_gates.started.load(Ordering::Relaxed), 2);
        assert_eq!(pool.setup_gates.started.load(Ordering::Relaxed), 9);

        // The newly admitted device gets the remaining four seconds, not a
        // fresh deadline. Also prove that the request does not expire early.
        tokio::time::advance(Duration::from_millis(3999)).await;
        assert!(futures_util::poll!(request.as_mut()).is_pending());

        // Tokio rounds timer deadlines up to its next millisecond tick.
        tokio::time::advance(Duration::from_millis(2)).await;
        let std::task::Poll::Ready(results) = futures_util::poll!(request.as_mut()) else {
            panic!("power read exceeded the ten-second request budget");
        };

        // Completed observations survive in their original slots; every
        // unfinished device, including the queued one, reports a timeout.
        let results = results.unwrap();
        assert_eq!(results.len(), endpoints.len());

        for (index, result) in results.iter().enumerate() {
            assert_eq!(result.mac_address, endpoints[index].bmc_mac);
            if index == 0 {
                assert_eq!(result.power_state, Ok(Some(PowerState::On)));
            } else if index == 3 {
                assert_eq!(result.power_state, Ok(Some(PowerState::Off)));
            } else {
                assert_eq!(result.power_state, Err(POWER_READ_TIMEOUT_ERROR.into()));
            }
        }

        // Started waits lose their receivers when the deadline cancels them.
        // The final setup receiver stays registered because it was not reached.
        assert_eq!(pool.setup_gates.started.load(Ordering::Relaxed), 9);
        assert_eq!(pool.read_gates.started.load(Ordering::Relaxed), 2);
        assert!(
            first_read.is_closed(),
            "timed-out power read was not dropped"
        );
        assert!(
            setup_senders[..setup_senders.len() - 1]
                .iter()
                .all(oneshot::Sender::is_closed)
        );
        assert!(
            !setup_senders.last().unwrap().is_closed(),
            "queued device started after deadline"
        );
        assert!(pool.sim.actions_since(&before).all_hosts().is_empty());
    }

    /// Setup completing exactly at the deadline must not start a power read.
    /// This protects the explicit guard after setup: `timeout_at` polls its
    /// inner future before checking whether the deadline has expired.
    #[tokio::test(start_paused = true)]
    async fn setup_finishing_at_deadline_does_not_start_a_power_read() {
        let endpoints = [test_compute_tray_endpoint(1)];
        let pool = compute_read_test_pool(&endpoints);
        let setup = pool.setup_gates.block(&endpoints[0]);
        let read = pool.read_gates.block(&endpoints[0]);
        let manager = CoreComputeTrayManager::new(pool.clone());
        let before = pool.sim.timepoint();
        let mut request = Box::pin(manager.get_power_state(&endpoints));

        // Reach setup while keeping the read gate registered but untouched.
        assert!(futures_util::poll!(request.as_mut()).is_pending());
        assert_eq!(pool.setup_gates.started.load(Ordering::Relaxed), 1);

        // Make setup ready on the deadline itself. `timeout_at` polls it
        // first, so only the post-setup guard can prevent the power read.
        tokio::time::advance(Duration::from_secs(10)).await;
        setup.send(()).unwrap();
        let std::task::Poll::Ready(results) = futures_util::poll!(request.as_mut()) else {
            panic!("setup finishing at the deadline did not return immediately");
        };

        // Client creation finished, but the read gate must remain unconsumed.
        assert_eq!(
            results.unwrap(),
            vec![ComponentPowerStateResult {
                mac_address: endpoints[0].bmc_mac,
                power_state: Err(POWER_READ_TIMEOUT_ERROR.into()),
            }]
        );
        assert_eq!(pool.sim.create_client_calls().len(), 1);
        assert_eq!(pool.read_gates.started.load(Ordering::Relaxed), 0);
        assert!(!read.is_closed(), "power read started at the deadline");
        assert!(pool.sim.actions_since(&before).all_hosts().is_empty());
    }

    /// Dropping the caller's future must drop active setup or read waits and
    /// leave queued work unstarted. No detached task may keep using the BMCs.
    #[tokio::test]
    async fn dropping_power_read_request_cancels_setup_reads_and_queued_work() {
        for phase in [PowerReadPhase::ClientCreation, PowerReadPhase::PowerRead] {
            // Block nine devices at the selected phase so one stays queued
            // behind the eight active requests.
            let endpoints: Vec<_> = (0..9).map(test_compute_tray_endpoint).collect();
            let pool = compute_read_test_pool(&endpoints);
            let gates = match phase {
                PowerReadPhase::ClientCreation => &pool.setup_gates,
                PowerReadPhase::PowerRead => pool.read_gates.as_ref(),
            };
            let senders: Vec<_> = endpoints
                .iter()
                .map(|endpoint| gates.block(endpoint))
                .collect();

            let manager = CoreComputeTrayManager::new(pool.clone());
            let before = pool.sim.timepoint();
            let mut request = Box::pin(manager.get_power_state(&endpoints));

            // Reach the eight waits before simulating caller cancellation.
            assert!(
                futures_util::poll!(request.as_mut()).is_pending(),
                "{phase:?}"
            );
            assert_eq!(gates.started.load(Ordering::Relaxed), 8, "{phase:?}");
            drop(request);

            // Consumed receivers drop with the request; the ninth receiver
            // remains registered in the fixture because no call reached it.
            assert!(
                senders[..8].iter().all(oneshot::Sender::is_closed),
                "{phase:?}"
            );
            assert!(
                !senders.last().unwrap().is_closed(),
                "{phase:?}: queued device started after cancellation"
            );
            assert_eq!(gates.started.load(Ordering::Relaxed), 8, "{phase:?}");
            assert!(
                pool.sim.actions_since(&before).all_hosts().is_empty(),
                "{phase:?}"
            );
        }
    }

    /// Vendor selection must use the matching Redfish adapter where supported
    /// and leave the generic path available for other compute trays.
    #[test]
    fn compute_tray_vendor_maps_to_redfish_vendor() {
        value_scenarios!(map_vendor:
            "supported Redfish vendors" {
                ComputeTrayVendor::Dell => Some(RedfishVendor::Dell),
                ComputeTrayVendor::Hpe => Some(RedfishVendor::Hpe),
                ComputeTrayVendor::Lenovo => Some(RedfishVendor::Lenovo),
                ComputeTrayVendor::Supermicro => Some(RedfishVendor::Supermicro),
            }

            "vendors without a Redfish adapter" {
                ComputeTrayVendor::Nvidia => None,
                ComputeTrayVendor::Unknown => None,
            }
        );
    }
}
