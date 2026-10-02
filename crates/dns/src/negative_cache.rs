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
//! TTL-expiring, LRU-bounded cache of negative DNS responses.
//!
//! Caching negatives keeps repeated lookups for the same name from re-querying
//! the api server. Two classes of negative are cached with different lifetimes:
//!
//! * *stable* negatives — answers that will not change on retry. An
//!   authoritative one (NODATA, NXDomain) carries the zone SOA and is cached
//!   for the zone's own negative TTL, bounded by `ttl`. A non-authoritative
//!   one (Refused, NotImp, FormErr) carries no SOA and is cached for `ttl`
//!   itself; and
//! * *transient* failures (ServFail) — a momentary upstream error, cached only
//!   for the short `transient_ttl` so a client retry storm collapses into one
//!   upstream call per name per window without outliving the api server's
//!   recovery (RFC 2308 §7.1, RFC 9520).
//!
//! Two mechanisms keep memory bounded:
//!
//! * an LRU capacity bound (`max_entries`): inserting into a full cache evicts
//!   the *least-recently-used* entry rather than refusing the newcomer, so a
//!   flood of distinct names keeps the hot working set cached instead of
//!   pinning whichever names happened to arrive first; and
//! * a periodic sweep ([`NegativeCache::evict_expired`]) that drops entries past
//!   their TTL, reclaiming capacity that expired-but-not-yet-re-queried entries
//!   would otherwise hold.

use std::num::NonZeroUsize;
use std::sync::Mutex;
use std::time::{Duration, Instant};

use hickory_resolver::proto::op::ResponseCode;
use hickory_server::proto::rr::{RData, Record, RecordType};
use lru::LruCache;

/// Identifies a cached negative response: the queried name and record type.
#[derive(Hash, Debug, Eq, PartialEq, Clone)]
pub(crate) struct CacheKey {
    pub qname: String,
    pub qtype: RecordType,
}

/// A negative response in full, so that replaying it from the cache is
/// indistinguishable from the answer the upstream originally gave.
#[derive(Debug, Clone)]
pub(crate) struct NegativeAnswer {
    /// The DNS response code returned to the client. NODATA and NXDOMAIN
    /// differ here (NoError vs NXDomain) even though both are negatives.
    pub code: ResponseCode,
    /// Whether the AA bit is set.
    pub authoritative: bool,
    /// The zone SOA for the authority section, when the upstream sent one.
    pub authority: Option<Record>,
}

#[derive(Debug)]
struct NegativeEntry {
    answer: NegativeAnswer,
    expires_at: Instant,
}

/// What [`NegativeCache::record`] stored, and whether admitting it pushed
/// another entry out.
#[derive(Debug)]
pub(crate) struct Recorded {
    /// The answer as cached, which is also the answer to send.
    pub answer: NegativeAnswer,
    /// A different key was evicted to make room.
    pub evicted: bool,
}

/// The negative-caching lifetime an authority SOA imposes: `min(SOA TTL,
/// SOA MINIMUM)` per RFC 2308 §3. `None` when the record is not an SOA.
fn soa_negative_ttl(record: &Record) -> Option<Duration> {
    let RData::SOA(soa) = &record.data else {
        return None;
    };
    Some(Duration::from_secs(u64::from(record.ttl.min(soa.minimum))))
}

#[derive(Debug)]
pub(crate) struct NegativeCache {
    // A plain `std::sync::Mutex`, not an async one: every critical section is a
    // handful of synchronous map operations and the guard is never held across
    // an `.await`. (`LruCache::get` bumps recency, so even the read path needs
    // `&mut`, ruling out an `RwLock`.) Using the sync mutex also lets the
    // observable metrics gauge read the length from a synchronous callback.
    entries: Mutex<LruCache<CacheKey, NegativeEntry>>,
    /// Ceiling for stable negatives. Used as-is when the answer has no SOA
    /// (Refused, NotImp, FormErr); an authority SOA can only shorten it.
    ttl: Duration,
    /// Lifetime for transient failures (ServFail); kept short on purpose.
    transient_ttl: Duration,
}

impl NegativeCache {
    pub(crate) fn new(ttl: Duration, transient_ttl: Duration, max_entries: usize) -> Self {
        // A zero capacity would evict every entry the instant it was inserted,
        // defeating the cache, and `NonZeroUsize::new(0)` is `None` — floor it
        // at 1 rather than panic on a misconfigured bound.
        let capacity = NonZeroUsize::new(max_entries).unwrap_or(NonZeroUsize::MIN);
        Self {
            entries: Mutex::new(LruCache::new(capacity)),
            ttl,
            transient_ttl,
        }
    }

