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

use db::{AnnotatedSqlxError, DatabaseError};

#[derive(thiserror::Error, Debug)]
pub enum DpaManagerError {
    #[error("database error: {0}")]
    Database(#[from] DatabaseError),
    #[error(transparent)]
    Sqlx(#[from] AnnotatedSqlxError),
    #[error("argument is invalid: {0}")]
    InvalidArgument(String),
    #[error("generic error: {0}")]
    Generic(#[from] eyre::ErrReport),
    #[error("internal error: {message}")]
    Internal { message: String },
}

pub type DpaManagerResult<T> = Result<T, DpaManagerError>;
