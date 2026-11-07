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

// src/client/handlers.rs
// Message handler abstractions and adapters for type-safe MQTT message processing.
//
// Provides type erasure and adapter patterns to enable storing different
// message handler types in the same collection while maintaining type safety
// for caller registrations.

use std::marker::PhantomData;
use std::sync::Arc;

use async_trait::async_trait;

use crate::client::MqtteaClient;
use crate::errors::MqtteaClientError;
use crate::traits::MessageHandler;

// ErasedHandler enables storing handlers for different message types in the
// same collection: type-erased function that takes client, raw payload bytes
// and topic -- returns a future.
pub type ErasedHandler = Box<
    dyn Fn(
            Arc<MqtteaClient>,
            Vec<u8>,
            String,
        ) -> std::pin::Pin<
            Box<dyn std::future::Future<Output = Result<(), MqtteaClientError>> + Send>,
        > + Send
        + Sync,
>;

// ClosureAdapter wraps user-provided closures to implement the MessageHandler
// trait. Enables convenient registration of closure-based message handlers with
// type inference.
pub struct ClosureAdapter<T, F, Fut>
where
    T: Send + Sync + 'static,
    F: Fn(Arc<MqtteaClient>, T, String) -> Fut + Send + Sync + 'static,
    Fut: std::future::Future<Output = ()> + Send + Sync + 'static,
{
    // closure is the user-provided message processing function
    pub closure: F,
    // _phantom ensures the type parameters are used (required by Rust's
    // type system).
    pub _phantom: PhantomData<(T, Fut)>,
}

impl<T, F, Fut> ClosureAdapter<T, F, Fut>
where
    T: Send + Sync + 'static,
    F: Fn(Arc<MqtteaClient>, T, String) -> Fut + Send + Sync + 'static,
    Fut: std::future::Future<Output = ()> + Send + Sync + 'static,
{
    // new creates a new ClosureAdapter wrapping the provided closure,
    // which enables converting closures into the MessageHandler trait.
    pub fn new(closure: F) -> Self {
        Self {
            closure,
            _phantom: PhantomData,
        }
    }
}

#[async_trait]
impl<T, F, Fut> MessageHandler<T> for ClosureAdapter<T, F, Fut>
where
    T: Send + Sync + 'static,
    F: Fn(Arc<MqtteaClient>, T, String) -> Fut + Send + Sync + 'static,
    Fut: std::future::Future<Output = ()> + Send + Sync + 'static,
{
    // handle processes the message by calling the wrapped closure,
    // and is what enablles integration between closures and
    // trait-based handlers.
    async fn handle(&self, client: Arc<MqtteaClient>, message: T, topic: String) {
        (self.closure)(client, message, topic).await;
    }
}
