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

use carbide_uuid::secret::SecretId;
use chrono::{DateTime, Utc};

/// One row of the `secrets` table: an envelope-encrypted credential value
/// plus its wrapped DEK. `seq` is the journal order -- higher means written
/// later. Decryption lives in `carbide::secrets`; this type only moves the
/// columns.
#[derive(sqlx::FromRow)]
pub struct SecretRow {
    pub secret_id: SecretId,
    pub seq: i64,
    pub path: String,
    pub encrypted_value: Vec<u8>,
    pub nonce: Vec<u8>,
    pub kek_id: String,
    pub created_at: DateTime<Utc>,
    pub encrypted_dek: Vec<u8>,
    pub dek_nonce: Vec<u8>,
}
