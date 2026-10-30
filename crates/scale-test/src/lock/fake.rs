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

//! A scriptable [`LockPort`] for tests: refuse, or lose the lock at a set time.

use std::sync::{Arc, Mutex, MutexGuard};
use std::time::Duration;

use async_trait::async_trait;
use tokio::time::Instant;

use super::{
    HeldLock, HolderRecord, HolderState, HolderView, LockError, LockPort, Refusal, WriteGate, plain,
};

/// What the fake saw, in order.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FakeLockEvent {
    Acquired(HolderRecord),
    SetState(HolderState),
    Released(HolderState),
}

#[derive(Default)]
struct State {
    refusal: Option<Refusal>,
    release_error: Option<LockError>,
    lose_at: Option<Instant>,
    current: Option<HolderView>,
    events: Vec<FakeLockEvent>,
    gates: Vec<WriteGate>,
}

#[derive(Clone, Default)]
pub struct FakeLock {
    state: Arc<Mutex<State>>,
}

fn lock(state: &Mutex<State>) -> MutexGuard<'_, State> {
    state.lock().unwrap_or_else(|e| e.into_inner())
}

impl FakeLock {
    pub fn new() -> Self {
        Self::default()
    }

    /// Every later acquire fails with `refusal`.
    pub fn refuse(&self, refusal: Refusal) {
        lock(&self.state).refusal = Some(refusal);
    }

    /// Held and later locks lose their write gate at `at`.
    pub fn lose_at(&self, at: Instant) {
        let mut state = lock(&self.state);
        state.lose_at = Some(at);
        for gate in &state.gates {
            gate.close_at(at);
        }
    }

    pub fn lose_now(&self) {
        self.lose_at(Instant::now());
    }

    /// The next release fails with `error` and writes nothing.
    pub fn fail_release(&self, error: LockError) {
        lock(&self.state).release_error = Some(error);
    }

    /// What `read` returns, e.g. a crashed run's holder.
    pub fn set_current(&self, view: Option<HolderView>) {
        lock(&self.state).current = view;
    }

    pub fn events(&self) -> Vec<FakeLockEvent> {
        lock(&self.state).events.clone()
    }

    /// The state of the last `set_state` or `release`, if any.
    pub fn last_state(&self) -> Option<HolderState> {
        lock(&self.state).events.iter().rev().find_map(|e| match e {
            FakeLockEvent::SetState(s) | FakeLockEvent::Released(s) => Some(*s),
            FakeLockEvent::Acquired(_) => None,
        })
    }
}

#[async_trait]
impl LockPort for FakeLock {
    async fn acquire(&self, me: HolderRecord) -> Result<Arc<dyn HeldLock>, LockError> {
        // As on the Lease: a record it cannot carry is refused first.
        if !plain(me.run_id.as_str()) || !plain(&me.execution_id) {
            return Err(LockError::Corrupt(
                "the execution ID has characters a Lease cannot carry".to_string(),
            ));
        }
        let mut state = lock(&self.state);
        if let Some(refusal) = &state.refusal {
            return Err(LockError::Refused(refusal.clone()));
        }
        let gate = WriteGate::open();
        if let Some(at) = state.lose_at {
            gate.close_at(at);
        }
        state.gates.push(gate.clone());
        state.events.push(FakeLockEvent::Acquired(me.clone()));
        state.current = Some(HolderView {
            record: HolderRecord {
                state: HolderState::Running,
                ..me
            },
            live: true,
            renew_age: Some(Duration::ZERO),
        });
        Ok(Arc::new(FakeHeld {
            state: self.state.clone(),
            gate,
        }))
    }

    async fn read(&self) -> Result<Option<HolderView>, LockError> {
        Ok(lock(&self.state).current.clone())
    }
}

struct FakeHeld {
    state: Arc<Mutex<State>>,
    gate: WriteGate,
}

/// As in production: dropping the last handle closes its gate.
impl Drop for FakeHeld {
    fn drop(&mut self) {
        self.gate.close();
    }
}

#[async_trait]
impl HeldLock for FakeHeld {
    fn gate(&self) -> WriteGate {
        self.gate.clone()
    }

    async fn set_state(&self, new: HolderState) -> Result<(), LockError> {
        // A lost lock never writes the Lease again.
        self.gate.check().map_err(|_| LockError::Lost)?;
        let mut state = lock(&self.state);
        if let Some(refused) = needs_cleanup(&state, new) {
            return Err(refused);
        }
        state.events.push(FakeLockEvent::SetState(new));
        if let Some(current) = state.current.as_mut() {
            current.record.state = new;
        }
        Ok(())
    }

