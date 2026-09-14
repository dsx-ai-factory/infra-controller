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

/// `ConditionalWrite` distinguishes an applied write from an unmet precondition.
/// Database failures remain in the enclosing `Result`; the caller decides how
/// to handle `NotApplied`. `Applied` does not commit a surrounding transaction.
/// Propagating the database error alone does not handle a rejected write:
///
/// ```compile_fail
/// # use db::{ConditionalWrite, ControllerStateNotCurrent};
/// # fn write() -> Result<ConditionalWrite<(), ControllerStateNotCurrent>, ()> {
/// #     Ok(ConditionalWrite::Applied(()))
/// # }
/// fn caller() -> Result<(), ()> {
///     #![deny(unused_must_use)]
///     write()?;
///     Ok(())
/// }
/// ```
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[must_use = "conditional write results must be handled"]
pub enum ConditionalWrite<T, R> {
    /// The write applied, producing `T`.
    Applied(T),
    /// The write did not apply for the domain-specific reason `R`.
    NotApplied(R),
}

/// `ControllerStateNotCurrent` means the row is missing or its controller-state
/// version no longer matches the snapshot. These cases share one rejection;
/// the write does not distinguish them.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ControllerStateNotCurrent;
