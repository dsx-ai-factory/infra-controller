// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

//! Reusable provider checks and an in-memory provider for API tests.

mod conformance;
mod provider;

pub use conformance::verify_provider;
pub use provider::LocalProvider;
