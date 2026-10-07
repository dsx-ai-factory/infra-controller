// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

//! Server-sent-event delivery for the mock's `EventService`: one connection's
//! cursor over the bounded replay history, comment heartbeats, and the raw
//! fault scripts a test can queue in place of a live stream. The registry these
//! subscribers read from is `crate::redfish::event_service::EventServiceState`.

use std::io;
use std::sync::Arc;
use std::time::Duration;

use bytes::Bytes;
use serde::{Deserialize, Serialize};
use tokio::sync::watch;
use tokio::time::Instant;

use crate::redfish::event_service::{EventServiceState, LiveFrame, ScriptStep};

/// One raw fault-stream operation. Claimed scripts run once per connection,
/// bypass normal event history, and are cancelled when their response is dropped.
#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum StreamStep {
    /// Send exact bytes, without SSE validation; JSON encodes bytes as integers.
    Bytes {
        /// Raw response bytes.
        data: Vec<u8>,
    },
    /// Wait this many milliseconds; a whole script may delay at most 60 seconds.
    Delay {
        /// Delay in milliseconds.
        millis: u64,
    },
    /// End the response cleanly. Must be the last step.
    Eof,
    /// End the response with an I/O error. Must be the last step.
    Error,
}

/// Fixed at subscription: either a claimed raw script or live replay with heartbeats.
pub(crate) enum Delivery {
    Live {
        next_seq: u64,
        heartbeat_at: Option<Instant>,
    },
    Script {
        delay_until: Option<Instant>,
    },
}

pub(crate) struct Subscriber {
    state: Arc<EventServiceState>,
    id: u64,
    delivery: Delivery,
    changed: watch::Receiver<()>,
}

impl Drop for Subscriber {
    fn drop(&mut self) {
        self.state.unsubscribe_on_drop(self.id);
    }
}

impl Subscriber {
    pub(crate) fn new(
        state: Arc<EventServiceState>,
        id: u64,
        delivery: Delivery,
        changed: watch::Receiver<()>,
    ) -> Self {
        Self {
            state,
            id,
            delivery,
            changed,
        }
    }