    async fn release(&self, new: HolderState) -> Result<(), LockError> {
        self.gate.check().map_err(|_| LockError::Lost)?;
        self.gate.close();
        let mut state = lock(&self.state);
        if let Some(error) = state.release_error.take() {
            return Err(error);
        }
        if let Some(refused) = needs_cleanup(&state, new) {
            if let Some(current) = state.current.as_mut() {
                current.live = false;
            }
            return Err(refused);
        }
        state.events.push(FakeLockEvent::Released(new));
        if new == HolderState::Final {
            state.current = None;
        } else if let Some(current) = state.current.as_mut() {
            current.record.state = new;
            current.live = false;
        }
        Ok(())
    }
}

/// As on the Lease: a recorded needsCleanup stays for an operator, and any
/// other state is refused.
fn needs_cleanup(state: &State, new: HolderState) -> Option<LockError> {
    let current = state.current.as_ref()?;
    (current.record.state == HolderState::NeedsCleanup && new != HolderState::NeedsCleanup)
        .then(|| LockError::Refused(Refusal::NeedsCleanup(current.record.clone())))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::RunId;
    use crate::lock::HolderKind;

    fn me() -> HolderRecord {
        HolderRecord {
            kind: HolderKind::Test,
            run_id: RunId::parse("r1").unwrap(),
            execution_id: "r1-x".to_string(),
            state: HolderState::Running,
            test: Some("burst".to_string()),
        }
    }

    #[tokio::test(start_paused = true)]
    async fn fake_lock_scripts_loss_and_refusal() {
        let fake = FakeLock::new();
        let held = fake.acquire(me()).await.unwrap();
        assert!(held.gate().check().is_ok());
        assert_eq!(
            fake.read().await.unwrap().unwrap().record.run_id.as_str(),
            "r1"
        );

        fake.lose_at(Instant::now() + Duration::from_secs(5));
        held.set_state(HolderState::NeedsCleanup).await.unwrap();
        tokio::time::advance(Duration::from_secs(5)).await;
        assert!(held.gate().check().is_err());
        assert_eq!(held.release(HolderState::Final).await, Err(LockError::Lost));
        assert_eq!(fake.last_state(), Some(HolderState::NeedsCleanup));

        fake.refuse(Refusal::Foreign("someone".to_string()));
        assert!(matches!(
            fake.acquire(me()).await.err(),
            Some(LockError::Refused(Refusal::Foreign(_)))
        ));
    }

    #[tokio::test]
    async fn the_fake_refuses_a_holder_the_lease_cannot_carry() {
        let fake = FakeLock::new();
        let mut bad = me();
        bad.execution_id = "r1:x".to_string();
        assert!(matches!(
            fake.acquire(bad).await.err(),
            Some(LockError::Corrupt(_))
        ));
        assert!(fake.events().is_empty());
        assert_eq!(fake.read().await.unwrap(), None);
    }

    #[tokio::test]
    async fn dropping_the_fake_handle_closes_its_gate() {
        let fake = FakeLock::new();
        let held = fake.acquire(me()).await.unwrap();
        let gate = held.gate();
        drop(held);
        assert!(gate.check().is_err());
    }

    #[tokio::test]
    async fn a_recorded_needs_cleanup_stays_in_the_fake() {
        let fake = FakeLock::new();
        let held = fake.acquire(me()).await.unwrap();
        held.set_state(HolderState::NeedsCleanup).await.unwrap();
        let refused = |result: Result<(), LockError>| {
            matches!(result, Err(LockError::Refused(Refusal::NeedsCleanup(_))))
        };
        assert!(refused(held.set_state(HolderState::Final).await));
        assert!(refused(held.release(HolderState::Final).await));
        assert!(held.gate().check().is_err());
        let view = fake.read().await.unwrap().expect("the holder stays");
        assert_eq!(view.record.state, HolderState::NeedsCleanup);
        assert!(!view.live);
    }

    #[tokio::test]
    async fn final_release_frees_the_fake() {
        let fake = FakeLock::new();
        let held = fake.acquire(me()).await.unwrap();
        held.release(HolderState::Final).await.unwrap();
        assert_eq!(fake.read().await.unwrap(), None);
        assert!(
            held.gate().check().is_err(),
            "a released lock writes no more"
        );
    }
}
