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

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering as AtomicOrdering};

use chrono::Duration;
use serde::{Deserialize, Deserializer, Serializer};

pub fn deserialize_arc_atomic_bool<'de, D>(deserializer: D) -> Result<Arc<AtomicBool>, D::Error>
where
    D: Deserializer<'de>,
{
    let b = bool::deserialize(deserializer)?;
    Ok(Arc::new(b.into()))
}

pub fn serialize_arc_atomic_bool<S>(cm: &Arc<AtomicBool>, s: S) -> Result<S::Ok, S::Error>
where
    S: Serializer,
{
    s.serialize_bool(cm.load(AtomicOrdering::Relaxed))
}

/// As of now, chrono::Duration does not support Serialization, so we have to handle it manually.
pub fn as_duration<S>(d: &Duration, serializer: S) -> Result<S::Ok, S::Error>
where
    S: Serializer,
{
    serializer.serialize_str(&format!("{}s", d.num_seconds()))
}

pub fn as_std_duration<S>(d: &std::time::Duration, serializer: S) -> Result<S::Ok, S::Error>
where
    S: Serializer,
{
    serializer.serialize_str(&format!("{}s", d.as_secs()))
}

pub fn as_option_duration<S>(d: &Option<Duration>, serializer: S) -> Result<S::Ok, S::Error>
where
    S: Serializer,
{
    match d {
        Some(d) => serializer.serialize_some(&format!("{}s", d.num_seconds())),
        None => serializer.serialize_none(),
    }
}
