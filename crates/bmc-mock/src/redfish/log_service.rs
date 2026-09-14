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

use std::borrow::Cow;
use std::collections::VecDeque;
use std::sync::Mutex;

use serde_json::{Value, json};

use crate::json::{JsonExt, JsonPatch};
use crate::redfish::Builder;
use crate::{SystemPowerControl, redfish};

pub(super) fn manager_collection(manager_id: &str) -> redfish::Collection<'static> {
    let odata_id = format!("/redfish/v1/Managers/{manager_id}/LogServices");
    redfish::Collection {
        odata_id: Cow::Owned(odata_id),
        odata_type: Cow::Borrowed("#LogServiceCollection.LogServiceCollection"),
        name: Cow::Borrowed("Log Service Collection"),
    }
}

pub(super) fn system_collection(system_id: &str) -> redfish::Collection<'static> {
    let odata_id = format!("/redfish/v1/Systems/{system_id}/LogServices");
    redfish::Collection {
        odata_id: Cow::Owned(odata_id),
        odata_type: Cow::Borrowed("#LogServiceCollection.LogServiceCollection"),
        name: Cow::Borrowed("Log Service Collection"),
    }
}

pub(super) fn system_resource<'a>(system_id: &str, service_id: &'a str) -> redfish::Resource<'a> {
    let odata_id = format!("/redfish/v1/Systems/{system_id}/LogServices/{service_id}");
    redfish::Resource {
        odata_id: Cow::Owned(odata_id),
        odata_type: Cow::Borrowed("#LogService.v1_2_0.LogService"),
        name: Cow::Borrowed("Log Service"),
        id: Cow::Borrowed(service_id),
    }
}

pub(super) fn system_clear_log_target(system_id: &str, service_id: &str) -> String {
    format!(
        "{}/Actions/LogService.ClearLog",
        system_resource(system_id, service_id).odata_id
    )
}

pub(super) fn system_entries_collection<'a>(
    system_id: &str,
    service_id: &'a str,
) -> redfish::Collection<'a> {
    let odata_id = format!("/redfish/v1/Systems/{system_id}/LogServices/{service_id}/Entries");
    redfish::Collection {
        odata_id: Cow::Owned(odata_id),
        odata_type: Cow::Borrowed("#LogEntryCollection.LogEntryCollection"),
        name: Cow::Borrowed("Log Entries"),
    }
}

pub(super) fn builder(resource: &redfish::Resource<'_>) -> LogServiceBuilder {
    LogServiceBuilder {
        value: resource.json_patch(),
    }
}

pub(crate) fn event_entry(collection: &redfish::Collection<'_>, id: &str) -> EntryBuilder {
    let odata_id = format!("{}/{}", collection.odata_id, id);
    EntryBuilder {
        value: redfish::Resource {
            odata_id: Cow::Owned(odata_id),
            odata_type: Cow::Borrowed("#LogEntry.v1_15_0.LogEntry"),
            name: Cow::Borrowed("Log Entry"),
            id: Cow::Borrowed(id),
        }
        .json_patch(),
    }
    .entry_type("Event")
}

pub(super) struct LogServiceBuilder {
    value: serde_json::Value,
}

impl Builder for LogServiceBuilder {
    fn apply_patch(self, patch: serde_json::Value) -> Self {
        Self {
            value: self.value.patch(patch),
        }
    }
}

impl LogServiceBuilder {
    pub(super) fn entries(self, v: &redfish::Collection<'_>) -> Self {
        self.apply_patch(v.nav_property("Entries"))
    }

    /// The bound and the wrap policy a client needs to interpret a log that
    /// no longer holds its oldest entries, plus the action that empties it.
    pub(super) fn capacity(self, max_records: usize, clear_log_target: &str) -> Self {
        self.apply_patch(json!({
            "ServiceEnabled": true,
            "MaxNumberOfRecords": max_records,
            "OverWritePolicy": "WrapsWhenFull",
            "Actions": {"#LogService.ClearLog": {"target": clear_log_target}},
        }))
    }

    pub(super) fn build(self) -> serde_json::Value {
        self.value
    }
}

pub(crate) struct EntryBuilder {
    value: serde_json::Value,
}

