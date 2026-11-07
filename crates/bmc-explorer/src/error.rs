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

use std::error::Error as StdError;
use std::fmt;

use nv_redfish::{Bmc, Error as NvRedfishError};

pub enum Error<B: Bmc> {
    NvRedfish {
        context: &'static str,
        err: NvRedfishError<B>,
    },
    BmcNotProvided(&'static str),
    InvalidValue(String),
}

impl<B: Bmc> fmt::Display for Error<B> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::NvRedfish { context, err } => write!(f, "redfish error in {context}: {err}"),
            Self::BmcNotProvided(what) => write!(f, "BMC has not provided {what}"),
            Self::InvalidValue(what) => write!(f, "Invalid value {what}"),
        }
    }
}

// Need to implement Debug manually because Derive requires B to
// implement Debug.
impl<B: Bmc> fmt::Debug for Error<B> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::NvRedfish { context, err } => f
                .debug_struct("NvRedfish")
                .field("context", context)
                .field("err", &format_args!("{err}"))
                .finish(),
            Self::BmcNotProvided(what) => f.debug_tuple("BmcNotProvided").field(what).finish(),
            Self::InvalidValue(what) => f.debug_tuple("InvalidValue").field(what).finish(),
        }
    }
}

impl<B> StdError for Error<B>
where
    B: Bmc,
    NvRedfishError<B>: 'static,
{
    fn source(&self) -> Option<&(dyn StdError + 'static)> {
        match self {
            Self::NvRedfish { err, .. } => Some(err),
            Self::BmcNotProvided(_) | Self::InvalidValue(_) => None,
        }
    }
}

impl<B: Bmc> Error<B> {
    pub(crate) fn nv_redfish(context: &'static str) -> impl Fn(NvRedfishError<B>) -> Self {
        move |err| Self::NvRedfish { context, err }
    }

    pub(crate) fn bmc_not_provided(what: &'static str) -> impl Fn() -> Self {
        move || Self::BmcNotProvided(what)
    }
}
