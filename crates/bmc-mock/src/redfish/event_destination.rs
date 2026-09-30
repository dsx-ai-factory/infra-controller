// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

//! `EventDestination`: one subscriber of the `EventService`. The mock creates a
//! member for every open SSE stream and removes it on disconnect; there is no
//! subscription POST, webhook delivery, or persistence.

use std::borrow::Cow;

use crate::json::{JsonExt, JsonPatch};
use crate::redfish;
use crate::redfish::Builder;

pub(crate) const SUBSCRIPTIONS: &str = "/redfish/v1/EventService/Subscriptions";

pub(crate) fn collection() -> redfish::Collection<'static> {
    redfish::Collection {
        odata_id: Cow::Borrowed(SUBSCRIPTIONS),
        odata_type: Cow::Borrowed("#EventDestinationCollection.EventDestinationCollection"),
        name: Cow::Borrowed("SSE Subscriptions"),
    }
}

pub(crate) fn resource(id: u64) -> redfish::Resource<'static> {
    redfish::Resource {
        odata_id: Cow::Owned(format!("{SUBSCRIPTIONS}/{id}")),
        odata_type: Cow::Borrowed("#EventDestination.v1_6_0.EventDestination"),
        id: Cow::Owned(id.to_string()),
        name: Cow::Borrowed("SSE Subscription"),
    }
}

pub(crate) fn builder(resource: &redfish::Resource<'_>) -> EventDestinationBuilder {
    EventDestinationBuilder {
        value: resource.json_patch(),
    }
}

pub(crate) struct EventDestinationBuilder {
    value: serde_json::Value,
}

impl Builder for EventDestinationBuilder {
    fn apply_patch(self, patch: serde_json::Value) -> Self {
        Self {
            value: self.value.patch(patch),
        }
    }
}

impl EventDestinationBuilder {
    /// The opaque `Context` a client would have supplied when subscribing.
    pub(crate) fn context(self, v: &str) -> Self {
        self.add_str_field("Context", v)
    }

    /// A server-sent-event destination over Redfish.
    pub(crate) fn sse(self) -> Self {
        self.add_str_field("Protocol", "Redfish")
            .add_str_field("SubscriptionType", "SSE")
    }

    pub(crate) fn build(self) -> serde_json::Value {
        self.value
    }
}
