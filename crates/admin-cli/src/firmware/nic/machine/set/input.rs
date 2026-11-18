/*
 * SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
 * SPDX-License-Identifier: Apache-2.0
 */

use std::io::IsTerminal;
use std::path::Path;

use eyre::Context;
use model::nic_firmware::NicFirmwareCardSelection;
use rpc::forge;
use rpc::forge::nic_firmware_card_selector::Identity;
use serde::Deserialize;

pub(super) const CARDS_HELP: &str = "\
Read a complete UTF-8 JSON array of card selections. Omission preserves saved \
cards without reading stdin; '-' reads stdin until EOF and any other value is \
a file path. [] clears all card selections. Null, an enclosing object, or YAML \
is not accepted. Each entry requires selector and profile_id. The selector is \
exactly one case-sensitive BaseMac or BaseGuid tag containing a string.

BaseMac accepts twelve hex digits or six two-digit octets separated consistently \
by colons or hyphens; it must be nonzero and unicast. BaseGuid is an exact \
reported string of 1–256 UTF-8 bytes without surrounding whitespace or controls; \
-- and N/A are invalid. Profile IDs are literal, nonempty strings without \
surrounding whitespace or controls. Unknown entry fields are rejected. Selector \
and profile-ID syntax are checked locally; Core checks profile existence, device \
matches, compatibility, duplicate selections and its max_find_by_ids card count limit.

Each selector must match exactly one stored device. Ordering is not priority. \
To reuse machine show JSON output, extract card_selections as JSON. A terminal \
stdin prints a reading hint to stderr. Unreadable or invalid JSON fails before \
the configuration RPC.

Example replacement (use identities from the host's stored device report):

```json

    [
      {\"selector\": {\"BaseMac\": \"00:11:22:33:44:55\"}, \"profile_id\": \"connectx-production\"},
      {\"selector\": {\"BaseGuid\": \"0011223344556677\"}, \"profile_id\": \"connectx-production\"}
    ]

```";

/// Preserve raw selector text until the RPC conversion checks its spelling;
/// deserializing a model MAC first would accept the MAC library's looser syntax.
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct CardInput {
    selector: SelectorInput,
    profile_id: String,
}

#[derive(Deserialize)]
enum SelectorInput {
    #[serde(rename = "BaseMac")]
    Mac(String),
    #[serde(rename = "BaseGuid")]
    Guid(String),
}

pub(super) fn read_cards(path: &Path) -> eyre::Result<Vec<forge::NicFirmwareCardSelection>> {
    let json = if path.as_os_str() == "-" {
        let stdin = std::io::stdin();
        if stdin.is_terminal() {
            eprintln!(
                "Reading NIC firmware card selections from standard input; send EOF when finished."
            );
        }
        std::io::read_to_string(stdin.lock())
            .wrap_err("while attempting to read NIC firmware card selections from standard input")?
    } else {
        std::fs::read_to_string(path).wrap_err_with(|| {
            format!(
                "while attempting to read NIC firmware card selections from {}",
                path.display()
            )
        })?
    };
    let cards: Vec<CardInput> = serde_json::from_str(&json)
        .wrap_err("while attempting to parse NIC firmware card selections JSON")?;
    cards
        .into_iter()
        .enumerate()
        .map(|(index, card)| {
            let card = forge::NicFirmwareCardSelection {
                selector: Some(forge::NicFirmwareCardSelector {
                    identity: Some(match card.selector {
                        SelectorInput::Mac(mac) => Identity::BaseMac(mac),
                        SelectorInput::Guid(guid) => Identity::BaseGuid(guid),
                    }),
                }),
                profile_id: card.profile_id,
            };
            let validated = NicFirmwareCardSelection::try_from(card).wrap_err_with(|| {
                format!("while attempting to validate card_selections[{index}]")
            })?;
            Ok(validated.into())
        })
        .collect()
}