    /// The lifetime for `answer` given whether the negative is `transient`.
    /// A transient failure is held only for the short `transient_ttl` so it
    /// does not outlive the upstream's recovery. The caller classifies
    /// transience (see the DNS handler), since it cannot be derived from the
    /// response code alone. A stable negative lives for the zone's own negative
    /// TTL when it carries an SOA (NODATA, NXDomain), and for the configured
    /// `ttl` when it does not (Refused, NotImp, FormErr); either way `ttl` is
    /// the ceiling.
    // TODO: RFC 9520 RECOMMENDS exponential/linear backoff that lengthens the
    // cached-failure lifetime for *persistent* failures.
    fn ttl_for(&self, answer: &NegativeAnswer, transient: bool) -> Duration {
        if transient {
            return self.transient_ttl;
        }
        match answer.authority.as_ref().and_then(soa_negative_ttl) {
            Some(zone_ttl) => self.ttl.min(zone_ttl),
            None => self.ttl,
        }
    }

    /// The number of cache entries currently held, including any that have expired but
    /// not yet been swept. Backs the cache-occupancy metrics gauge.
    pub(crate) fn entry_count(&self) -> usize {
        self.entries
            .lock()
            .expect("negative cache mutex poisoned")
            .len()
    }

    /// Returns the cached negative answer for `key` if a non-expired entry
    /// exists, marking it most-recently-used.
    ///
    /// An entry that has expired but has not yet been swept is treated as absent
    /// and dropped on the spot, so a stale negative is never served and the
    /// expired entry stops counting against the capacity bound.
    ///
    /// The replayed SOA's TTL is the entry's remaining lifetime, not the
    /// original value (RFC 2308 §5). Otherwise each hit would hand a resolver a
    /// fresh full-length negative, and the negative could outlive this cache by
    /// another whole TTL.
    pub(crate) fn get(&self, key: &CacheKey) -> Option<NegativeAnswer> {
        let now = Instant::now();
        let mut entries = self.entries.lock().expect("negative cache mutex poisoned");
        let answer = entries
            .get(key)
            .filter(|entry| entry.expires_at > now)
            .map(|entry| {
                let mut answer = entry.answer.clone();
                if let Some(record) = &mut answer.authority {
                    let remaining = entry.expires_at.duration_since(now).as_secs();
                    record.ttl = record.ttl.min(u32::try_from(remaining).unwrap_or(u32::MAX));
                }
                answer
            });
        // Either the key was absent (no-op) or it was present but expired; in
        // the latter case remove it so it neither serves nor occupies a slot.
        if answer.is_none() {
            entries.pop(key);
        }
        answer
    }

    /// Records the negative `answer` for `key` and returns what was stored.
    /// `transient` selects the entry's lifetime (see [`Self::ttl_for`]).
    ///
    /// The stored copy has its SOA TTL clamped to that lifetime, and the caller
    /// sends the returned answer rather than the upstream original, so a fresh
    /// answer and every later replay advertise the same deadline to resolvers.
    ///
    /// The cache always admits the entry: inserting into a full cache evicts the
    /// least-recently-used entry. `evicted` is `true` when such a capacity
    /// eviction occurred (a *different* key was pushed out to make room), and
    /// `false` when the entry fit or merely refreshed an existing key.
    pub(crate) fn record(
        &self,
        key: CacheKey,
        mut answer: NegativeAnswer,
        transient: bool,
    ) -> Recorded {
        let lifetime = self.ttl_for(&answer, transient);
        if let Some(record) = &mut answer.authority {
            let lifetime_secs = u32::try_from(lifetime.as_secs()).unwrap_or(u32::MAX);
            record.ttl = record.ttl.min(lifetime_secs);
        }
        let entry = NegativeEntry {
            expires_at: Instant::now() + lifetime,
            answer: answer.clone(),
        };
        let mut entries = self.entries.lock().expect("negative cache mutex poisoned");
        // `push` returns the displaced (key, value): the same key on an in-place
        // refresh, or a *different* key when a full cache evicted its LRU entry
        // to admit this one.
        let evicted = match entries.push(key.clone(), entry) {
            Some((displaced_key, _)) => displaced_key != key,
            None => false,
        };
        Recorded { answer, evicted }
    }

    /// Removes expired entries and returns the number evicted.
    pub(crate) fn evict_expired(&self) -> usize {
        let now = Instant::now();
        let mut entries = self.entries.lock().expect("negative cache mutex poisoned");
        let expired: Vec<CacheKey> = entries
            .iter()
            .filter(|(_, entry)| entry.expires_at <= now)
            .map(|(key, _)| key.clone())
            .collect();
        for key in &expired {
            entries.pop(key);
        }
        expired.len()
    }
}

#[cfg(test)]
mod tests {
    use hickory_server::proto::rr::Name;
    use hickory_server::proto::rr::rdata::SOA;

    use super::*;

