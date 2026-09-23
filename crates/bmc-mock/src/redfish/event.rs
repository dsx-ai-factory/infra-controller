// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

//! `Event`: the document a BMC pushes to its subscribers, carrying one or
//! more `EventRecord`s. The mock publishes one per lifecycle action.

use std::borrow::Cow;

use serde_json::json;

use crate::json::{JsonExt, JsonPatch};
use crate::redfish;
use crate::redfish::Builder;

/// The `Event` document numbered `sequence` on this BMC. The path is not
/// served; like a real BMC's, it only identifies the event.
pub(crate) fn resource(sequence: u64) -> redfish::Resource<'static> {
    redfish::Resource {
        odata_id: Cow::Owned(format!("/redfish/v1/EventService/Events/{sequence}")),
        odata_type: Cow::Borrowed("#Event.v1_6_0.Event"),
        id: Cow::Owned(sequence.to_string()),
        name: Cow::Borrowed("Event Array"),
    }
}

/// One `EventRecord`, in the fields a log collector reads inline.
pub(crate) struct EventRecord<'a> {
    pub(crate) message_id: &'a str,
    pub(crate) message: &'a str,
    /// Redfish `Health` vocabulary: `OK`, `Warning`, or `Critical`.
    pub(crate) severity: &'a str,
    /// RFC 3339 `EventTimestamp`.
    pub(crate) timestamp: &'a str,
    /// `@odata.id` of the resource or `LogEntry` the record is about.
    pub(crate) origin: &'a str,
}

pub(crate) fn builder(resource: &redfish::Resource<'_>) -> EventBuilder {
    EventBuilder {
        value: resource.json_patch().patch(json!({"Events": []})),
        odata_id: resource.odata_id.to_string(),
        id: resource.id.to_string(),
    }
}

pub(crate) struct EventBuilder {
    value: serde_json::Value,
    odata_id: String,
    id: String,
}

impl Builder for EventBuilder {
    fn apply_patch(self, patch: serde_json::Value) -> Self {
        Self {
            value: self.value.patch(patch),
            odata_id: self.odata_id,
            id: self.id,
        }
    }
}

impl EventBuilder {
    /// Append a record. `MemberId` is its position; `EventId` is the event's.
    pub(crate) fn record(mut self, record: &EventRecord<'_>) -> Self {
        let events = self.value["Events"]
            .as_array_mut()
            .expect("the Event document carries an Events array");
        let member = events.len();
        events.push(json!({
            "@odata.id": format!("{}#/Events/{member}", self.odata_id),
            "MemberId": member.to_string(),
            "EventId": self.id,
            "EventType": "Alert",
            "EventTimestamp": record.timestamp,
            "MessageId": record.message_id,
            "Message": record.message,
            "MessageSeverity": record.severity,
            "OriginOfCondition": {"@odata.id": record.origin},
        }));
        self
    }

    pub(crate) fn build(self) -> serde_json::Value {
        self.value
    }
}
