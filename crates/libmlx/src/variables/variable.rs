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

// src/variable.rs
// This file defines the MlxConfigVariable and corresponding
// builder used by build.rs. The variable contains the variable
// name, a description (which usually comes from show_confs), and
// the spec -- the spec says what type of variable it is, and any
// corresponding options depending on the type.

use ::rpc::errors::RpcDataConversionError;
use ::rpc::protos::mlx_device::MlxConfigVariable as MlxConfigVariablePb;
use serde::{Deserialize, Serialize};

use crate::variables::spec::MlxVariableSpec;
use crate::variables::value::{IntoMlxValue, MlxConfigValue, MlxValueError};

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct MlxConfigVariable {
    pub name: String,
    pub description: String,
    pub read_only: bool,
    pub spec: MlxVariableSpec,
}

// MlxConfigVariableBuilder is a builder for a
// new MlxConfigVariable, used by build.rs.
pub struct MlxConfigVariableBuilder {
    name: Option<String>,
    description: Option<String>,
    read_only: bool,
    spec: Option<MlxVariableSpec>,
}

impl Default for MlxConfigVariableBuilder {
    fn default() -> Self {
        Self::new()
    }
}

impl MlxConfigVariableBuilder {
    pub fn new() -> Self {
        Self {
            name: None,
            description: None,
            read_only: false,
            spec: None,
        }
    }

    pub fn name<T: Into<String>>(mut self, name: T) -> Self {
        self.name = Some(name.into());
        self
    }

    pub fn description<T: Into<String>>(mut self, description: T) -> Self {
        self.description = Some(description.into());
        self
    }

    pub fn read_only(mut self, read_only: bool) -> Self {
        self.read_only = read_only;
        self
    }

    pub fn spec(mut self, spec: MlxVariableSpec) -> Self {
        self.spec = Some(spec);
        self
    }

    pub fn build(self) -> MlxConfigVariable {
        MlxConfigVariable {
            name: self.name.expect("name is required"),
            description: self.description.expect("description is required"),
            read_only: self.read_only,
            spec: self.spec.expect("spec is required"),
        }
    }
}

impl MlxConfigVariable {
    pub fn builder() -> MlxConfigVariableBuilder {
        MlxConfigVariableBuilder::new()
    }

    // with creates a value for this variable,
    // leveraging our IntoMlxValue trait.
    pub fn with<T: IntoMlxValue>(&self, value: T) -> Result<MlxConfigValue, MlxValueError> {
        let mlx_value = value.into_mlx_value_for_spec(&self.spec)?;
        MlxConfigValue::new(self.clone(), mlx_value)
    }

    // spec returns the underlying spec for the variable.
    pub fn spec(&self) -> &MlxVariableSpec {
        &self.spec
    }
}

// MlxConfigVariable conversions
impl From<MlxConfigVariable> for MlxConfigVariablePb {
    fn from(var: MlxConfigVariable) -> Self {
        MlxConfigVariablePb {
            name: var.name,
            description: var.description,
            read_only: var.read_only,
            spec: Some(var.spec.into()),
        }
    }
}

impl TryFrom<MlxConfigVariablePb> for MlxConfigVariable {
    type Error = RpcDataConversionError;

    fn try_from(pb: MlxConfigVariablePb) -> Result<Self, Self::Error> {
        Ok(MlxConfigVariable {
            name: pb.name,
            description: pb.description,
            read_only: pb.read_only,
            spec: pb
                .spec
                .ok_or(RpcDataConversionError::MissingArgument("spec"))?
                .try_into()?,
        })
    }
}
