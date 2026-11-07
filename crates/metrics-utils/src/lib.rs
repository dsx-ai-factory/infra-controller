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

use glob::Pattern;
use opentelemetry_sdk::metrics::{Aggregation, Instrument, InstrumentKind, Stream};

/// Type alias that matches the closure signature for otel's `MeterProviderBuilder::with_view`
pub type OtelView = Box<dyn Fn(&Instrument) -> Option<Stream> + Send + Sync + 'static>;

#[derive(thiserror::Error, Debug)]
pub enum Error {
    #[error("error building opentelemetry view: {0}")]
    ViewBuilding(#[from] NewViewError),
}

pub type Result<T> = std::result::Result<T, Error>;

/// Replacement for the now-removed `opentelemetry_sdk::metrics::view::new_view`
///
/// Implements a "View" (which now means a closure) that matches [`Instrument`]'s that have:
///
/// - A name matching `name`, which can be a unix-style glob
/// - A kind matching `kind`
///
/// And returns a stream with the aggregation provided by `aggregation`.
pub fn new_view(
    name: &'static str,
    kind: Option<InstrumentKind>,
    aggregation: Aggregation,
) -> Result<OtelView> {
    let glob_pattern = if name.contains(['*', '?']) {
        Some(Pattern::new(name).map_err(|error| NewViewError::Glob { name, error })?)
    } else {
        None
    };

    Ok(Box::new(move |i: &Instrument| -> Option<Stream> {
        if let Some(kind) = kind
            && i.kind() != kind
        {
            return None;
        }

        if let Some(glob_pattern) = glob_pattern.as_ref() {
            if !glob_pattern.matches(i.name()) {
                return None;
            }
        } else if i.name() != name {
            return None;
        }

        Stream::builder()
            .with_aggregation(aggregation.clone())
            .with_name(i.name().to_owned())
            .with_unit(i.unit().to_owned())
            .build()
            .inspect_err(|e| {
                tracing::error!(
                    view = i.name(),
                    error = %e,
                    "BUG: Could not build stream from view"
                )
            })
            .ok()
    }))
}

#[derive(thiserror::Error, Debug)]
pub enum NewViewError {
    #[error("metrics view name {name:?} is an invalid glob: {error}")]
    Glob {
        name: &'static str,
        error: glob::PatternError,
    },
}
