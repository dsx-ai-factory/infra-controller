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

//! Generic state change emitter for broadcasting state transitions to registered hooks.

use chrono::{DateTime, Utc};

/// Event emitted when a state transition occurs.
///
/// Contains references to the state data to avoid cloning in the controller.
/// Hooks that need to store data should clone only what they need.
#[derive(Debug)]
pub struct StateChangeEvent<'a, Id, S> {
    /// The ID of the object that changed state.
    pub object_id: &'a Id,
    /// The state before the transition (if known).
    #[cfg(any(test, feature = "test-support"))]
    // not used by any hooks yet, only used in tests
    pub previous_state: Option<&'a S>,
    /// The new state after the transition.
    pub new_state: &'a S,
    /// Timestamp when the state change occurred.
    pub timestamp: DateTime<Utc>,
}

/// Trait for hooks that handle state changes.
///
/// Hooks are called synchronously for each event. If a hook needs to perform
/// expensive or async work, it should clone the data it needs and queue it
/// internally for processing in a background task.
pub trait StateChangeHook<Id: Clone, S: Clone>: Send + Sync + 'static {
    /// Called when a state change occurs.
    ///
    /// This method is synchronous and should return quickly. If async work
    /// is needed, the hook should clone needed data and queue it for processing.
    fn on_state_changed(&self, event: &StateChangeEvent<'_, Id, S>);
}

/// Handle for emitting state change events to registered hooks.
///
/// Events are dispatched synchronously to all registered hooks.
pub struct StateChangeEmitter<Id: Clone, S: Clone> {
    hooks: Vec<Box<dyn StateChangeHook<Id, S>>>,
}

impl<Id: Clone + Send + 'static, S: Clone + Send + 'static> Default for StateChangeEmitter<Id, S> {
    fn default() -> Self {
        StateChangeEmitterBuilder::default().build()
    }
}

impl<Id: Clone + Send + 'static, S: Clone + Send + 'static> StateChangeEmitter<Id, S> {
    /// Emit a state change event to registered hooks.
    ///
    /// This method calls each hook synchronously. Hooks that need to perform
    /// async work should queue the event internally.
    pub fn emit(&self, event: StateChangeEvent<'_, Id, S>) {
        for hook in self.hooks.iter() {
            hook.on_state_changed(&event);
        }
    }
}

/// Builder for creating a [`StateChangeEmitter`] with registered hooks.
pub struct StateChangeEmitterBuilder<Id: Clone + Send + 'static, S: Clone + Send + 'static> {
    hooks: Vec<Box<dyn StateChangeHook<Id, S>>>,
}

impl<Id: Clone + Send + 'static, S: Clone + Send + 'static> Default
    for StateChangeEmitterBuilder<Id, S>
{
    fn default() -> Self {
        Self { hooks: Vec::new() }
    }
}

impl<Id: Clone + Send + 'static, S: Clone + Send + 'static> StateChangeEmitterBuilder<Id, S> {
    /// Register a hook to be called on state changes.
    pub fn hook(mut self, hook: Box<dyn StateChangeHook<Id, S>>) -> Self {
        self.hooks.push(hook);
        self
    }

    /// Build the emitter with the registered hooks.
    pub fn build(self) -> StateChangeEmitter<Id, S> {
        StateChangeEmitter { hooks: self.hooks }
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;
    use std::sync::atomic::{AtomicUsize, Ordering};

    use super::*;

    #[derive(Debug, Clone, PartialEq)]
    enum TestState {
        A,
    }

    struct CountingHook(Arc<AtomicUsize>);

    impl CountingHook {
        fn new() -> (Self, Arc<AtomicUsize>) {
            let counter = Arc::new(AtomicUsize::new(0));
            (Self(counter.clone()), counter)
        }
    }

    impl StateChangeHook<String, TestState> for CountingHook {
        fn on_state_changed(&self, _event: &StateChangeEvent<'_, String, TestState>) {
            self.0.fetch_add(1, Ordering::SeqCst);
        }
    }

    fn emit_event(emitter: &StateChangeEmitter<String, TestState>) {
        let id = "test".to_string();
        let state = TestState::A;
        emitter.emit(StateChangeEvent {
            object_id: &id,
            previous_state: None,
            new_state: &state,
            timestamp: Utc::now(),
        });
    }

    #[test]
    fn test_zero_hooks() {
        let emitter = StateChangeEmitterBuilder::default().build();
        emit_event(&emitter);
    }

    #[test]
    fn test_one_hook() {
        let (hook, counter) = CountingHook::new();
        let emitter = StateChangeEmitterBuilder::default()
            .hook(Box::new(hook))
            .build();

        emit_event(&emitter);

        assert_eq!(counter.load(Ordering::SeqCst), 1);
    }

    #[test]
    fn test_multiple_hooks() {
        let (hook1, counter1) = CountingHook::new();
        let (hook2, counter2) = CountingHook::new();
        let emitter = StateChangeEmitterBuilder::default()
            .hook(Box::new(hook1))
            .hook(Box::new(hook2))
            .build();

        emit_event(&emitter);

        assert_eq!(counter1.load(Ordering::SeqCst), 1);
        assert_eq!(counter2.load(Ordering::SeqCst), 1);
    }
}
