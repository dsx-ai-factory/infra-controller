// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

//! `EventDestination`: one subscriber of the `EventService`. The mock creates a
//! member for every open SSE stream and removes it on disconnect; there is no
//! subscription POST, webhook delivery, or persistence.

use std::borrow::Cow;

use crate::redfish;
use crate::redfish::ModelExt;

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
        odata_type: Cow::Borrowed(redfish::schema::event_destination::EventDestination::ODATA_TYPE),
        id: Cow::Owned(id.to_string()),
        name: Cow::Borrowed("SSE Subscription"),
    }
}

pub(crate) fn sse(resource: &redfish::Resource<'_>, context: &str) -> serde_json::Value {
    use super::schema::event_destination::{
        EventDestination, EventDestinationProtocol, EventDestinationRequired, SubscriptionType,
    };
    EventDestination::new(EventDestinationRequired {
        odata_id: resource.odata_id.to_string(),
        id: resource.id.to_string(),
        name: resource.name.to_string(),
        context: Some(context.to_string()),
        subscription_type: Some(SubscriptionType::Sse),
    })
    .protocol(EventDestinationProtocol::Redfish)
    .into_json()
}