    /// Next body chunk: a frame, a heartbeat, or script bytes. `None` ends the
    /// body cleanly; `Err` ends it with a body error.
    pub(crate) async fn next(&mut self) -> Option<Result<Bytes, io::Error>> {
        loop {
            let wake_at = match &mut self.delivery {
                Delivery::Script { delay_until } => match *delay_until {
                    Some(at) if Instant::now() < at => {
                        if !self.state.is_subscribed(self.id) {
                            return None;
                        }
                        Some(at)
                    }
                    Some(_) => {
                        *delay_until = None;
                        continue;
                    }
                    None => match self.state.pop_script_step(self.id) {
                        ScriptStep::Closed
                        | ScriptStep::Finished
                        | ScriptStep::Step(StreamStep::Eof) => return None,
                        ScriptStep::Step(StreamStep::Error) => {
                            return Some(Err(io::Error::other("injected SSE body error")));
                        }
                        ScriptStep::Step(StreamStep::Bytes { data }) => {
                            return Some(Ok(Bytes::from(data)));
                        }
                        ScriptStep::Step(StreamStep::Delay { millis }) => {
                            *delay_until = Some(Instant::now() + Duration::from_millis(millis));
                            continue;
                        }
                    },
                },
                Delivery::Live {
                    next_seq,
                    heartbeat_at,
                } => match self.state.next_frame(self.id, *next_seq) {
                    LiveFrame::Closed => return None,
                    LiveFrame::Lagged => {
                        return Some(Err(io::Error::other(
                            "SSE subscriber exceeded replay retention",
                        )));
                    }
                    LiveFrame::Frame(bytes) => {
                        *next_seq += 1;
                        *heartbeat_at = None;
                        return Some(Ok(bytes));
                    }
                    LiveFrame::Pending => {
                        if heartbeat_at.is_none() {
                            *heartbeat_at = self
                                .state
                                .config
                                .limits
                                .heartbeat
                                .map(|interval| Instant::now() + interval);
                        }
                        *heartbeat_at
                    }
                },
            };
            let timer = async {
                match wake_at {
                    Some(at) => tokio::time::sleep_until(at).await,
                    None => std::future::pending().await,
                }
            };
            tokio::select! {
                _ = self.changed.changed() => {}
                _ = timer => match &mut self.delivery {
                    Delivery::Live { heartbeat_at, .. } => {
                        *heartbeat_at = None;
                        if !self.state.is_subscribed(self.id) {
                            return None;
                        }
                        return Some(Ok(Bytes::from_static(b": heartbeat\n\n")));
                    }
                    Delivery::Script { delay_until } => *delay_until = None,
                },
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use futures::FutureExt;
    use tokio::time::Instant;

    use super::StreamStep;
    use crate::redfish::event_service::fixtures::{event, metric, state};
    use crate::redfish::event_service::{EventServiceConfig, EventServiceError, EventServiceState};

    #[tokio::test]
    async fn replay_fanout_live_handoff_and_lag() {
        let state = state(2);
        let first = state.publish(event()).unwrap();
        let second = state.publish(metric()).unwrap();
        let mut replay = state.subscribe(Some(&first)).unwrap();
        let mut live = state.subscribe(None).unwrap();
        assert!(matches!(
            state.subscribe(None),
            Err(EventServiceError::Unavailable)
        ));
        assert!(
            String::from_utf8(replay.next().await.unwrap().unwrap().to_vec())
                .unwrap()
                .starts_with(&format!("id: {second}\n"))
        );
        assert!(
            live.next().now_or_never().is_none(),
            "no unsolicited replay"
        );
        let third = state.publish(event()).unwrap();
        let bytes = replay.next().await.unwrap().unwrap();
        assert_eq!(bytes, live.next().await.unwrap().unwrap());
        assert!(bytes.starts_with(format!("id: {third}\n").as_bytes()));
        drop(live);
        drop(replay);
        // History now holds the second and third frames. The first was just
        // evicted, so resuming after it is still lossless.
        let mut edge = state.subscribe(Some(&first)).unwrap();
        assert!(
            edge.next()
                .await
                .unwrap()
                .unwrap()
                .starts_with(format!("id: {second}\n").as_bytes())
        );
        drop(edge);
        carbide_test_support::value_scenarios!(run = |id: String|
            matches!(state.subscribe(Some(&id)), Err(EventServiceError::Invalid(_)));
            "unavailable or noncanonical replay cursors" {
                format!("{}:0", state.stats().generation) => true,
                format!("{}:{}", state.stats().generation, u64::MAX) => true,
                "nonsense".into() => true,
                format!("{}:999", state.stats().generation) => true,
                format!("{}:02", state.stats().generation) => true,
                self::state(1).publish(event()).unwrap() => true,
            }
        );
        let mut slow = state.subscribe(None).unwrap();
        for _ in 0..3 {
            state.publish(event()).unwrap();
        }
        assert!(slow.next().await.unwrap().is_err());
        assert_eq!(state.stats().lagged, 1);
        drop(slow);
        assert_eq!(state.stats().subscribers, 0);
    }

    #[tokio::test(start_paused = true)]
    async fn heartbeat_scripts_and_close_interrupt_delays() {
        let state = EventServiceState::new(EventServiceConfig::default());
        let mut live = state.subscribe(None).unwrap();
        let before = Instant::now();
        assert_eq!(live.next().await.unwrap().unwrap(), ": heartbeat\n\n");
        assert_eq!(before.elapsed(), Duration::from_secs(15));
        drop(live);
        state
            .queue_script(vec![
                StreamStep::Bytes {
                    data: b"data: {".to_vec(),
                },
                StreamStep::Delay { millis: 100 },
                StreamStep::Bytes {
                    data: b"}\n\n".to_vec(),
                },
                StreamStep::Error,
            ])
            .unwrap();
        let id = state.publish(event()).unwrap();
        // Replay never claims a script.
        let replay = state.subscribe(Some(&id)).unwrap();
        assert_eq!(state.stats().queued_scripts, 1);
        drop(replay);
        let mut script = state.subscribe(None).unwrap();
        assert_eq!(state.stats().queued_script_bytes, 0);
        assert_eq!(script.next().await.unwrap().unwrap(), "data: {");
        let before = Instant::now();
        {
            // A publication during a scripted delay must not shorten the delay.
            let mut delayed = std::pin::pin!(script.next());
            assert!(delayed.as_mut().now_or_never().is_none());
            state.publish(event()).unwrap();
            assert_eq!(delayed.await.unwrap().unwrap(), "}\n\n");
        }
        assert_eq!(before.elapsed(), Duration::from_millis(100));
        assert!(script.next().await.unwrap().is_err());
        drop(script);
        state
            .queue_script(vec![StreamStep::Delay { millis: 60_000 }, StreamStep::Eof])
            .unwrap();
        let mut script = state.subscribe(None).unwrap();
        assert!(script.next().now_or_never().is_none());
        state.close_subscribers();
        assert!(script.next().await.is_none());
        assert_eq!(state.stats().retained_frames, 2);
    }

    #[tokio::test]
    async fn reset_wakes_existing_stream_and_invalidates_history() {
        let state = EventServiceState::new(EventServiceConfig::default());
        let id = state.publish(event()).unwrap();
        let mut live = state.subscribe(None).unwrap();
        state.queue_script(vec![StreamStep::Eof]).unwrap();
        assert!(live.next().now_or_never().is_none());
        state.reset();
        assert!(
            tokio::time::timeout(Duration::from_secs(1), live.next())
                .await
                .unwrap()
                .is_none()
        );
        let stats = state.stats();
        assert_eq!(
            (
                stats.subscribers,
                stats.retained_frames,
                stats.queued_scripts,
                stats.closed
            ),
            (0, 0, 0, 1)
        );
        assert!(!id.starts_with(&stats.generation));
        assert!(state.subscribe(Some(&id)).is_err());
        assert!(state.publish(event()).unwrap().ends_with(":1"));
    }
}
