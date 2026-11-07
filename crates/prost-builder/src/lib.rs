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

//! This proc_macro gives possibility to derive Builder implementation
//! for prost Message structures.
//!
//! To use it, derive `Builder` on a prost-generated message type.
//! For example:
//! ```rust,ignore
//! #[derive(carbide_prost_builder::Builder)]
//! pub struct DhcpDiscovery {
//!     pub mac_address: ::prost::alloc::string::String,
//!     pub relay_address: ::prost::alloc::string::String,
//!     pub vendor_string: ::core::option::Option<::prost::alloc::string::String>,
//!     pub link_address: ::core::option::Option<::prost::alloc::string::String>,
//!     pub circuit_id: ::core::option::Option<::prost::alloc::string::String>,
//!     pub remote_id: ::core::option::Option<::prost::alloc::string::String>,
//! }
//! ```
//!
//! All required fields will be requested in fn `DhcpDiscovery::builder` constructor.
//!
//! For all optional field of type `Type` two methods will be generated:
//! 1. `pub fn <name>(self, impl Into<Type>)` - to set corresponding field from anything convertable to Type
//! 2. `pub fn set_<name>(self, Option<Type>)` - to set convertable field as is.
//!
//! As result you can use it as following:
//!
//! To produce tonic request of corresponding type:
//! ```rust,ignore
//! DhcpDiscovery::builder(mac_address, relay_address)
//!    .vendor_string("Some vendor")
//!    .tonic_request()
//! ```
//!
//! To produce prost::Message of corresponding type:
//! ```rust,ignore
//! DhcpDiscovery::builder(mac_address, relay_address)
//!    .vendor_string("Some vendor")
//!    .rpc()
//! ```
use proc_macro::TokenStream;
use proc_macro2::{Span, TokenStream as TokenStream2};
use quote::quote;
use syn::{Data, DeriveInput, Fields, GenericArgument, Ident, PathArguments, Type};

#[proc_macro_derive(Builder, attributes(implement, transparent, capability, permissive))]
pub fn derive_builder(input: TokenStream) -> TokenStream {
    let derive = syn::parse_macro_input!(input as DeriveInput);
    let name = derive.ident;
    type FieldData = (Ident, Type);
    let fields = match derive.data {
        Data::Struct(ref ds) => match &ds.fields {
            Fields::Named(named) => named
                .named
                .iter()
                .map(|f| (f.ident.clone().unwrap(), f.ty.clone()))
                .collect::<Vec<FieldData>>(),
            _ => panic!("#[derive(Builder)] requires a normal struct"),
        },
        _ => panic!("#[derive(Builder)] can only be used on structs"),
    };

    let (required_fields, optional_fields): (Vec<_>, Vec<_>) =
        fields.into_iter().partition(|(_, tp)| match tp {
            Type::Path(typepath) => typepath
                .path
                .segments
                .last()
                .is_none_or(|segment| segment.ident != "Option"),
            _ => false,
        });

    let builder_name = Ident::new(format!("{name}Builder").as_str(), Span::call_site());
    let mut args = TokenStream2::new();
    let mut builder_fields = TokenStream2::new();
    let mut builder_required_ctor_impl = TokenStream2::new();
    let mut message_fields_move = TokenStream2::new();
    for (name, tp) in required_fields {
        if is_string(&tp) {
            // Special treatment for String. We want reduce noise in
            // tests...
            args.extend(quote! { #name: impl ::std::fmt::Display, });
            builder_required_ctor_impl.extend(quote! { #name: #name.to_string(), });
        } else {
            args.extend(quote! { #name: #tp, });
            builder_required_ctor_impl.extend(quote! { #name, });
        }
        builder_fields.extend(quote! { #name: #tp, });
        message_fields_move.extend(quote! { #name: self.#name, });
    }
    let mut optional_ctor_impl = TokenStream2::new();
    let mut optional_methods_impl = TokenStream2::new();
    for (name, tp) in optional_fields {
        optional_ctor_impl.extend(quote! { #name: None, });
        builder_fields.extend(quote! { #name: #tp, });
        message_fields_move.extend(quote! { #name: self.#name, });
        let tp = match tp {
            Type::Path(typepath) => typepath,
            _ => panic!("Type::Path is expected"),
        };
        let last_segment = tp.path.segments.last().expect("must be Option");
        let ftype = match &last_segment.arguments {
            PathArguments::AngleBracketed(ab) => {
                let arg = ab
                    .args
                    .first()
                    .expect("Option must have type as first argument");
                match arg {
                    GenericArgument::Type(ftype) => ftype,
                    _ => panic!("Option expect first generic argument as Type"),
                }
            }
            _ => {
                panic!("Option must have angle-bracked argument");
            }
        };
        if is_string(ftype) {
            optional_methods_impl.extend(quote! {
                pub fn #name(mut self, #name: impl ::std::fmt::Display) -> Self {
                    self.#name = Some(#name.to_string());
                    self
                }
            });
        } else {
            optional_methods_impl.extend(quote! {
                pub fn #name(mut self, #name: impl Into<#ftype>) -> Self {
                    self.#name = Some(#name.into());
                    self
                }
            });
        }
        let setter_name = Ident::new(format!("set_{name}").as_str(), Span::call_site());
        optional_methods_impl.extend(quote! {
            pub fn #setter_name(mut self, #name: #tp) -> Self {
                self.#name = #name;
                self
            }
        });
    }

    TokenStream::from(quote! {
        pub struct #builder_name {
            #builder_fields
        }

        impl #name {
            pub fn builder(#args) -> #builder_name {
                #builder_name {
                    #builder_required_ctor_impl
                    #optional_ctor_impl
                }
            }
        }

        impl #builder_name {
            #optional_methods_impl

            pub fn tonic_request(self) -> ::tonic::Request<#name> {
                ::tonic::Request::new(self.rpc())
            }

            pub fn rpc(self) -> #name {
                #name {
                    #message_fields_move
                }
            }
        }

        impl From<#builder_name> for #name {
            fn from(v: #builder_name) -> Self {
                v.rpc()
            }
        }
    })
}

fn is_string(tp: &Type) -> bool {
    match tp {
        Type::Path(typepath) => typepath
            .path
            .segments
            .last()
            .is_some_and(|segment| segment.ident == "String"),
        _ => false,
    }
}
