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

use std::any::TypeId;
use std::marker::PhantomData;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

use tracing::span::{self, Attributes};
use tracing::{Event, Subscriber};
use tracing_subscriber::Layer;
use tracing_subscriber::layer::Context;

/// Construct a new `SpanCounterLayer`
pub fn layer<S>() -> SpanCounterLayer<S>
where
    S: Subscriber,
{
    SpanCounterLayer {
        _registry: PhantomData,
        counter: Arc::new(Counter::default()),
    }
}

/// A Layer which counts the amount of spans that are open
pub struct SpanCounterLayer<S> {
    _registry: std::marker::PhantomData<S>,
    counter: Arc<Counter>,
}

impl<S> SpanCounterLayer<S>
where
    S: Subscriber,
{
    /// Returns a `SpanCountReader` that allows to read the amount of open spans
    pub fn reader(&self) -> SpanCountReader {
        SpanCountReader {
            counter: self.counter.clone(),
        }
    }
}

#[derive(Default, Debug)]
struct Counter {
    counter: AtomicUsize,
}

#[derive(Debug, Clone)]
pub struct SpanCountReader {
    counter: Arc<Counter>,
}

impl SpanCountReader {
    /// Returns the current amount of open spans
    pub fn open_spans(&self) -> usize {
        self.counter.counter.load(Ordering::Relaxed)
    }
}

impl<S> Layer<S> for SpanCounterLayer<S>
where
    S: Subscriber,
{
    fn on_new_span(&self, _attrs: &Attributes<'_>, _id: &span::Id, _ctx: Context<'_, S>) {
        self.counter.counter.fetch_add(1, Ordering::Relaxed);
    }

    fn on_enter(&self, _id: &span::Id, _ctx: Context<'_, S>) {}

    fn on_exit(&self, _id: &span::Id, _ctx: Context<'_, S>) {}

    fn on_record(&self, _id: &span::Id, _values: &span::Record<'_>, _ctx: Context<'_, S>) {}

    fn on_follows_from(&self, _id: &span::Id, _follows: &span::Id, _ctx: Context<S>) {}

    fn on_event(&self, _event: &Event<'_>, _ctx: Context<'_, S>) {}

    fn on_close(&self, _id: span::Id, _ctx: Context<'_, S>) {
        self.counter.counter.fetch_sub(1, Ordering::Relaxed);
    }

    // SAFETY: this is safe because the `WithContext` function pointer is valid
    // for the lifetime of `&self`.
    unsafe fn downcast_raw(&self, id: TypeId) -> Option<*const ()> {
        match id {
            id if id == TypeId::of::<Self>() => Some(self as *const _ as *const ()),
            _ => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use tracing_subscriber::prelude::*;

    use super::*;

    #[test]
    fn test_spancounter_layer() {
        let layer = layer::<tracing_subscriber::Registry>();
        let reader = layer.reader();
        let _subscriber = tracing_subscriber::registry().with(layer).set_default();

        assert_eq!(reader.open_spans(), 0);
        let span = tracing::span!(tracing::Level::WARN, "a",);
        assert_eq!(reader.open_spans(), 1);
        let entered = span.enter();
        assert_eq!(reader.open_spans(), 1);
        drop(entered);
        assert_eq!(reader.open_spans(), 1);

        let span2 = tracing::span!(tracing::Level::WARN, "b",);
        assert_eq!(reader.open_spans(), 2);
        drop(span2);
        assert_eq!(reader.open_spans(), 1);

        drop(span);
        assert_eq!(reader.open_spans(), 0);
    }
}
