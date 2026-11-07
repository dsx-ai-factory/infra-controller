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
use std::fs::File;
use std::io;
use std::io::Read;
use std::path::Path;

use crate::forge_resolver::resolver::ResolverError;

pub mod resolver;

pub fn read_resolv_conf<P: AsRef<Path>>(path: P) -> Result<resolv_conf::Config, ResolverError> {
    let mut data = String::new();
    let mut file = File::open(&path)
        .map_err(|_| {
            io::Error::other(eyre::eyre!(
                "unable to read resolv.conf at {:?}",
                path.as_ref().file_name()
            ))
        })
        .map_err(|e| ResolverError::CouldNotReadResolvConf {
            path: path.as_ref().to_path_buf(),
            error: e,
        })?;

    file.read_to_string(&mut data)
        .map_err(|e| ResolverError::CouldNotReadResolvConf {
            path: path.as_ref().to_path_buf(),
            error: e,
        })?;

    resolv_conf::Config::parse(&data).map_err(|err| ResolverError::CouldNotParseResolvConf {
        path: path.as_ref().to_path_buf(),
        error: err,
    })
}