    fn key(qname: &str) -> CacheKey {
        CacheKey {
            qname: qname.to_string(),
            qtype: RecordType::A,
        }
    }

    /// An SOA record for the authority section, with the record TTL and the
    /// MINIMUM field set independently so tests can tell which one bounds the
    /// negative lifetime.
    fn soa(record_ttl: u32, minimum: u32) -> Record {
        let apex = Name::from_ascii("example.com.").expect("literal name parses");
        let soa = SOA::new(
            apex.clone(),
            Name::from_ascii("hostmaster.example.com.").expect("literal name parses"),
            7,
            3600,
            900,
            1_209_600,
            minimum,
        );
        Record::from_rdata(apex, record_ttl, RData::SOA(soa))
    }

    fn nxdomain() -> NegativeAnswer {
        NegativeAnswer {
            code: ResponseCode::NXDomain,
            authoritative: true,
            authority: Some(soa(300, 300)),
        }
    }

    fn nodata() -> NegativeAnswer {
        NegativeAnswer {
            code: ResponseCode::NoError,
            authoritative: true,
            authority: Some(soa(300, 300)),
        }
    }

    fn servfail() -> NegativeAnswer {
        NegativeAnswer {
            code: ResponseCode::ServFail,
            authoritative: false,
            authority: None,
        }
    }

    fn refused() -> NegativeAnswer {
        NegativeAnswer {
            code: ResponseCode::Refused,
            authoritative: false,
            authority: None,
        }
    }

    #[test]
    fn evicts_least_recently_used_when_full() {
        let cache = NegativeCache::new(Duration::from_secs(120), Duration::from_secs(5), 2);
        cache.record(key("a.example.com."), nxdomain(), false);
        cache.record(key("b.example.com."), nxdomain(), false);

        // Touch `a`, making `b` the least-recently-used entry.
        assert!(cache.get(&key("a.example.com.")).is_some());

        // Admitting `c` reports an eviction and pushes out `b`, not `a`.
        let evicted = cache
            .record(key("c.example.com."), nxdomain(), false)
            .evicted;
        assert!(evicted);
        assert_eq!(cache.entry_count(), 2);
        assert!(cache.get(&key("b.example.com.")).is_none());
        assert!(cache.get(&key("a.example.com.")).is_some());
        assert!(cache.get(&key("c.example.com.")).is_some());
    }

    #[test]
    fn refreshes_existing_key_without_eviction_when_full() {
        let cache = NegativeCache::new(Duration::from_secs(120), Duration::from_secs(5), 2);
        cache.record(key("a.example.com."), nxdomain(), false);
        cache.record(key("b.example.com."), nxdomain(), false);

        let evicted = cache
            .record(key("a.example.com."), nxdomain(), false)
            .evicted;
        assert!(!evicted);
        assert_eq!(cache.entry_count(), 2);
    }

    #[test]
    fn get_returns_none_for_expired_entry() {
        // A zero TTL means every entry is already expired when read back.
        let cache = NegativeCache::new(Duration::from_secs(0), Duration::from_secs(0), 16);
        cache.record(key("gone.example.com."), nxdomain(), false);
        assert!(cache.get(&key("gone.example.com.")).is_none());
    }

    #[test]
    fn evict_expired_drops_only_expired_entries() {
        let cache = NegativeCache::new(Duration::from_secs(0), Duration::from_secs(0), 16);
        cache.record(key("a.example.com."), nxdomain(), false);
        cache.record(key("b.example.com."), refused(), false);

        assert_eq!(cache.evict_expired(), 2);
        assert_eq!(cache.entry_count(), 0);
    }

    #[test]
    fn servfail_uses_transient_ttl_not_authoritative_ttl() {
        let cache = NegativeCache::new(Duration::from_secs(120), Duration::from_secs(0), 16);

        cache.record(key("fail.example.com."), servfail(), true);
        assert!(cache.get(&key("fail.example.com.")).is_none());

        cache.record(key("gone.example.com."), nxdomain(), false);
        assert!(cache.get(&key("gone.example.com.")).is_some());
    }

    /// The replayed authority record carries the recorded SOA data at the
    /// recorded owner name, with a TTL no longer than the entry's lifetime.
    /// `Record`'s own equality ignores TTL (RFC 2136), so the fields are
    /// checked one by one rather than through `==`.
    fn assert_replayed_soa(hit: &NegativeAnswer, recorded: &Record, max_ttl: u32) {
        let replayed = hit.authority.as_ref().expect("SOA replayed");
        assert_eq!(replayed.name, recorded.name);
        assert_eq!(replayed.data, recorded.data);
        assert!(
            replayed.ttl <= max_ttl,
            "replayed SOA TTL {} exceeds the entry lifetime of {max_ttl}s",
            replayed.ttl
        );
        assert!(replayed.ttl > 0, "a live entry has time left");
    }

