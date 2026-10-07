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

use std::fmt;
use std::sync::Arc;

use arc_swap::ArcSwap;
use chrono::{DateTime, Utc};
use tracing_subscriber::{EnvFilter, reload};

use crate::logging::setup::dep_log_filter;

pub trait Reloadable: Send + Sync {
    fn reload(&self, f: EnvFilter) -> Result<(), eyre::Error>;
}

#[derive(Debug)]
pub struct ReloadableFilter<S> {
    handle: ReloadHandle<S>,
}

impl<S> ReloadableFilter<S> {
    pub fn new(handle: ReloadHandle<S>) -> Self {
        Self { handle }
    }
}

impl<S> Reloadable for ReloadableFilter<S> {
    fn reload(&self, f: EnvFilter) -> Result<(), eyre::Error> {
        Ok(self.handle.reload(f)?)
    }
}

pub(crate) type ReloadHandle<S> = reload::Handle<EnvFilter, S>;

/// The current RUST_LOG setting.
/// Immutable. Owner holds it in an ArcSwap and replaces the whole object using one of `with_base` or
/// `reset_from`.
pub struct ActiveLevel {
    /// Handle to reload the logging level.
    reload_handle: Option<Box<dyn Reloadable>>,

    /// The current RUST_LOG
    current: ArcSwap<String>,

    /// The RUST_LOG we had on startup
    base: String,

    /// When to switch back to the RUST_LOG we had on startup
    expiry: ArcSwap<Option<DateTime<Utc>>>,
}

impl fmt::Debug for ActiveLevel {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "ActiveLevel{{ current: {:?}, base: {:?}, expiry: {:?} }}",
            self.current, self.base, self.expiry
        )
    }
}

impl Default for ActiveLevel {
    fn default() -> Self {
        Self {
            reload_handle: None,
            current: Default::default(),
            base: "".to_string(),
            expiry: Default::default(),
        }
    }
}

impl ActiveLevel {
    pub fn new(f: EnvFilter, reload_handle: Option<Box<dyn Reloadable>>) -> Self {
        Self {
            current: ArcSwap::new(f.to_string().into()),
            base: f.to_string(),
            expiry: Default::default(),
            reload_handle,
        }
    }

    // Build a new ActiveLevel with the same 'base' as caller
    pub(crate) fn update(
        &self,
        filter: &str,
        until: Option<DateTime<Utc>>,
    ) -> Result<(), eyre::Error> {
        let current = dep_log_filter(EnvFilter::builder().parse(filter)?);
        self.expiry.store(until.into());
        if let Some(handle) = self.reload_handle.as_ref() {
            handle.reload(current.clone())?;
        }
        self.current.store(Arc::new(current.to_string()));
        Ok(())
    }

    // Build a new ActiveLevel use 'base' as the RUST_LOG
    pub(crate) fn reset_if_expired(&self) -> Result<(), eyre::Error> {
        if let Some(expiry) = self.expiry.load().as_ref()
            && *expiry < chrono::Utc::now()
        {
            self.update(&self.base, None)
        } else {
            Ok(())
        }
    }
}

impl fmt::Display for ActiveLevel {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let current = self.current.to_string();
        match self.expiry.load().as_ref() {
            None => write!(f, "{current}"),
            Some(exp) => write!(f, "{current} until {exp}"),
        }
    }
}
