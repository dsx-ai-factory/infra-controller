// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

//! `Event`: the document a BMC pushes to its subscribers, carrying one or
//! more `EventRecord`s. The mock publishes one per lifecycle action.

use std::borrow::Cow;

use serde_json::json;

use crate::json::JsonExt;
use crate::redfish;
use crate::redfish::ModelExt;

/// The `Event` document numbered `sequence` on this BMC. The path is not
/// served; like a real BMC's, it only identifies the event.
pub(crate) fn resource(sequence: u64) -> redfish::Resource<'static> {
    redfish::Resource {
        odata_id: Cow::Owned(format!("/redfish/v1/EventService/Events/{sequence}")),
        odata_type: Cow::Borrowed(redfish::schema::event::Event::ODATA_TYPE),
        id: Cow::Owned(sequence.to_string()),
        name: Cow::Borrowed("Event Array"),
    }
}

/// One `EventRecord`, in the fields a log collector reads inline.
pub(crate) struct EventRecord<'a> {
    pub(crate) message_id: &'a str,
    pub(crate) message: &'a str,
    /// Redfish `Health` vocabulary: `OK`, `Warning`, or `Critical`.
    pub(crate) severity: super::schema::resource::Health,
    /// RFC 3339 `EventTimestamp`.
    pub(crate) timestamp: &'a str,
    /// `@odata.id` of the resource or `LogEntry` the record is about.
    pub(crate) origin: &'a str,
}

pub(crate) fn response(
    resource: &redfish::Resource<'_>,
    record: &EventRecord<'_>,
) -> serde_json::Value {
    use super::schema::event::{
        Event, EventRecord as Record, EventRecordRequired, EventRequired, EventType,
    };
    let member_uri = format!("{}#/Events/0", resource.odata_id);
    let event = Event::new(EventRequired {
        odata_id: resource.odata_id.to_string(),
        id: resource.id.to_string(),
        name: resource.name.to_string(),
        events: vec![member_uri.as_str().into()],
    });
    let record = Record::new(EventRecordRequired {
        odata_id: member_uri,
        member_id: "0".to_string(),
        event_type: EventType::Alert,
        message_id: record.message_id.to_string(),
    })
    .event_id(resource.id.as_ref())
    .event_timestamp(record.timestamp)
    .message(record.message)
    .message_severity(record.severity)
    .origin_of_condition(record.origin);
    // The generator represents navigation properties as links; SSE carries
    // EventRecord resources expanded inline.
    event.into_json().patch(json!({"Events": [record]}))
}
