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

//! Thin top crate for the `carbide-api` server. Almost nothing lives here — the
//! runtime code is split across two other crates:
//!
//!   - [`carbide_api_core`] — the server itself: the [`Api`]/`Forge` service, the
//!     request handlers, and the core service runtime.
//!   - [`carbide_api_web`] — the admin web UI (the HTML pages under `/admin`).
//!
//! `carbide-api-web` depends on `carbide-api-core` (it needs the [`Api`] type), so
//! `carbide-api-core` can't depend back on `carbide-api-web` — that would be a
//! dependency cycle. But *something* has to know about both in order to hand the
//! web pages to the server at startup. That's this crate's whole job: it's the one
//! place allowed to depend on both, so the wiring happens here (see `main.rs`).
//!
//! This crate owns process bootstrap and exports [`run`] so that the
//! `carbide-api` binary and integration tests have one stable entrypoint.

mod command_line;
mod logging;
mod metrics;
mod postgres;
mod resources;
mod run;
mod shutdown_handler;

pub use carbide_api_core::AdminUiRoutesBuilder;
pub use command_line::{Command, Options};
pub use postgres::postgres_connect_options;
pub use run::{ApiServerAddresses, run};
