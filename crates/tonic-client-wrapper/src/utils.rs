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

use prost_types::FieldDescriptorProto;
use prost_types::field_descriptor_proto::{Label, Type};

pub(crate) fn base_type(proto_type: &str) -> Option<syn::Type> {
    match proto_type {
        ".google.protobuf.BoolValue" => Some(syn::parse_quote!(bool)),
        ".google.protobuf.BytesValue" => Some(syn::parse_quote!(::prost::alloc::vec::Vec<u8>)),
        ".google.protobuf.DoubleValue" => Some(syn::parse_quote!(f64)),
        ".google.protobuf.Empty" => Some(syn::parse_quote!(())),
        ".google.protobuf.FloatValue" => Some(syn::parse_quote!(f32)),
        ".google.protobuf.Int32Value" => Some(syn::parse_quote!(i32)),
        ".google.protobuf.Int64Value" => Some(syn::parse_quote!(i64)),
        ".google.protobuf.StringValue" => Some(syn::parse_quote!(::prost::alloc::string::String)),
        ".google.protobuf.UInt32Value" => Some(syn::parse_quote!(u32)),
        ".google.protobuf.UInt64Value" => Some(syn::parse_quote!(u64)),
        _ => None,
    }
}

pub(crate) fn resolve_field_primitive_type(field: &FieldDescriptorProto) -> Option<syn::Type> {
    match field.r#type() {
        Type::Float => Some(syn::parse_quote!(f32)),
        Type::Double => Some(syn::parse_quote!(f64)),
        Type::Uint32 | Type::Fixed32 => Some(syn::parse_quote!(u32)),
        Type::Uint64 | Type::Fixed64 => Some(syn::parse_quote!(u64)),
        Type::Int32 | Type::Sfixed32 | Type::Sint32 | Type::Enum => Some(syn::parse_quote!(i32)),
        Type::Int64 | Type::Sfixed64 | Type::Sint64 => Some(syn::parse_quote!(i64)),
        Type::Bool => Some(syn::parse_quote!(bool)),
        Type::String => Some(syn::parse_quote!(::prost::alloc::string::String)),
        Type::Bytes => Some(syn::parse_quote!(Vec<u8>)),
        _ => None,
    }
}

pub(crate) fn field_is_optional(field: &FieldDescriptorProto) -> bool {
    if field.proto3_optional.unwrap_or(false) {
        return true;
    }

    if field.label() != Label::Optional {
        return false;
    }

    matches!(field.r#type(), Type::Message)
}
