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
use tracing_subscriber::filter::Targets;
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

    /// `current` as plain target directives, for answering `enables` without a subscriber.
    /// `None` when `current` has directives that cannot be expressed that way.
    targets: ArcSwap<Option<Targets>>,
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
            targets: Default::default(),
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
            targets: ArcSwap::from_pointee(f.to_string().parse().ok()),
        }
    }

    /// Whether the current filter lets `target` log at `level`. Answers true when the filter
    /// has directives that cannot be evaluated by target and level alone, so callers err toward
    /// producing the output the filter might allow.
    pub fn enables(&self, target: &str, level: &tracing::Level) -> bool {
        let targets = self.targets.load();
        match &**targets {
            Some(targets) => targets.would_enable(target, level),
            None => true,
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
        self.targets
            .store(Arc::new(current.to_string().parse().ok()));
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

#[cfg(test)]
mod tests {
    use carbide_test_support::{Check, check_values};

    use super::*;

    #[test]
    fn enables_follows_the_filter_at_startup_and_after_reload() {
        let checks = [
            Check {
                scenario: "default level excludes DEBUG",
                input: "info",
                expect: false,
            },
            Check {
                scenario: "target directive raises one target",
                input: "info,rms_rpc_audit=debug",
                expect: true,
            },
            Check {
                scenario: "target directive overrides a DEBUG default",
                input: "debug,rms_rpc_audit=off",
                expect: false,
            },
            Check {
                scenario: "span directives cannot be evaluated, so the answer errs toward true",
                input: "info,other[span{field=1}]=trace",
                expect: true,
            },
        ];
        check_values(checks, |filter| {
            let parse = |filter: &str| dep_log_filter(EnvFilter::builder().parse(filter).unwrap());
            let at_startup = ActiveLevel::new(parse(filter), None);
            let reloaded = ActiveLevel::new(parse("error"), None);
            reloaded.update(filter, None).unwrap();
            let enables =
                |level: &ActiveLevel| level.enables("rms_rpc_audit", &tracing::Level::DEBUG);
            let (startup, reload) = (enables(&at_startup), enables(&reloaded));
            assert_eq!(startup, reload, "startup and reload disagree for {filter}");
            startup
        });
    }
}
