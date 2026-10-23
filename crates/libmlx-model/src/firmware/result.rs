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

use serde::{Deserialize, Serialize};

/// `FirmwareFlashReport` carries firmware progress from libmlx through Scout
/// to Core. It is not a durable attempt record or approval to retry a mutation.
///
/// For libmlx execution, optional phases follow `FlashOptions`: `None` means
/// unrequested, `Some(true)` means completed, and `Some(false)` means failed or
/// not reached after an earlier failure. Dry-run leaves requested phases false.
/// Older Scout senders may report success without a new burn when no profile
/// was supplied. Correlated operations instead report `flashed=false` for
/// that completed request.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct FirmwareFlashReport {
    /// Whether the burn completed. Older Scout senders also set this when no profile
    /// was supplied; it is not proof of a new flash attempt.
    pub flashed: bool,
    /// Whether the requested reset completed; absent when unrequested.
    pub reset: Option<bool>,
    /// Whether verification against the source image completed successfully;
    /// absent when unrequested.
    pub verified_image: Option<bool>,
    /// Whether the observed version matches the exact target; absent when
    /// unrequested. This does not establish which firmware is running after reset.
    pub verified_version: Option<bool>,
    /// The last usable version reported by `mlxfwmanager` during this operation,
    /// including a mismatched version, retained across subsequent failed queries.
    /// Absent when no usable version was observed, including dry-run or no query.
    pub observed_version: Option<String>,
    /// The literal target version from the configuration, when supplied.
    pub expected_version: Option<String>,
}
