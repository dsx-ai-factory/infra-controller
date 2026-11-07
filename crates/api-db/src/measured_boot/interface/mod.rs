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

/*!
 * The `interface` module provides thin functions to connect db
 * types to the database via queries.
 *
 * This includes basic insert/select/delete/update calls for:
 *  - `bundle`: Measurement bundles.
 *  - `common`: Generic functions leveraged by all interfaces.
 *  - `journal`: Measurement journals.
 *  - `machine`: Mock machines (will eventually go away).
 *  - `profile`: System profiles.
 *  - `report`: Machine measurement reports.
 *  - `site`: Site management.
 */

pub mod bundle;
pub mod common;
pub mod journal;
pub mod machine;
pub mod profile;
pub mod report;
pub mod site;