impl Builder for EntryBuilder {
    fn apply_patch(self, patch: serde_json::Value) -> Self {
        Self {
            value: self.value.patch(patch),
        }
    }
}

impl EntryBuilder {
    fn entry_type(self, v: &str) -> Self {
        self.add_str_field("EntryType", v)
    }

    pub(crate) fn message(self, v: &str) -> Self {
        self.add_str_field("Message", v)
    }

    pub(crate) fn severity(self, v: &str) -> Self {
        self.add_str_field("Severity", v)
    }

    pub(crate) fn created(self, v: &str) -> Self {
        self.add_str_field("Created", v)
    }

    pub(crate) fn message_id(self, v: &str) -> Self {
        self.add_str_field("MessageId", v)
    }

    pub(crate) fn origin_of_condition(self, odata_id: &str) -> Self {
        self.apply_patch(json!({"Links": {"OriginOfCondition": {"@odata.id": odata_id}}}))
    }

    pub(crate) fn build(self) -> serde_json::Value {
        self.value
    }
}

/// Redfish `Health` vocabulary for mock-generated log entries and events.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Severity {
    Ok,
    Warning,
}

impl Severity {
    pub(crate) fn as_str(self) -> &'static str {
        match self {
            Self::Ok => "OK",
            Self::Warning => "Warning",
        }
    }
}

/// A lifecycle record the mock appends to a system event log and publishes as
/// a Redfish Event, the way a BMC logs and announces what it was asked to do.
#[derive(Clone, Debug)]
pub(crate) struct LogEntryDraft {
    /// DMTF ResourceEvent registry identifier.
    pub(crate) message_id: &'static str,
    pub(crate) message: String,
    pub(crate) severity: Severity,
    /// `@odata.id` of the affected resource.
    pub(crate) origin: String,
}

impl LogEntryDraft {
    /// A `ComputerSystem.Reset` action the mock accepted.
    pub(crate) fn reset_requested(system: &str, reset_type: SystemPowerControl) -> Self {
        let (message_id, message) = match reset_type {
            SystemPowerControl::On | SystemPowerControl::ForceOn => (
                "ResourceEvent.1.3.ResourcePoweredOn",
                format!("The resource '{system}' has powered on."),
            ),
            SystemPowerControl::GracefulShutdown | SystemPowerControl::ForceOff => (
                "ResourceEvent.1.3.ResourcePoweredOff",
                format!("The resource '{system}' has powered off."),
            ),
            other => (
                "ResourceEvent.1.3.ResourceStateChanged",
                format!("The state of resource '{system}' has changed to state {other:?}."),
            ),
        };
        Self {
            message_id,
            message,
            severity: Severity::Ok,
            origin: system.to_owned(),
        }
    }

    /// The embedder observed the host power on.
    pub(crate) fn powered_on(system: &str) -> Self {
        Self {
            message_id: "ResourceEvent.1.3.ResourcePoweredOn",
            message: format!("The resource '{system}' has powered on."),
            severity: Severity::Ok,
            origin: system.to_owned(),
        }
    }

    /// The embedder observed the host finish booting.
    pub(crate) fn boot_completed(system: &str) -> Self {
        Self {
            message_id: "ResourceEvent.1.3.ResourceStateChanged",
            message: format!("The state of resource '{system}' has changed to state Enabled."),
            severity: Severity::Ok,
            origin: system.to_owned(),
        }
    }

    /// A BMC reset was requested. Logged only: the reset closes every stream.
    pub(crate) fn manager_resetting(manager: &str, via: &str) -> Self {
        Self {
            message_id: "ResourceEvent.1.3.ResourceStateChanged",
            message: format!("The manager '{manager}' is resetting ({via})."),
            severity: Severity::Warning,
            origin: manager.to_owned(),
        }
    }
}

struct StoredEntry {
    id: u64,
    draft: LogEntryDraft,
    created: String,
}

/// Timestamp of profile-seeded entries, matching the captures they came from.
const SEED_CREATED: &str = "2026-02-12T02:06:58+00:00";

/// `LogService.MaxNumberOfRecords` unless a profile says otherwise: the size
/// of a typical IPMI system event log.
pub(crate) const DEFAULT_MAX_RECORDS: usize = 512;

