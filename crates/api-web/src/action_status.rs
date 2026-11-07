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
use std::collections::HashMap;

use itertools::Itertools;

#[derive(PartialEq, Eq)]
pub(super) enum Type {
    Power,
    ResetBmc,
    MachineSetup,
    DesiredBootInterface,
    SetFirstBootOrder,
    RestoreBootInterface,
    DisableSecureBoot,
    EnableLockdown,
    DisableLockdown,
    Sku,
}

impl Type {
    fn from_query(query: &HashMap<String, String>) -> Option<Self> {
        query
            .get("action_type")
            .map(String::as_str)
            .and_then(|action| match action {
                "power" => Some(Type::Power),
                "reset_bmc" => Some(Type::ResetBmc),
                "machine_setup" => Some(Type::MachineSetup),
                "desired_boot_interface" => Some(Type::DesiredBootInterface),
                "set_first_boot_order" => Some(Type::SetFirstBootOrder),
                "restore_boot_interface" => Some(Type::RestoreBootInterface),
                "disable_secure_boot" => Some(Type::DisableSecureBoot),
                "enable_lockdown" => Some(Type::EnableLockdown),
                "disable_lockdown" => Some(Type::DisableLockdown),
                "sku" => Some(Type::Sku),
                _ => None,
            })
    }
    fn to_query(&self) -> (&'static str, &'static str) {
        (
            "action_type",
            match self {
                Type::Power => "power",
                Type::ResetBmc => "reset_bmc",
                Type::MachineSetup => "machine_setup",
                Type::DesiredBootInterface => "desired_boot_interface",
                Type::SetFirstBootOrder => "set_first_boot_order",
                Type::RestoreBootInterface => "restore_boot_interface",
                Type::DisableSecureBoot => "disable_secure_boot",
                Type::EnableLockdown => "enable_lockdown",
                Type::DisableLockdown => "disable_lockdown",
                Type::Sku => "sku",
            },
        )
    }
    fn display_name(&self) -> &'static str {
        match self {
            Type::Power => "Power Control",
            Type::ResetBmc => "BMC Reset",
            Type::MachineSetup => "Machine Setup",
            Type::DesiredBootInterface => "Desired Boot Interface",
            Type::SetFirstBootOrder => "Boot Order",
            Type::RestoreBootInterface => "Restore Boot Interface",
            Type::DisableSecureBoot => "Disable Secure Boot",
            Type::EnableLockdown => "Enable Lockdown",
            Type::DisableLockdown => "Disable Lockdown",
            Type::Sku => "SKU Assignment",
        }
    }
}

pub(super) enum Class {
    Success,
    Warning,
    Error,
}

impl Class {
    fn from_query(query: &HashMap<String, String>) -> Self {
        match query.get("action_class").map(String::as_str) {
            Some("success") => Self::Success,
            Some("error") => Self::Error,
            _ => Self::Warning,
        }
    }

    fn to_query(&self) -> (&'static str, &'static str) {
        ("action_class", self.as_str())
    }

    fn as_str(&self) -> &'static str {
        match self {
            Self::Success => "success",
            Self::Error => "error",
            Self::Warning => "warning",
        }
    }
}

pub(super) struct ActionStatus<'a> {
    pub(super) action: Type,
    pub(super) class: Class,
    pub(super) message: Cow<'a, str>,
}

impl ActionStatus<'_> {
    pub(super) fn from_query(query: &HashMap<String, String>) -> Option<ActionStatus<'_>> {
        Type::from_query(query).map(|action| {
            let message = query
                .get("action_message")
                .map(String::as_str)
                .unwrap_or(action.to_query().1);
            ActionStatus {
                action,
                message: Cow::Borrowed(message),
                class: Class::from_query(query),
            }
        })
    }

    fn url_cleanup_script() -> &'static str {
        r#"<script>
(function() {
  const url = new URL(window.location.href);
  url.searchParams.delete('action_type');
  url.searchParams.delete('action_class');
  url.searchParams.delete('action_message');
  const newUrl = url.pathname + url.search + url.hash;
  window.history.replaceState({}, document.title, newUrl);
})();
</script>"#
    }

    pub(super) fn action_result_script(&self) -> String {
        let cleanup = Self::url_cleanup_script();
        let name = self.action.display_name();
        let escaped_msg = self
            .message
            .replace('\\', "\\\\")
            .replace('\'', "\\'")
            .replace('\n', "\\n");
        match self.class {
            Class::Success | Class::Warning => {
                format!(
                    "<script>document.addEventListener('DOMContentLoaded',function(){{showToast('{escaped_msg}')}});</script>\n{cleanup}"
                )
            }
            Class::Error => {
                format!(
                    "<script>document.addEventListener('DOMContentLoaded',function(){{showErrorModal('{escaped_msg}','{name} Failed')}});</script>\n{cleanup}"
                )
            }
        }
    }

    pub(super) fn update_redirect_url(&self, redirect_url: &str) -> String {
        let (base_url, anchor) = match redirect_url.rfind('#') {
            Some(pos) => (&redirect_url[..pos], &redirect_url[pos..]),
            None => (redirect_url, ""),
        };
        format!(
            "{base_url}?{}{anchor}",
            [
                self.action.to_query(),
                self.class.to_query(),
                ("action_message", &self.message),
            ]
            .iter()
            .map(|(k, v)| format!("{k}={}", urlencoding::encode(v)))
            .join("&"),
        )
    }
}
