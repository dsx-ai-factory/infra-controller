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

use clap::Parser;

/// Require a pinned SHA256 digest on container image references.
///
/// Tags are mutable; a digest is the only guarantee of reproducibility.
fn parse_img_name(s: &str) -> Result<String, String> {
    let (name_part, digest_part) = s
        .split_once('@')
        .ok_or("must include a SHA256 digest (e.g. image:tag@sha256:<digest>)")?;
    if name_part.is_empty() {
        return Err("image name before '@' must not be empty".into());
    }
    if digest_part.contains('@') {
        return Err("must contain exactly one '@sha256:<digest>' suffix".into());
    }
    let hex_str = digest_part
        .strip_prefix("sha256:")
        .ok_or("digest must use the 'sha256:' algorithm prefix")?;
    let decoded = hex::decode(hex_str).map_err(|e| format!("digest is not valid hex: {e}"))?;
    if decoded.len() != 32 {
        return Err(format!(
            "SHA256 digest must decode to 32 bytes, got {}",
            decoded.len()
        ));
    }
    Ok(s.to_string())
}

#[derive(Parser, Debug)]
#[command(after_long_help = "\
EXAMPLES:

List supported tests (optionally including unverified):
    $ nico-admin-cli machine-validation tests show

Verify a test version:
    $ nico-admin-cli machine-validation tests verify --test-id gpu_bandwidth --version 1.2.0

Enable / disable a test version:
    $ nico-admin-cli machine-validation tests enable --test-id gpu_bandwidth --version 1.2.0
    $ nico-admin-cli machine-validation tests disable --test-id gpu_bandwidth --version 1.2.0

")]
pub(crate) enum Args {
    #[clap(about = "Show tests")]
    Show(ShowTestOptions),
    #[clap(about = "Verify a given test")]
    Verify(VerifyTestOptions),
    #[clap(about = "Add new test case")]
    Add(AddTestOptions),
    #[clap(about = "Update existing test case")]
    Update(UpdateTestOptions),
    #[clap(about = "Enabled a test")]
    Enable(EnableDisableTestOptions),
    #[clap(about = "Disable a test")]
    Disable(EnableDisableTestOptions),
}

#[derive(Parser, Debug)]
pub(crate) struct ShowTestOptions {
    #[clap(short, long, help = "Unique identification of the test")]
    pub(super) test_id: Option<String>,

    #[clap(short, long, help = "List of platforms")]
    pub(super) platforms: Vec<String>,

    #[clap(short, long, help = "List of contexts/tags")]
    pub(super) contexts: Vec<String>,

    #[clap(long, default_value = "false", help = "List unverfied tests also.")]
    pub(super) show_un_verfied: bool,
}

#[derive(Parser, Debug)]
pub(crate) struct VerifyTestOptions {
    #[clap(short, long, help = "Unique identification of the test")]
    pub(super) test_id: String,

    #[clap(short, long, help = "Version to be verify")]
    pub(super) version: String,
}

#[derive(Parser, Debug)]
pub(crate) struct EnableDisableTestOptions {
    #[clap(short, long, help = "Unique identification of the test")]
    pub(super) test_id: String,

    #[clap(short, long, help = "Version to be verify")]
    pub(super) version: String,
}

#[derive(Parser, Debug)]
pub(crate) struct UpdateTestOptions {
    #[clap(long, help = "Unique identification of the test")]
    pub(super) test_id: String,

    #[clap(long, help = "Version to be verify")]
    pub(super) version: String,

    #[clap(long, help = "List of contexts")]
    pub(super) contexts: Vec<String>,

    #[clap(long, help = "Container image name (must include @sha256:<digest>)",
           value_parser = parse_img_name)]
    pub(super) img_name: Option<String>,

    #[clap(long, help = "Run command using chroot in case of container")]
    pub(super) execute_in_host: Option<bool>,

    #[clap(long, help = "Container args", allow_hyphen_values = true)]
    pub(super) container_arg: Option<String>,

    #[clap(long, help = "Description")]
    pub(super) description: Option<String>,

    #[clap(long, help = "Command ")]
    pub(super) command: Option<String>,

    #[clap(long, help = "Command args", allow_hyphen_values = true)]
    pub(super) args: Option<String>,

    #[clap(long, help = "Command output error file ")]
    pub(super) extra_err_file: Option<String>,

    #[clap(long, help = "Command output file ")]
    pub(super) extra_output_file: Option<String>,

    #[clap(long, help = "External file")]
    pub(super) external_config_file: Option<String>,

    #[clap(long, help = "Pre condition")]
    pub(super) pre_condition: Option<String>,

    #[clap(long, help = "Command Timeout")]
    pub(super) timeout: Option<i64>,

    #[clap(long, help = "List of supported platforms")]
    pub(super) supported_platforms: Vec<String>,

    #[clap(long, help = "List of custom tags")]
    pub(super) custom_tags: Vec<String>,

    #[clap(long, help = "List of system components")]
    pub(super) components: Vec<String>,

    #[clap(long, help = "Enable the test")]
    pub(super) is_enabled: Option<bool>,
}

#[derive(Parser, Debug)]
pub(crate) struct AddTestOptions {
    #[clap(long, help = "Name of the test case")]
    pub(super) name: String,

    #[clap(long, help = "Command of the test case")]
    pub(super) command: String,

    #[clap(long, help = "Command args", allow_hyphen_values = true)]
    pub(super) args: String,

    #[clap(long, help = "List of contexts")]
    pub(super) contexts: Vec<String>,

    #[clap(long, help = "Container image name (must include @sha256:<digest>)",
           value_parser = parse_img_name)]
    pub(super) img_name: Option<String>,

    #[clap(long, help = "Run command using chroot in case of container")]
    pub(super) execute_in_host: Option<bool>,

    #[clap(long, help = "Container args", allow_hyphen_values = true)]
    pub(super) container_arg: Option<String>,

    #[clap(long, help = "Description")]
    pub(super) description: Option<String>,

    #[clap(long, help = "Command output error file ")]
    pub(super) extra_err_file: Option<String>,

    #[clap(long, help = "Command output file ")]
    pub(super) extra_output_file: Option<String>,

    #[clap(long, help = "External file")]
    pub(super) external_config_file: Option<String>,

    #[clap(long, help = "Pre condition")]
    pub(super) pre_condition: Option<String>,

    #[clap(long, help = "Command Timeout")]
    pub(super) timeout: Option<i64>,

    #[clap(long, help = "List of supported platforms")]
    pub(super) supported_platforms: Vec<String>,

    #[clap(long, help = "List of custom tags")]
    pub(super) custom_tags: Vec<String>,

    #[clap(long, help = "List of system components")]
    pub(super) components: Vec<String>,

    #[clap(long, help = "Enable the test")]
    pub(super) is_enabled: Option<bool>,

    #[clap(long, help = "Is read-only")]
    pub(super) read_only: Option<bool>,
}