struct Journal {
    /// The next entry's `Id`. Ids stay unique while the log wraps and restart
    /// only when the log is cleared, the way a BMC's SEL numbers its records.
    next_id: u64,
    entries: VecDeque<StoredEntry>,
}

/// One page of an entries collection.
pub(crate) struct EntryPage {
    pub(crate) members: Vec<Value>,
    /// Entries in the whole log, as `Members@odata.count`.
    pub(crate) total: usize,
    /// `$skip` of the next page, when this one did not reach the end.
    pub(crate) next_skip: Option<usize>,
}

/// One system's runtime event log: profile-seeded entries plus the lifecycle
/// entries the mock appends. Bounded like a real SEL — the oldest entry goes
/// when the log is full — and clearable through `LogService.ClearLog`.
pub(crate) struct EventLog {
    id: &'static str,
    capacity: usize,
    /// Entries per collection page; `None` serves the whole log at once.
    page_size: Option<usize>,
    journal: Mutex<Journal>,
}

impl EventLog {
    pub(crate) fn new(
        id: &'static str,
        capacity: usize,
        page_size: Option<usize>,
        messages: impl IntoIterator<Item = &'static str>,
    ) -> Self {
        let entries: VecDeque<StoredEntry> = messages
            .into_iter()
            .enumerate()
            .map(|(id, message)| StoredEntry {
                id: id as u64,
                draft: LogEntryDraft {
                    message_id: "ResourceEvent.1.3.ResourceStateChanged",
                    message: message.to_owned(),
                    severity: Severity::Ok,
                    origin: String::new(),
                },
                created: SEED_CREATED.to_owned(),
            })
            .collect();
        assert!(capacity > 0, "an event log holds at least one entry");
        assert!(
            entries.len() <= capacity,
            "seeded entries exceed the log's capacity"
        );
        Self {
            id,
            capacity,
            page_size,
            journal: Mutex::new(Journal {
                next_id: entries.len() as u64,
                entries,
            }),
        }
    }

    fn seeded(id: &'static str, messages: impl IntoIterator<Item = &'static str>) -> Self {
        Self::new(id, DEFAULT_MAX_RECORDS, None, messages)
    }

    pub(crate) fn id(&self) -> &str {
        self.id
    }

    pub(crate) fn capacity(&self) -> usize {
        self.capacity
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, Journal> {
        self.journal.lock().expect("event log poisoned")
    }

    /// Append an entry and return its `Id`, evicting the oldest entry when
    /// the log is full.
    pub(crate) fn append(&self, draft: LogEntryDraft, created: &str) -> String {
        let mut journal = self.lock();
        let id = journal.next_id;
        journal.next_id += 1;
        journal.entries.push_back(StoredEntry {
            id,
            draft,
            created: created.to_owned(),
        });
        while journal.entries.len() > self.capacity {
            journal.entries.pop_front();
        }
        id.to_string()
    }

    /// `LogService.ClearLog`: the log is empty and ids start over, so a
    /// client that keyed on `Id` alone sees new records under old ids.
    pub(crate) fn clear(&self) {
        let mut journal = self.lock();
        journal.entries.clear();
        journal.next_id = 0;
    }

    /// One page of entries under `collection`, oldest first. `top` is capped
    /// at the profile's page size; unpaged logs serve everything from `skip`.
    pub(crate) fn page(
        &self,
        collection: &redfish::Collection<'_>,
        skip: usize,
        top: Option<usize>,
    ) -> EntryPage {
        let journal = self.lock();
        let total = journal.entries.len();
        let limit = match (top, self.page_size) {
            (Some(top), Some(page)) => Some(top.min(page)),
            (Some(top), None) => Some(top),
            (None, page) => page,
        };
        let members: Vec<Value> = journal
            .entries
            .iter()
            .skip(skip)
            .take(limit.unwrap_or(usize::MAX))
            .map(|entry| entry.render(collection))
            .collect();
        let served = skip.saturating_add(members.len());
        let next_skip = (limit.is_some() && served < total).then_some(served);
        EntryPage {
            members,
            total,
            next_skip,
        }
    }

    /// The LogEntry document with this `Id`, if the log still holds it.
    pub(crate) fn entry(&self, collection: &redfish::Collection<'_>, id: &str) -> Option<Value> {
        let id: u64 = id.parse().ok()?;
        self.lock()
            .entries
            .iter()
            .find(|entry| entry.id == id)
            .map(|entry| entry.render(collection))
    }
}

impl StoredEntry {
    fn render(&self, collection: &redfish::Collection<'_>) -> Value {
        let builder = event_entry(collection, &self.id.to_string())
            .message(&self.draft.message)
            .message_id(self.draft.message_id)
            .severity(self.draft.severity.as_str())
            .created(&self.created);
        if self.draft.origin.is_empty() {
            builder
        } else {
            builder.origin_of_condition(&self.draft.origin)
        }
        .build()
    }
}

/// The log services one system exposes. Every current profile that has any
/// exposes a single `EventLog`.
pub(crate) struct LogServices {
    services: Vec<EventLog>,
}

impl LogServices {
    /// One `EventLog` service seeded with informational entries.
    pub(crate) fn event_log(messages: impl IntoIterator<Item = &'static str>) -> Self {
        Self {
            services: vec![EventLog::seeded("EventLog", messages)],
        }
    }

