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

#[cfg(feature = "cli")]
use std::sync::atomic::{AtomicBool, Ordering};

pub mod bundle;
pub mod journal;
pub mod machine;
pub mod pcr;
pub mod profile;
pub mod records;
pub mod report;
pub mod site;

#[derive(thiserror::Error, Debug)]
pub enum Error {
    #[error("{0}")]
    Parse(String),
    #[error("{0}")]
    RpcConversion(String),
}

pub type Result<T> = std::result::Result<T, Error>;

pub trait DisplayName {
    fn display_name() -> &'static str;
}

/// SUMMARY is a global variable that is being used by a few structs which
/// implement serde::Serialize with skip_serialization_if.
///
/// I had wanted the ability to have summarized or extended versions of
/// serialized output, and decided I could use skip_serialization_if along with
/// a function that looks at a global variable.
///
/// You set --extended on the CLI, which controls whether or not to summarized
/// (default is summarized).
#[cfg(feature = "cli")]
static SUMMARY: AtomicBool = AtomicBool::new(false);

#[cfg(feature = "cli")]
pub fn serde_just_print_summary<T>(_: &T) -> bool {
    SUMMARY.load(Ordering::SeqCst)
}

#[cfg(feature = "cli")]
pub fn just_print_summary() -> bool {
    SUMMARY.load(Ordering::SeqCst)
}

#[cfg(feature = "cli")]
pub fn set_summary(val: bool) {
    SUMMARY.store(val, Ordering::SeqCst);
}

/// ToTable is a trait which is used alongside the cli_output command
/// and being able to prettytable print results.
#[cfg(feature = "cli")]
pub trait ToTable {
    fn into_table(self) -> eyre::Result<String>;
}
