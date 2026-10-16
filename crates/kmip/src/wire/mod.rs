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
//! The KMIP 1.4 TTLV message model for the scoped subset this crate speaks.
//!
//! Types follow the conventions of the [`kmip_ttlv`] serde (de)serializer:
//!
//! - A brace struct renamed to its tag (`#[serde(rename = "0x42xxxx")]`) is a
//!   TTLV Structure. Serialization ignores field names and writes each field's
//!   own tag; deserialization matches items positionally, in specification
//!   order, and uses the field's renamed tag only to decide that an optional
//!   item is absent. Fields are therefore declared in specification order and
//!   every field type carries its own tag.
//! - A newtype renamed `Transparent:0x42xxxx` is one primitive item.
//! - An enum renamed to its tag with hex-renamed unit variants is a TTLV
//!   Enumeration; the `Other` variant absorbs values this crate does not model.
//! - A deserialize-only enum whose variants are renamed
//!   `if 0x42005C==0x........` is selected by the Operation value seen earlier
//!   in the same message.
//!
//! Tag codes and enumeration values are from the OASIS KMIP 1.4 specification,
//! section 9.1.3.

/// A TTLV item carrying one primitive value under a fixed tag.
macro_rules! ttlv_newtype {
    ($(#[$meta:meta])* $vis:vis struct $name:ident($inner:ty) = $tag:literal;) => {
        $(#[$meta])*
        #[derive(Clone, Debug, PartialEq, Eq, ::serde::Serialize, ::serde::Deserialize)]
        #[serde(rename = $tag)]
        $vis struct $name($vis $inner);
    };
}

/// A TTLV Byte String item under a fixed tag.
macro_rules! ttlv_bytes {
    ($(#[$meta:meta])* $vis:vis struct $name:ident = $tag:literal;) => {
        $(#[$meta])*
        #[derive(Clone, Debug, PartialEq, Eq, ::serde::Serialize, ::serde::Deserialize)]
        #[serde(rename = $tag)]
        $vis struct $name(#[serde(with = "serde_bytes")] $vis Vec<u8>);
    };
}

/// A KMIP Enumeration: the tag on the enum, the 32-bit value on each variant,
/// and `Other` for values this crate does not model. `Display` renders the
/// specification name of the variant.
macro_rules! kmip_enumeration {
    ($(#[$meta:meta])* $vis:vis enum $name:ident = $tag:literal {
        $($variant:ident = $value:literal),+ $(,)?
    }) => {
        $(#[$meta])*
        #[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, ::serde::Serialize, ::serde::Deserialize)]
        #[serde(rename = $tag)]
        $vis enum $name {
            $( #[serde(rename = $value)] $variant, )+
            /// A value this crate does not model: a vendor extension or a
            /// newer specification. Never sent by this crate.
            #[serde(other)]
            Other,
        }

        impl ::std::fmt::Display for $name {
            fn fmt(&self, f: &mut ::std::fmt::Formatter<'_>) -> ::std::fmt::Result {
                f.write_str(match self {
                    $( Self::$variant => stringify!($variant), )+
                    Self::Other => "Other",
                })
            }
        }
    };
}

mod common;
mod request;
mod response;

pub(crate) use common::{
    Attribute, AttributeName, AttributeValue, AuthenticatedEncryptionTag, BatchCount,
    BlockCipherMode, CryptographicParameters, Data, IvCounterNonce, IvLength, MaximumResponseSize,
    PaddingMethod, QueryFunction, RandomIv, TagLength, UniqueIdentifier,
};
pub use common::{CryptographicAlgorithm, Operation, ResultReason, ResultStatus, State};
#[cfg(any(test, feature = "test-support"))]
pub(crate) use common::{
    ObjectType, ProtocolVersion, ProtocolVersionMajor, ProtocolVersionMinor, ResultMessage,
    ServerInformation, TimeStamp, VendorIdentification,
};
#[cfg(any(test, feature = "test-support"))]
pub(crate) use request::RequestPayload;
pub(crate) use request::{
    DecryptRequestPayload, DiscoverVersionsRequestPayload, EncryptRequestPayload,
    GetAttributesRequestPayload, PROTOCOL_VERSION_1_4, QueryRequestPayload, RequestBatchItem,
    RequestHeader, RequestMessage,
};
#[cfg(any(test, feature = "test-support"))]
pub(crate) use response::ResponseHeader;
pub(crate) use response::{
    DecryptResponsePayload, DiscoverVersionsResponsePayload, EncryptResponsePayload,
    GetAttributesResponsePayload, IgnoredPayload, QueryResponsePayload, ResponseBatchItem,
    ResponseMessage,
};