    /// Serve entries `page_size` at a time with `Members@odata.nextLink`, as
    /// BMCs with large logs do; a client that reads only the first page sees
    /// only the oldest entries.
    pub(crate) fn paged(mut self, page_size: usize) -> Self {
        assert!(page_size > 0, "a page holds at least one entry");
        for service in &mut self.services {
            service.page_size = Some(page_size);
        }
        self
    }

    pub(crate) fn services(&self) -> &[EventLog] {
        &self.services
    }

    pub(crate) fn find(&self, id: &str) -> Option<&EventLog> {
        self.services.iter().find(|service| service.id() == id)
    }

    /// The log that receives lifecycle entries.
    pub(crate) fn primary(&self) -> Option<&EventLog> {
        self.services.first()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ids(log: &EventLog) -> Vec<String> {
        let collection = system_entries_collection("S", log.id());
        log.page(&collection, 0, None)
            .members
            .iter()
            .map(|entry| entry["Id"].as_str().unwrap().to_owned())
            .collect()
    }

    #[test]
    fn a_full_log_drops_its_oldest_entry_and_keeps_ids_unique() {
        let log = EventLog::new("SEL", 3, None, ["seed"]);
        for _ in 0..3 {
            log.append(
                LogEntryDraft::powered_on("/redfish/v1/Systems/S"),
                SEED_CREATED,
            );
        }
        assert_eq!(ids(&log), ["1", "2", "3"], "the seed at id 0 rotated out");
        assert_eq!(ids(&log).len(), log.capacity());
        let collection = system_entries_collection("S", "SEL");
        assert!(log.entry(&collection, "0").is_none());
        assert_eq!(log.entry(&collection, "3").unwrap()["Id"], "3");
        assert!(log.entry(&collection, "not-a-number").is_none());

        log.clear();
        assert!(ids(&log).is_empty());
        assert_eq!(
            log.append(
                LogEntryDraft::powered_on("/redfish/v1/Systems/S"),
                SEED_CREATED
            ),
            "0",
            "a cleared log reuses its ids"
        );
    }

    #[test]
    fn pages_are_capped_at_the_page_size_and_link_onward() {
        let log = EventLog::new("SEL", 10, Some(2), ["a", "b", "c", "d", "e"]);
        let collection = system_entries_collection("S", "SEL");
        let first = log.page(&collection, 0, None);
        assert_eq!(
            (first.members.len(), first.total, first.next_skip),
            (2, 5, Some(2))
        );
        let capped = log.page(&collection, 0, Some(4));
        assert_eq!(capped.members.len(), 2, "$top cannot exceed the page size");
        let last = log.page(&collection, 4, None);
        assert_eq!((last.members.len(), last.next_skip), (1, None));
        assert!(log.page(&collection, 9, None).members.is_empty());

        let unpaged = EventLog::new("SEL", 10, None, ["a", "b", "c"]);
        let all = unpaged.page(&collection, 0, None);
        assert_eq!((all.members.len(), all.next_skip), (3, None));
        let top = unpaged.page(&collection, 0, Some(2));
        assert_eq!(top.next_skip, Some(2), "a client-supplied $top still pages");
    }
}
