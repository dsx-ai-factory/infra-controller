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
//! Sequential message processing with cancellable, scheduled messages.
//!
//! The owner runs and supervises the actor task. The mailbox is unbounded so
//! synchronous control paths can send without awaiting capacity; callers must
//! bound or coalesce externally generated work before enqueueing it.
use std::cmp::Reverse;
use std::collections::{BinaryHeap, HashMap, HashSet};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Instant;

use tokio::sync::mpsc;

/// Handler decision after processing one message.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ActorResult {
    /// Continue processing messages.
    Noop,
    /// Stop the actor and discard remaining messages and alarms.
    Stop,
}

/// Opaque identifier for one scheduled message in an actor's mailbox.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct AlarmId(u64);

/// Sending failed because the actor's message receiver has been dropped.
#[derive(Debug, thiserror::Error)]
#[error("actor mailbox is closed")]
pub struct ActorMailboxClosed;

#[derive(Debug)]
enum MailboxCommand<Message> {
    Send(Message),
    SendAt {
        alarm_id: AlarmId,
        deadline: Instant,
        message: Message,
    },
    Cancel(AlarmId),
}

/// Cloneable sender for an actor's immediate and scheduled messages.
#[derive(Debug)]
pub struct ActorMailbox<Message> {
    tx: mpsc::UnboundedSender<MailboxCommand<Message>>,
    next_alarm_id: Arc<AtomicU64>,
    cancelled_alarms: Arc<Mutex<HashSet<AlarmId>>>,
}

impl<Message> Clone for ActorMailbox<Message> {
    fn clone(&self) -> Self {
        Self {
            tx: self.tx.clone(),
            next_alarm_id: self.next_alarm_id.clone(),
            cancelled_alarms: self.cancelled_alarms.clone(),
        }
    }
}

impl<Message> ActorMailbox<Message> {
    /// Enqueues a message without waiting for its processing; fails if the actor has stopped.
    pub fn send(&self, message: Message) -> Result<(), ActorMailboxClosed> {
        self.tx
            .send(MailboxCommand::Send(message))
            .map_err(|_| ActorMailboxClosed)
    }

    /// Schedules a message for the deadline, or later if another message is still running.
    pub fn send_at(
        &self,
        deadline: Instant,
        message: Message,
    ) -> Result<AlarmId, ActorMailboxClosed> {
        // `fetch_add` wraps on overflow, but exhausting the u64 ID space during one actor's
        // lifetime is not realistic.
        let alarm_id = AlarmId(self.next_alarm_id.fetch_add(1, Ordering::Relaxed));
        self.tx
            .send(MailboxCommand::SendAt {
                alarm_id,
                deadline,
                message,
            })
            .map_err(|_| ActorMailboxClosed)?;
        Ok(alarm_id)
    }

    /// Cancels a scheduled message that has not begun processing.
    pub fn cancel(&self, alarm_id: AlarmId) {
        self.cancelled_alarms
            .lock()
            .expect("cancelled alarms lock must not be poisoned")
            .insert(alarm_id);
        self.tx.send(MailboxCommand::Cancel(alarm_id)).ok();
    }

    /// Cancels the previous alarm, if any, and schedules its replacement.
    pub fn replace_alarm(
        &self,
        alarm_id: Option<AlarmId>,
        deadline: Instant,
        message: Message,
    ) -> Result<AlarmId, ActorMailboxClosed> {
        if let Some(alarm_id) = alarm_id {
            self.cancel(alarm_id);
        }
        self.send_at(deadline, message)
    }
}

/// State-owned message handler; each invocation completes before the next begins.
pub trait ActorCallbacks<Message> {
    /// Processes one message and determines whether the actor continues running.
    fn message(
        &mut self,
        mailbox: &ActorMailbox<Message>,
        message: Message,
    ) -> impl Future<Output = ActorResult> + Send;
}

/// Sequential executor for a state object's messages and alarms.
pub struct Actor<Message> {
    mailbox: ActorMailbox<Message>,
    mailbox_rx: mpsc::UnboundedReceiver<MailboxCommand<Message>>,
    alarms: BinaryHeap<Reverse<(Instant, AlarmId)>>,
    alarm_messages: HashMap<AlarmId, Message>,
    cancelled_alarms: Arc<Mutex<HashSet<AlarmId>>>,
}

impl<Message> Actor<Message> {
    /// Creates an actor; the owner must run and supervise it.
    pub fn new() -> (Self, ActorMailbox<Message>) {
        let (tx, mailbox_rx) = mpsc::unbounded_channel();
        let cancelled_alarms = Arc::new(Mutex::new(HashSet::new()));
        let mailbox = ActorMailbox {
            tx,
            next_alarm_id: Arc::new(AtomicU64::new(0)),
            cancelled_alarms: cancelled_alarms.clone(),
        };
        (
            Self {
                mailbox: mailbox.clone(),
                mailbox_rx,
                alarms: BinaryHeap::new(),
                alarm_messages: HashMap::new(),
                cancelled_alarms,
            },
            mailbox,
        )
    }

    /// Processes messages until the handler returns `Stop` or the owner cancels this future.
    /// The actor retains a mailbox for self-messages, so dropping external mailboxes does not stop it.
    pub async fn run(mut self, mut state: impl ActorCallbacks<Message>) {
        loop {
            while self
                .alarms
                .peek()
                .is_some_and(|Reverse((_, alarm_id))| !self.alarm_messages.contains_key(alarm_id))
            {
                self.alarms.pop();
            }
            let next_deadline = self.alarms.peek().map(|Reverse((deadline, _))| *deadline);
            let input = if let Some(deadline) = next_deadline {
                tokio::select! {
                    _ = tokio::time::sleep_until(deadline.into()) => ActorInput::Alarm,
                    command = self.mailbox_rx.recv() => ActorInput::Command(command),
                }
            } else {
                ActorInput::Command(self.mailbox_rx.recv().await)
            };

            let message = match input {
                ActorInput::Alarm => {
                    let Some(Reverse((_, alarm_id))) = self.alarms.pop() else {
                        continue;
                    };
                    let Some(message) = self.alarm_messages.remove(&alarm_id) else {
                        continue;
                    };
                    if self
                        .cancelled_alarms
                        .lock()
                        .expect("cancelled alarms lock must not be poisoned")
                        .remove(&alarm_id)
                    {
                        continue;
                    }
                    message
                }
                ActorInput::Command(Some(MailboxCommand::Send(message))) => message,
                ActorInput::Command(Some(MailboxCommand::SendAt {
                    alarm_id,
                    deadline,
                    message,
                })) => {
                    self.alarms.push(Reverse((deadline, alarm_id)));
                    self.alarm_messages.insert(alarm_id, message);
                    continue;
                }
                ActorInput::Command(Some(MailboxCommand::Cancel(alarm_id))) => {
                    self.alarm_messages.remove(&alarm_id);
                    self.cancelled_alarms
                        .lock()
                        .expect("cancelled alarms lock must not be poisoned")
                        .remove(&alarm_id);
                    continue;
                }
                ActorInput::Command(None) => break,
            };

            if state.message(&self.mailbox, message).await == ActorResult::Stop {
                break;
            }
        }
    }
}

enum ActorInput<Message> {
    Alarm,
    Command(Option<MailboxCommand<Message>>),
}
