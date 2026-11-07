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

use super::args::Args;
use crate::credential::NMX_M_UNSUPPORTED_MESSAGE;
use crate::errors::{CarbideCliError, CarbideCliResult};

pub(super) fn add_nmxm(_data: Args) -> CarbideCliResult<()> {
    Err(CarbideCliError::UnsupportedOperation(
        NMX_M_UNSUPPORTED_MESSAGE,
    ))
}

#[cfg(test)]
mod tests {
    use clap::Parser;

    use super::*;

    #[test]
    fn returns_unsupported_without_creating_a_credential() {
        let args = Args::try_parse_from(["add-nmx-m"]).expect("arguments should parse");
        let error =
            add_nmxm(args).expect_err("the compatibility command must not create a credential");

        assert_eq!(
            error.to_string(),
            format!("unsupported operation: {NMX_M_UNSUPPORTED_MESSAGE}")
        );
    }
}
