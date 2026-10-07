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
use russh::keys::Certificate;

use crate::config::{CertAuthorization, CertAuthorizationStrategy, KeyIdFormat};

/// Search for the given role in the Key ID field of a certificate, returning if it is declared.
pub(crate) fn certificate_contains_role(
    certificate: &Certificate,
    role: &str,
    cert_authorization: &CertAuthorization,
) -> bool {
    for strategy in &cert_authorization.strategy {
        match strategy {
            CertAuthorizationStrategy::KeyId => {
                if key_id_contains_role(
                    certificate.key_id(),
                    role,
                    &cert_authorization.keyid_format,
                ) {
                    return true;
                }
            }
        }
    }

    false
}

/// Try to get the username from the given certificate, or return None if we couldn't find one.
pub(crate) fn get_user_from_certificate<'a>(
    certificate: &'a Certificate,
    cert_authorization: &CertAuthorization,
) -> Option<&'a str> {
    if let Some(principal) = certificate.valid_principals().first() {
        return Some(principal.as_str());
    }
    for strategy in &cert_authorization.strategy {
        match strategy {
            CertAuthorizationStrategy::KeyId => {
                if let Some(user) =
                    get_user_from_key_id(certificate.key_id(), &cert_authorization.keyid_format)
                {
                    return Some(user);
                }
            }
        }
    }

    None
}

fn key_id_contains_role(key_id: &str, role: &str, key_id_format: &KeyIdFormat) -> bool {
    // Example:
    //     group=some-group user=some-user roles=role1,role2,role3
    let Some(roles_attr) = key_id
        .split(&key_id_format.field_separator)
        .find_map(|field| {
            field.split_once('=').and_then(|(k, v)| {
                if k == key_id_format.role_field {
                    Some(v)
                } else {
                    None
                }
            })
        })
    else {
        tracing::warn!(
            role_field = key_id_format.role_field.as_str(),
            "Could not find role field in key ID"
        );
        return false;
    };

    roles_attr
        .split(&key_id_format.role_separator)
        .any(|k| k == role)
}

fn get_user_from_key_id<'a>(key_id: &'a str, key_id_format: &KeyIdFormat) -> Option<&'a str> {
    // Example:
    //     group=some-group user=some-user roles=role1,role2,role3
    let Some(user) = key_id
        .split(&key_id_format.field_separator)
        .find_map(|field| {
            field.split_once('=').and_then(|(k, v)| {
                if k == key_id_format.user_field {
                    Some(v)
                } else {
                    None
                }
            })
        })
    else {
        tracing::warn!(
            user_field = key_id_format.user_field.as_str(),
            "Could not find user field in key ID"
        );
        return None;
    };

    Some(user)
}

#[cfg(test)]
mod tests {
    use carbide_test_support::value_scenarios;

    use super::*;

    /// A custom [`KeyIdFormat`] for the scenarios that exercise non-default
    /// separators or field names. Spelled out so each row reads as the format it
    /// means; [`KeyIdFormat::default()`] covers the common `" "`/`","` case.
    fn key_id_fmt(
        field_separator: &str,
        user_field: &str,
        role_field: &str,
        role_separator: &str,
    ) -> KeyIdFormat {
        KeyIdFormat {
            field_separator: field_separator.into(),
            user_field: user_field.into(),
            role_field: role_field.into(),
            role_separator: role_separator.into(),
        }
    }

    /// One row of the [`key_id_contains_role`] table: a Key ID, the role to look
    /// for, and the format that governs how the Key ID is split.
    struct RoleCase {
        key_id: &'static str,
        role: &'static str,
        format: KeyIdFormat,
    }

    /// One row of the [`get_user_from_key_id`] table: a Key ID and the format that
    /// governs how the user field is found.
    struct UserCase {
        key_id: &'static str,
        format: KeyIdFormat,
    }