    /// A hit has to reproduce the whole answer, not just its RCODE: without the
    /// AA bit and the authority SOA a resolver cannot cache the negative
    /// itself, and a NODATA that came back as NXDOMAIN would deny a name that
    /// exists.
    #[test]
    fn hit_replays_the_recorded_answer() {
        let cache = NegativeCache::new(Duration::from_secs(120), Duration::from_secs(5), 16);
        cache.record(key("empty.example.com."), nodata(), false);
        cache.record(key("gone.example.com."), nxdomain(), false);

        let hit = cache
            .get(&key("empty.example.com."))
            .expect("NODATA entry is cached");
        assert_eq!(hit.code, ResponseCode::NoError);
        assert!(hit.authoritative);
        assert_replayed_soa(&hit, &soa(300, 300), 120);

        let hit = cache
            .get(&key("gone.example.com."))
            .expect("NXDOMAIN entry is cached");
        assert_eq!(hit.code, ResponseCode::NXDomain);
        assert!(hit.authoritative);
        assert_replayed_soa(&hit, &soa(300, 300), 120);
    }

    /// The key is (qname, qtype) while the answer distinguishes NODATA from
    /// NXDOMAIN, so a wrong lookup would silently swap one for the other.
    #[test]
    fn nodata_and_nxdomain_do_not_cross_contaminate() {
        let cache = NegativeCache::new(Duration::from_secs(120), Duration::from_secs(5), 16);
        cache.record(key("empty.example.com."), nodata(), false);
        cache.record(key("gone.example.com."), nxdomain(), false);

        assert_eq!(
            cache
                .get(&key("empty.example.com."))
                .expect("NODATA entry is cached")
                .code,
            ResponseCode::NoError
        );
        assert_eq!(
            cache
                .get(&key("gone.example.com."))
                .expect("NXDOMAIN entry is cached")
                .code,
            ResponseCode::NXDomain
        );
    }

    /// A fresh miss and a later hit must advertise the same deadline. The
    /// returned answer's SOA TTL is the entry lifetime, not the upstream value.
    #[test]
    fn record_returns_the_answer_with_soa_ttl_clamped_to_the_lifetime() {
        let cache = NegativeCache::new(Duration::from_secs(60), Duration::from_secs(5), 16);

        let recorded = cache.record(key("gone.example.com."), nxdomain(), false);
        assert_replayed_soa(&recorded.answer, &soa(300, 300), 60);
        assert_eq!(
            recorded.answer.authority.as_ref().expect("SOA").ttl,
            60,
            "the fresh answer carries the full entry lifetime"
        );

        // An SOA already shorter than the ceiling is left alone.
        let short = NegativeAnswer {
            authority: Some(soa(30, 30)),
            ..nxdomain()
        };
        let recorded = cache.record(key("short.example.com."), short, false);
        assert_eq!(recorded.answer.authority.as_ref().expect("SOA").ttl, 30);
    }

    /// The SOA a resolver receives from a hit must not promise a longer
    /// negative lifetime than the entry itself has left.
    #[test]
    fn hit_ages_the_soa_ttl_to_the_remaining_lifetime() {
        let cache = NegativeCache::new(Duration::from_secs(60), Duration::from_secs(5), 16);
        cache.record(key("gone.example.com."), nxdomain(), false);

        let hit = cache
            .get(&key("gone.example.com."))
            .expect("NXDOMAIN entry is cached");
        assert_replayed_soa(&hit, &soa(300, 300), 60);
    }

    #[test]
    fn zone_soa_shortens_but_never_lengthens_the_negative_ttl() {
        let cache = NegativeCache::new(Duration::from_secs(120), Duration::from_secs(5), 16);

        // MINIMUM of zero expires the entry immediately even though the SOA
        // record's own TTL and the configured ceiling are both long.
        cache.record(
            key("minimum.example.com."),
            NegativeAnswer {
                authority: Some(soa(300, 0)),
                ..nxdomain()
            },
            false,
        );
        assert!(cache.get(&key("minimum.example.com.")).is_none());

        // ...and so does a zero record TTL with a long MINIMUM.
        cache.record(
            key("record-ttl.example.com."),
            NegativeAnswer {
                authority: Some(soa(0, 300)),
                ..nxdomain()
            },
            false,
        );
        assert!(cache.get(&key("record-ttl.example.com.")).is_none());

        // An SOA asking for longer than the configured ceiling does not get it.
        let capped = NegativeCache::new(Duration::from_secs(0), Duration::from_secs(5), 16);
        capped.record(
            key("long.example.com."),
            NegativeAnswer {
                authority: Some(soa(86_400, 86_400)),
                ..nxdomain()
            },
            false,
        );
        assert!(capped.get(&key("long.example.com.")).is_none());
    }
}
