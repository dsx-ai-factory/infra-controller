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

use crate::redfish;

pub(super) fn resource<'a>(system_id: &'a str) -> redfish::Resource<'a> {
    let odata_id = format!(
        "{}/SecureBoot",
        redfish::computer_system::resource(system_id).odata_id
    );
    redfish::Resource {
        odata_id: Cow::Owned(odata_id),
        odata_type: Cow::Borrowed(redfish::schema::secure_boot::SecureBoot::ODATA_TYPE),
        id: Cow::Borrowed("SecureBoot"),
        name: Cow::Borrowed("UEFI Secure Boot"),
    }
}

pub(super) fn builder(resource: &redfish::Resource) -> redfish::schema::secure_boot::SecureBoot {
    redfish::schema::secure_boot::SecureBoot::new(
        resource.odata_id.as_ref(),
        resource.id.as_ref(),
        resource.name.as_ref(),
    )
}
