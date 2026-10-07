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

use std::num::{NonZeroU32, NonZeroUsize};
use std::time::Duration;

use tokio::sync::Semaphore;

#[derive(Debug, thiserror::Error)]
pub(crate) enum AdmissionLimitsError {
    #[error("max_work_in_flight must be greater than zero")]
    ZeroWorkCapacity,
    #[error("max_work_in_flight must not exceed {maximum}")]
    WorkCapacityTooLarge { maximum: usize },
    #[error("max_pending must be greater than zero")]
    ZeroPendingCapacity,
    #[error("max_pending must not exceed {maximum}")]
    PendingCapacityTooLarge { maximum: usize },
    #[error("max_work_in_flight_per_client must be greater than zero")]
    ZeroClientWorkCapacity,
    #[error("max_work_in_flight_per_client must not exceed max_work_in_flight")]
    ClientWorkCapacityTooLarge,
    #[error("max_pending_per_client must be greater than zero")]
    ZeroClientPendingCapacity,
    #[error("max_pending_per_client must not exceed max_pending")]
    ClientPendingCapacityTooLarge,
    #[error("pending_timeout must be greater than zero")]
    ZeroPendingTimeout,
    #[error("client_idle_timeout must be greater than zero")]
    ZeroClientIdleTimeout,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct ClientLimits {
    pub(super) max_work_in_flight: NonZeroU32,
    pub(super) max_pending: NonZeroUsize,
    pending_timeout: Duration,
}

impl ClientLimits {
    pub(crate) fn new(
        max_work_in_flight: usize,
        max_pending: usize,
        pending_timeout: Duration,
        global_max_work_in_flight: usize,
        global_max_pending: usize,
    ) -> Result<Self, AdmissionLimitsError> {
        if max_work_in_flight == 0 {
            return Err(AdmissionLimitsError::ZeroClientWorkCapacity);
        }
        if max_work_in_flight > global_max_work_in_flight
            || u32::try_from(max_work_in_flight).is_err()
        {
            return Err(AdmissionLimitsError::ClientWorkCapacityTooLarge);
        }
        if max_pending == 0 {
            return Err(AdmissionLimitsError::ZeroClientPendingCapacity);
        }
        if max_pending > global_max_pending {
            return Err(AdmissionLimitsError::ClientPendingCapacityTooLarge);
        }
        if pending_timeout.is_zero() {
            return Err(AdmissionLimitsError::ZeroPendingTimeout);
        }

        Ok(Self {
            max_work_in_flight: NonZeroU32::new(
                u32::try_from(max_work_in_flight)
                    .expect("validated client work capacity fits in u32"),
            )
            .expect("validated client work capacity is non-zero"),
            max_pending: NonZeroUsize::new(max_pending)
                .expect("validated client pending capacity is non-zero"),
            pending_timeout,
        })
    }

    pub(crate) const fn max_work_in_flight(self) -> usize {
        self.max_work_in_flight.get() as usize
    }

    pub(crate) const fn max_pending(self) -> usize {
        self.max_pending.get()
    }

    pub(crate) const fn pending_timeout(self) -> Duration {
        self.pending_timeout
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct AdmissionLimits {
    pub(super) max_work_in_flight: NonZeroUsize,
    pub(super) max_pending: NonZeroUsize,
    default_client: ClientLimits,
    client_idle_timeout: Duration,
}

impl AdmissionLimits {
    pub(crate) fn new(
        max_work_in_flight: usize,
        max_pending: usize,
        default_client_work_in_flight: usize,
        default_client_pending: usize,
        pending_timeout: Duration,
        client_idle_timeout: Duration,
    ) -> Result<Self, AdmissionLimitsError> {
        if max_work_in_flight == 0 {
            return Err(AdmissionLimitsError::ZeroWorkCapacity);
        }
        if max_work_in_flight > Semaphore::MAX_PERMITS {
            return Err(AdmissionLimitsError::WorkCapacityTooLarge {
                maximum: Semaphore::MAX_PERMITS,
            });
        }
        if max_pending == 0 {
            return Err(AdmissionLimitsError::ZeroPendingCapacity);
        }
        if max_pending > Semaphore::MAX_PERMITS {
            return Err(AdmissionLimitsError::PendingCapacityTooLarge {
                maximum: Semaphore::MAX_PERMITS,
            });
        }
        if client_idle_timeout.is_zero() {
            return Err(AdmissionLimitsError::ZeroClientIdleTimeout);
        }
        let default_client = ClientLimits::new(
            default_client_work_in_flight,
            default_client_pending,
            pending_timeout,
            max_work_in_flight,
            max_pending,
        )?;

        Ok(Self {
            max_work_in_flight: NonZeroUsize::new(max_work_in_flight)
                .expect("validated global work capacity is non-zero"),
            max_pending: NonZeroUsize::new(max_pending)
                .expect("validated global pending capacity is non-zero"),
            default_client,
            client_idle_timeout,
        })
    }

    pub(crate) const fn max_work_in_flight(self) -> usize {
        self.max_work_in_flight.get()
    }

    pub(crate) const fn max_pending(self) -> usize {
        self.max_pending.get()
    }

    pub(crate) const fn default_client(self) -> ClientLimits {
        self.default_client
    }

    pub(super) const fn client_idle_timeout(self) -> Duration {
        self.client_idle_timeout
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn client_limits_reject_global_overrides() {
        assert!(matches!(
            ClientLimits::new(65, 1, Duration::from_secs(1), 64, 1024),
            Err(AdmissionLimitsError::ClientWorkCapacityTooLarge)
        ));
        assert!(matches!(
            ClientLimits::new(1, 1025, Duration::from_secs(1), 64, 1024),
            Err(AdmissionLimitsError::ClientPendingCapacityTooLarge)
        ));
        assert_eq!(
            ClientLimits::new(8, 64, Duration::from_secs(5), 64, 1024)
                .expect("test limits are valid")
                .max_work_in_flight(),
            8
        );
    }
}
