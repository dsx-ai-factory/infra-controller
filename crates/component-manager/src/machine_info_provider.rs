// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

//! Backend-neutral contracts for machine slot and tray discovery.

use std::error::Error;
use std::net::IpAddr;
#[cfg(feature = "test-support")]
use std::sync::atomic::{AtomicUsize, Ordering};
#[cfg(feature = "test-support")]
use std::time::Duration;

use carbide_secrets::credentials::Credentials;
use carbide_uuid::rack::RackId;
use mac_address::MacAddress;
use model::rack_type::RackProfile;

pub(crate) mod sealed {
    pub trait Sealed {}
}

/// A machine whose physical rack location should be queried.
///
/// # Examples
///
/// ```
/// use std::net::{IpAddr, Ipv4Addr};
///
/// use carbide_uuid::rack::RackId;
/// use component_manager::MachineLocationTarget;
/// use mac_address::MacAddress;
/// use model::rack_type::RackProfile;
///
/// let profile = RackProfile::default();
/// let target = MachineLocationTarget {
///     node_id: "machine-1".into(),
///     rack_id: RackId::new("rack-1"),
///     profile: &profile,
///     bmc_ip: IpAddr::V4(Ipv4Addr::new(192, 0, 2, 10)),
///     bmc_mac: MacAddress::new([0x02, 0, 0, 0, 0, 1]),
///     credentials: None,
/// };
/// assert_eq!(target.node_id, "machine-1");
/// ```
pub struct MachineLocationTarget<'a> {
    /// Stable identifier used to correlate the backend response.
    pub node_id: String,

    /// Rack that contains the machine.
    pub rack_id: RackId,

    /// Rack profile used by the backend to identify the machine type.
    pub profile: &'a RackProfile,

    /// Machine BMC IP address.
    pub bmc_ip: IpAddr,

    /// Machine BMC MAC address.
    pub bmc_mac: MacAddress,

    /// Optional BMC credentials used by the backend.
    pub credentials: Option<Credentials>,
}

/// Physical location observed for a machine.
///
/// # Examples
///
/// ```
/// use component_manager::MachineLocationObservation;
///
/// let location = MachineLocationObservation {
///     node_id: "machine-1".into(),
///     slot_number: Some(4),
///     tray_index: Some(1),
/// };
/// assert_eq!(location.slot_number, Some(4));
/// ```
#[derive(Debug, PartialEq, Eq)]
pub struct MachineLocationObservation {
    /// Stable identifier copied from the backend response.
    pub node_id: String,

    /// Physical slot number, when reported by the backend.
    pub slot_number: Option<u32>,

    /// Physical tray index, when reported by the backend.
    pub tray_index: Option<u32>,
}

/// Failure returned by a machine-location backend.
///
/// The underlying backend error remains available through [`Error::source`].
#[derive(Debug, thiserror::Error)]
#[error("{0}")]
pub struct MachineLocationError(#[source] Box<dyn Error + Send + Sync>);

impl MachineLocationError {
    pub(crate) fn new(error: impl Error + Send + Sync + 'static) -> Self {
        Self(Box::new(error))
    }

    #[cfg(feature = "test-support")]
    fn message(message: impl ToString) -> Self {
        Self(message.to_string().into())
    }
}

/// Backend contract for validating targets and querying machine information.
#[async_trait::async_trait]
pub trait MachineInfoProvider: sealed::Sealed + Send + Sync {
    /// Validates the rack-profile fields required to query a machine location.
    ///
    /// # Errors
    ///
    /// Returns a backend error when the profile cannot identify a compute node.
    fn validate_profile(&self, profile: &RackProfile) -> Result<(), MachineLocationError>;

    /// Queries physical locations for a batch of machines.
    ///
    /// Returned observations retain backend node IDs and raw unsigned location
    /// values so the caller can apply its own correlation and range policy.
    ///
    /// # Errors
    ///
    /// Returns a backend error when the batch request cannot be completed.
    async fn get_machine_locations(
        &self,
        targets: Vec<MachineLocationTarget<'_>>,
    ) -> Result<Vec<MachineLocationObservation>, MachineLocationError>;
}

/// Controllable machine-information provider for integration tests.
#[cfg(feature = "test-support")]
#[derive(Debug, Default)]
pub struct TestMachineInfoProvider {
    responses: tokio::sync::Mutex<
        std::collections::VecDeque<Result<Vec<MachineLocationObservation>, MachineLocationError>>,
    >,
    delay: tokio::sync::Mutex<Duration>,
    call_count: AtomicUsize,
}

#[cfg(feature = "test-support")]
impl TestMachineInfoProvider {
    /// Queues machine locations for the next query.
    pub async fn enqueue_locations(&self, locations: Vec<MachineLocationObservation>) {
        self.responses.lock().await.push_back(Ok(locations));
    }

    /// Queues a synthetic provider error for the next query.
    pub async fn enqueue_error(&self, error: impl ToString) {
        self.responses
            .lock()
            .await
            .push_back(Err(MachineLocationError::message(error)));
    }

    /// Sets the delay applied before each query completes.
    pub async fn set_delay(&self, delay: Duration) {
        *self.delay.lock().await = delay;
    }

    /// Returns the number of received location queries.
    pub fn call_count(&self) -> usize {
        self.call_count.load(Ordering::Relaxed)
    }
}

#[cfg(feature = "test-support")]
impl sealed::Sealed for TestMachineInfoProvider {}

#[cfg(feature = "test-support")]
#[async_trait::async_trait]
impl MachineInfoProvider for TestMachineInfoProvider {
    fn validate_profile(&self, _profile: &RackProfile) -> Result<(), MachineLocationError> {
        Ok(())
    }

    async fn get_machine_locations(
        &self,
        _targets: Vec<MachineLocationTarget<'_>>,
    ) -> Result<Vec<MachineLocationObservation>, MachineLocationError> {
        self.call_count.fetch_add(1, Ordering::Relaxed);

        let delay = *self.delay.lock().await;

        if !delay.is_zero() {
            tokio::time::sleep(delay).await;
        }

        self.responses.lock().await.pop_front().ok_or_else(|| {
            MachineLocationError::message(
                "no TestMachineInfoProvider response was queued for the query",
            )
        })?
    }
}