    #[test]
    fn key_id_contains_role_finds_declared_roles() {
        value_scenarios!(
            run = |RoleCase { key_id, role, format }| key_id_contains_role(key_id, role, &format);

            "declared roles, default format" {
                RoleCase { key_id: "group=group1 user=ksimon roles=role1,role2,role3", role: "role1", format: KeyIdFormat::default() } => true,
                RoleCase { key_id: "group=group1 user=ksimon roles=role1,role2,role3", role: "role3", format: KeyIdFormat::default() } => true,
                RoleCase { key_id: "group=group1 user=ksimon roles=role1,role2,role3", role: "fakerole", format: KeyIdFormat::default() } => false,
            }

            "exact match, not substring" {
                RoleCase { key_id: "group=g user=u roles=role1,role2", role: "role", format: KeyIdFormat::default() } => false,
                RoleCase { key_id: "group=g user=u roles=role1,role2", role: "role2", format: KeyIdFormat::default() } => true,
            }

            "missing or empty roles field" {
                RoleCase { key_id: "group=g user=u", role: "role1", format: KeyIdFormat::default() } => false,
                RoleCase { key_id: "group=g user=u roles=", role: "anything", format: KeyIdFormat::default() } => false,
            }

            "trailing role separator" {
                RoleCase { key_id: "group=g user=u roles=role1,role2,", role: "role1", format: KeyIdFormat::default() } => true,
                RoleCase { key_id: "group=g user=u roles=role1,role2,", role: "role2", format: KeyIdFormat::default() } => true,
                RoleCase { key_id: "group=g user=u roles=role1,role2,", role: "role3", format: KeyIdFormat::default() } => false,
            }

            "custom role separator" {
                RoleCase { key_id: "group=g user=u roles=alpha;beta;gamma", role: "beta", format: key_id_fmt(" ", "user", "roles", ";") } => true,
                RoleCase { key_id: "group=g user=u roles=alpha;beta;gamma", role: "delta", format: key_id_fmt(" ", "user", "roles", ";") } => false,
            }

            "custom field separator" {
                RoleCase { key_id: "group=g|user=u|roles=a,b,c", role: "b", format: key_id_fmt("|", "user", "roles", ",") } => true,
            }

            "fields in any order" {
                RoleCase { key_id: "roles=r1,r2 group=g user=u", role: "r2", format: KeyIdFormat::default() } => true,
            }

            "custom fields and separator" {
                RoleCase { key_id: "tenant=acme|login=alice|scopes=read;write", role: "write", format: key_id_fmt("|", "login", "scopes", ";") } => true,
            }

            "duplicate roles" {
                RoleCase { key_id: "group=g user=u roles=dup,dup", role: "dup", format: KeyIdFormat::default() } => true,
            }

            "role field name must match" {
                // Roles under a different field name; the default format does not find them.
                RoleCase { key_id: "group=g user=u permissions=a,b,c", role: "a", format: KeyIdFormat::default() } => false,
                // With a matching role_field it does.
                RoleCase { key_id: "group=g user=u permissions=a,b,c", role: "a", format: key_id_fmt(" ", "user", "permissions", ",") } => true,
            }
        );
    }

    #[test]
    fn get_user_from_key_id_extracts_the_user() {
        value_scenarios!(
            run = |UserCase { key_id, format }| get_user_from_key_id(key_id, &format);

            "user present, default format" {
                UserCase { key_id: "group=group1 user=ksimon roles=role1,role2,role3", format: KeyIdFormat::default() } => Some("ksimon"),
            }

            "missing user field" {
                UserCase { key_id: "group=g roles=r1,r2", format: KeyIdFormat::default() } => None,
            }

            "custom fields and separator" {
                UserCase { key_id: "tenant=acme|login=alice|scopes=read;write", format: key_id_fmt("|", "login", "scopes", ";") } => Some("alice"),
            }

            "value contains an equals sign" {
                // Values may contain '='; split_once still yields the correct (k, v).
                UserCase { key_id: "group=g user=alice=dev roles=r1,r2", format: KeyIdFormat::default() } => Some("alice=dev"),
            }
        );
    }
}
