/*
 * SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
 * SPDX-License-Identifier: Apache-2.0
 */

use std::io::IsTerminal;
use std::path::{Path, PathBuf};

use clap::Parser;
use eyre::Context;
use model::nic_firmware::NicFirmwareProfileConfig;

const CONFIG_HELP: &str = "\
Read a complete NIC firmware profile config as UTF-8 JSON. Omission or a single hyphen (-) reads \
standard input until EOF; any other value is a file path. This is request data, \
not a controller configuration file. Supply the config object itself, without \
an enclosing id, config, or version field. To reuse JSON output, extract \
[0].config as JSON. YAML output is not accepted as input. When standard input \
is a terminal, the CLI prints a reading hint to stderr.

The required entries array must be nonempty. Entries preserve order and must \
have unique (part_number, psid) pairs. Each entry requires firmware with \
part_number, psid, and exact version strings; image with url and sha256; and \
approach set to the case-sensitive string \"Scout\". Part numbers, PSIDs, and \
firmware versions are literal, nonempty strings without surrounding whitespace \
or control characters. An older firmware version is a valid target. The optional \
device_config is omitted or null when absent, or has the same url/sha256 fields \
as image. Hardware selection and resolution support at most 256 UTF-8 bytes \
per part number or PSID; catalog registration accepts longer strings.

Artifact URLs must be absolute HTTP(S) URLs accessible without login, with no \
userinfo, query, or fragment. Each sha256 must contain exactly 64 hexadecimal \
digits. URLs are normalized and digests stored lowercase. Registration does \
not fetch artifacts or verify their bytes. Unrecognized JSON fields are ignored; \
a misspelled device_config is treated as absent. Omitting device_config removes \
it when replacing a profile.

Unreadable input, malformed JSON, missing required fields, invalid URL syntax, \
and unsupported approach names fail locally before a catalog RPC. Core validates \
hardware pairs, artifact restrictions, and digests, returning server status \
INVALID_ARGUMENT for invalid definitions.

Example config (replace the URL and digest with values for the actual artifact):

```json

    {
      \"entries\": [{
        \"firmware\": {
          \"part_number\": \"900-9D3B4-00CV-TA0\",
          \"psid\": \"MT_0000000884\",
          \"version\": \"32.43.1014\"
        },
        \"image\": {
          \"url\": \"https://firmware.example.com/connectx.bin\",
          \"sha256\": \"aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa\"
        },
        \"device_config\": null,
        \"approach\": \"Scout\"
      }]
    }

```";

#[derive(Parser, Debug)]
pub(super) struct ConfigInput {
    #[arg(
        long,
        default_value = "-",
        value_name = "PATH",
        help = "Config JSON file, or '-' for standard input (see --help for schema)",
        long_help = CONFIG_HELP
    )]
    pub(super) config_file: PathBuf,
}

pub(super) fn read_config(path: &Path) -> eyre::Result<NicFirmwareProfileConfig> {
    let json = if path.as_os_str() == "-" {
        let stdin = std::io::stdin();
        if stdin.is_terminal() {
            eprintln!(
                "Reading NIC firmware profile JSON from standard input; send EOF when finished, or use --config-file PATH."
            );
        }
        std::io::read_to_string(stdin.lock())
            .wrap_err("while attempting to read NIC firmware profile config from standard input")?
    } else {
        std::fs::read_to_string(path).wrap_err_with(|| {
            format!(
                "while attempting to read NIC firmware profile config from {}",
                path.display()
            )
        })?
    };
    serde_json::from_str(&json)
        .wrap_err("while attempting to parse NIC firmware profile config JSON")
}
