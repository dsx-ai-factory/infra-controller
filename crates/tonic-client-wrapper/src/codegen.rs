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

//! Descriptor-driven code generation for tonic client wrappers.
//!
//! This module is a downstream code-generation backend: it does not parse
//! `.proto` files or invoke `protoc`. [`CodeGenerator`] borrows a complete
//! [`FileDescriptorSet`] so it can resolve request and response types across
//! imports. [`Config::root_files`] independently selects the protobuf files
//! whose services receive wrapper methods and convenience converters.

use std::borrow::Cow;
use std::cmp::Ordering;
use std::collections::{HashMap, HashSet};
use std::fs;
use std::path::Path;

use carbide_proto_compiler::ExternPathSearchIndex;
use heck::{ToSnakeCase, ToUpperCamelCase};
use proc_macro2::{LexError, TokenStream};
use prost_types::field_descriptor_proto::Label;
use prost_types::{FileDescriptorProto, FileDescriptorSet, MethodDescriptorProto};
use quote::{TokenStreamExt, quote};

use crate::utils::{base_type, field_is_optional, resolve_field_primitive_type};

#[derive(thiserror::Error, Debug)]
pub enum Error {
    #[error("invalid token for {target}: {error}")]
    InvalidToken {
        target: String,
        error: proc_macro2::LexError,
    },
    #[error(transparent)]
    Lex(#[from] LexError),
    #[error("invalid protobuf type: {0}")]
    InvalidProtobufType(String),
    #[error("root protobuf file not found in descriptor set: {0}")]
    MissingRootFile(String),
    #[error("duplicate root protobuf file: {0}")]
    DuplicateRootFile(String),
    #[error(transparent)]
    Io(#[from] std::io::Error),
    #[error("syntax error in generated code: {0}")]
    Syntax(#[from] syn::Error),
}

pub type Result<T> = std::result::Result<T, Error>;

/// Configures code generation of a tonic client wrapper.
pub struct Config<'a> {
    /// The name of the generated tonic client wrapper.
    pub wrapper_name: String,
    /// The fully qualified type of the tonic client this wrapper will call.
    pub inner_rpc_client_type: String,

    /// The module path of the generated types within your crate, not including the service name,
    /// relative to the crate root (do not include `crate::`.) This is used to fully qualify the
    /// type names in the generated wrapper methods.
    ///
    /// For example, if your service `my_service` is generated in `crate::rpc::protos::my_service`,
    /// use `"rpc::protos"` here.
    pub generated_types_path_within_crate: String,

    /// Service-selection roots, not protobuf compilation inputs.
    ///
    /// Each value must exactly match a [`FileDescriptorProto::name`] in the
    /// shared descriptor set, such as `forge.proto`. The generator uses the
    /// complete descriptor set for cross-file type lookup, but emits services
    /// and converters only for these files.
    pub root_files: Vec<String>,

    /// List of protobuf types to override with specific Rust types. This should mirror any types
    /// customized through `tonic_prost_build::Builder::extern_path` so the generated code matches.
    pub extern_paths: ExternPathSearchIndex<'a>,
}

pub type ProtobufType = &'static str;
pub type RustType = syn::Type;

/// A wrapper backend borrowing a complete protobuf schema.
///
/// The borrow keeps selected files and the cross-file message index tied to the
/// source [`FileDescriptorSet`] for the generator's lifetime.
pub struct CodeGenerator<'a> {
    inner_rpc_client_type: TokenStream,
    wrapper_name: TokenStream,
    root_files: Vec<&'a FileDescriptorProto>,
    generated_types_path_within_crate: TokenStream,
    message_types: HashMap<String, MessageWithPackage<'a>>,
    extern_paths: ExternPathSearchIndex<'a>,
}

impl<'a> CodeGenerator<'a> {
    /// Creates a backend from wrapper settings and a complete descriptor set.
    ///
    /// [`Config::root_files`] chooses service roots within `descriptor_set`;
    /// imported and otherwise unselected files remain available for resolving
    /// method types.
    pub fn new(config: Config<'a>, descriptor_set: &'a FileDescriptorSet) -> Result<Self> {
        let inner_rpc_client_type =
            config
                .inner_rpc_client_type
                .parse()
                .map_err(|error| Error::InvalidToken {
                    target: "inner_rpc_client_type".to_string(),
                    error,
                })?;
        let wrapper_name = config
            .wrapper_name
            .parse()
            .map_err(|error| Error::InvalidToken {
                target: "wrapper_name".to_string(),
                error,
            })?;
        let generated_types_path_within_crate = config
            .generated_types_path_within_crate
            .parse()
            .map_err(|error| Error::InvalidToken {
                target: "generated_types_path_within_crate".to_string(),
                error,
            })?;

        // Resolve service-selection roots by their logical descriptor names.
        let mut selected_root_names = HashSet::new();
        let root_files = config
            .root_files
            .iter()
            .map(|root_file| {
                if !selected_root_names.insert(root_file.as_str()) {
                    return Err(Error::DuplicateRootFile(root_file.clone()));
                }

                let mut matches = descriptor_set
                    .file
                    .iter()
                    .filter(|file| file.name() == root_file.as_str());
                let root = matches
                    .next()
                    .ok_or_else(|| Error::MissingRootFile(root_file.clone()))?;
                if matches.next().is_some() {
                    return Err(Error::DuplicateRootFile(root_file.clone()));
                }
                Ok(root)
            })
            .collect::<Result<Vec<_>>>()?;

        // Index messages from the complete descriptor set, not only the service
        // roots, so methods may use types declared in imported files.
        let message_types: HashMap<String, MessageWithPackage<'_>> = descriptor_set
            .file
            .iter()
            .flat_map(|fd| {
                fd.message_type.iter().map(|message| {
                    let message_with_package = MessageWithPackage {
                        message,
                        package: fd.package.as_deref(),
                    };
                    (message_with_package.qualified_name(), message_with_package)
                })
            })
            .collect();

        Ok(Self {
            inner_rpc_client_type,
            wrapper_name,
            root_files,
            generated_types_path_within_crate,
            message_types,
            extern_paths: config.extern_paths,
        })
    }

    /// Writes wrapper methods for services in the selected root files.
    pub fn write_rpc_client_wrapper<P: AsRef<Path>>(&self, out: P) -> Result<()> {
        let mut wrapper_methods = TokenStream::new();

        let mut labeled_methods = Vec::new();
        for fd in &self.root_files {
            for svc in &fd.service {
                let service_label = svc.name().to_snake_case();
                for method in &svc.method {
                    labeled_methods.push((service_label.clone(), method));
                }
            }
        }
        labeled_methods
            .iter()
            .map(|(service_label, m)| self.make_rpc_wrapper_method(service_label, m))
            .collect::<Result<Vec<_>>>()? // fail if any of the wrappers failed
            .into_iter()
            .for_each(|m| wrapper_methods.append_all(m));

        let inner_rpc_client_type = &self.inner_rpc_client_type;
        let wrapper_name = &self.wrapper_name;

        let file = quote! {
            use std::ops::Deref;

            #[derive(Clone, Debug)]
            pub struct #wrapper_name {
                inner: std::sync::Arc<Inner>
            }

            #[derive(Debug)]
            struct Inner {
                connection_provider: Box<dyn ::tonic_client_wrapper::ConnectionProvider<#inner_rpc_client_type>>,
                connection: ::tokio::sync::Mutex<Option<InnerConnection>>,
            }

            #[derive(Debug)]
            struct InnerConnection {
                client: #inner_rpc_client_type,
                created: std::time::SystemTime,
            }

            impl #wrapper_name {
                pub fn build<P: ::tonic_client_wrapper::ConnectionProvider<#inner_rpc_client_type>>(connection_provider: P) -> Self {
                    let inner = Inner {
                        connection_provider: Box::new(connection_provider),
                        connection: tokio::sync::Mutex::new(None),
                    };

                    Self {
                        inner: std::sync::Arc::new(inner),
                    }
                }

                pub async fn connection(&self) -> std::result::Result<#inner_rpc_client_type, tonic::Status> {
                    let mut guard = self.inner.connection.lock().await;
                    if let Some(connection) = guard.deref() {
                        if self.inner.connection_provider.connection_is_stale(connection.created).await {
                            guard.take();
                        }
                    }

                    match guard.deref() {
                        Some(connection) => Ok(connection.client.clone()),
                        None => {
                            let client = self.inner.connection_provider.provide_connection().await?;
                            guard.replace(InnerConnection {
                                client: client.clone(),
                                created: std::time::SystemTime::now(),
                            });
                            Ok(client)
                        }
                    }
                }

                pub fn url(&self) -> &str {
                    self.inner.connection_provider.connection_url()
                }

                #wrapper_methods
            }
        };

        write_token_stream_if_not_up_to_date(file, &out)?;
        Ok(())
    }

    /// Writes convenience `From<...>` implementations for request types used by
    /// gRPC methods in the selected root files.
    ///
    /// A converter will be written for a type if:
    ///
    /// - It's used as the input for a gRPC method
    /// - It has zero or one fields.
    ///
    /// If the type has zero fields, a converter will be generated from the empty tuple (`()`).
    ///
    /// If the type has one field, a converter will be generated from any type which is convertible
    /// to that single field (i.e., `From<T: Into<SomeField>>`).
    pub fn write_rpc_convenience_converters<P: AsRef<Path>>(&self, out: P) -> Result<()> {
        // Collect inputs only from selected services, then resolve them through
        // the complete message index. Deduplication avoids duplicate impls when
        // several methods use the same request type.
        let method_inputs_type_strings: HashSet<&String> = self
            .root_files
            .iter()
            .flat_map(|fd| &fd.service)
            .flat_map(|service| &service.method)
            .filter_map(|method| method.input_type.as_ref())
            .collect();

        let mut converters = TokenStream::new();
        // Look up each input type in self.message_types to get its metadata
        let mut sorted_messages = method_inputs_type_strings
            .into_iter()
            .filter_map(|t| self.message_types.get(t))
            .collect::<Vec<_>>();

        sorted_messages.sort_by(|a, b| match a.package.cmp(&b.package) {
            Ordering::Equal => a.message.name.cmp(&b.message.name),
            other => other,
        });
        for message_and_package in sorted_messages {
            // Generate a convenience converter for each one.
            converters.append_all(self.make_convenience_converter(message_and_package)?);
        }

        write_token_stream_if_not_up_to_date(converters, &out)?;
        Ok(())
    }

    fn make_convenience_converter(
        &self,
        message_with_package: &MessageWithPackage<'_>,
    ) -> Result<Option<TokenStream>> {
        let message = &message_with_package.message;
        let qualified_name = message_with_package.qualified_name();

        if message.field.len() > 1 {
            // We only make convenience converters for messages with 1 or 0 fields
            return Ok(None);
        }

        if base_type(&qualified_name).is_some() {
            // Except we can't create convenience converters for primitives
            return Ok(None);
        }

        if self.extern_paths.contains_key(qualified_name.as_str()) {
            // Nor do we create them for extern types
            return Ok(None);
        }

        // No fields in the message means we can convert from `()`
        let Some(field) = message.field.first() else {
            return Ok(Some(
                self.make_convenience_converter_from_void(message_with_package)?,
            ));
        };

        let is_repeated = field.label.is_some_and(|l| l == Label::Repeated as i32);
        let is_optional = field_is_optional(field);

        // Define the template values used in the generated code...

        // The type of the message itself being converted *to*
        let message_type =
            self.convert_protobuf_type_to_rust_type(&message_with_package.qualified_name())?;

        // The name of the single field we're going to be populating from the From<> type
        let field_name = if let Some(oneof_index) = field.oneof_index {
            message.oneof_decl[oneof_index as usize].name()
        } else {
            field.name()
        }
        .to_snake_case()
        .parse::<TokenStream>()?;

        // The type of the single field
        let field_type: TokenStream = {
            let typename = if let Some(t) = resolve_field_primitive_type(field) {
                Cow::Owned(t)
            } else if let Some(type_name) = &field.type_name {
                self.convert_protobuf_type_to_rust_type(type_name)?
            } else {
                // This might be a primitive type we don't know about.
                return Ok(None);
            };

            if is_repeated {
                quote! { Vec<#typename> }
            } else {
                quote! { #typename }
            }
        };

        // The value we're setting the single field to
        let value = if field.oneof_index.is_some() && field.proto3_optional.is_none_or(|o| !o) {
            // If it's a `oneof`, it's going to be seen by rust as an Enum with an associated
            // value. The enum package is going to be the message name in snake-case, the enum's
            // type is the name of the oneof field, and each arm of the enum is going to be one
            // of the oneof arms (which we've ensured there is only one.)
            //
            // Note about proto3_optional: If proto3_optional is set, it generally means that
            // this is a "synthetic" oneof, which is a sort of hack used by prost-types to make
            // the field show up as optional for proto3. We *don't* want to treat these cases as
            // enums, because they don't show up in rust as real enums.
            self.convert_protobuf_type_to_rust_type(&format!(
                "{}.{}",
                message_with_package.qualified_name(), // this will be snake_cased
                message.oneof_decl[field.oneof_index() as usize].name()  // this will be CamelCased
            ))
            .and_then(|s| {
                let name: syn::TypePath = syn::parse_str(&field.name().to_upper_camel_case())?;
                Ok(quote! { Some(#s::#name(t.into())) })
            })?
        } else if is_optional {
            quote! { Some(t.into()) }
        } else {
            quote! { t.into() }
        };

        Ok(Some(quote! {
            impl<T: Into<#field_type>> From<T> for #message_type {
                fn from(t: T) -> Self {
                    Self {
                        #field_name: #value
                    }
                }
            }
        }))
    }

    fn make_convenience_converter_from_void(
        &self,
        message_with_package: &MessageWithPackage<'_>,
    ) -> Result<TokenStream> {
        let message_type =
            self.convert_protobuf_type_to_rust_type(&message_with_package.qualified_name())?;

        Ok(quote! {
            impl From<()> for #message_type {
                fn from(_: ()) -> Self {
                    Self {}
                }
            }
        })
    }

    fn make_rpc_wrapper_method(
        &self,
        service_label: &str,
        method: &MethodDescriptorProto,
    ) -> Result<TokenStream> {
        let method_name: TokenStream = method.name().to_snake_case().parse()?;
        // Compile-time literals from the proto: the bounded `backend` and
        // `operation` labels for the outbound-call RED metric.
        let operation_label = method.name().to_snake_case();
        let input_type = self.convert_protobuf_type_to_rust_type(method.input_type())?;
        let output_type = self.convert_protobuf_type_to_rust_type(method.output_type())?;
        let input_is_unit =
            matches!(input_type.as_ref(), syn::Type::Tuple(tuple) if tuple.elems.is_empty());

        let is_client_streaming = method.client_streaming.unwrap_or(false);
        let is_server_streaming = method.server_streaming.unwrap_or(false);

        match (is_client_streaming, is_server_streaming) {
            (true, true) => {
                // Bidirectional streaming.
                Ok(quote! {
                    pub async fn #method_name<S>(&self, request: S) -> Result<tonic::Response<tonic::codec::Streaming<#output_type>>, tonic::Status>
                    where
                        S: tonic::IntoStreamingRequest<Message = #input_type>,
                    {
                        ::carbide_instrument::red::instrumented(#service_label, #operation_label, async move {
                            self.connection().await?.#method_name(request).await
                        }).await
                    }
                })
            }
            (true, false) => {
                // Client streaming.
                Ok(quote! {
                    pub async fn #method_name<S>(&self, request: S) -> Result<#output_type, tonic::Status>
                    where
                        S: tonic::IntoStreamingRequest<Message = #input_type>,
                    {
                        ::carbide_instrument::red::instrumented(#service_label, #operation_label, async move {
                            Ok(self
                                .connection()
                                .await?
                                .#method_name(request)
                                .await?
                                .into_inner())
                        }).await
                    }
                })
            }
            (false, true) => {
                // Server streaming.
                let token_stream = if input_is_unit {
                    quote! {
                        pub async fn #method_name(&self) -> Result<tonic::codec::Streaming<#output_type>, tonic::Status> {
                                ::carbide_instrument::red::instrumented(#service_label, #operation_label, async move {
                                    Ok(self
                                        .connection()
                                        .await?
                                        .#method_name(tonic::Request::new(()))
                                        .await?
                                        .into_inner())
                                }).await
                            }
                    }
                } else {
                    let has_zero_fields = method
                        .input_type
                        .as_ref()
                        .and_then(|t| self.message_types.get(t))
                        .is_some_and(|t| t.message.field.is_empty());

                    if has_zero_fields {
                        quote! {
                            pub async fn #method_name(&self) -> Result<tonic::codec::Streaming<#output_type>, tonic::Status> {
                                    ::carbide_instrument::red::instrumented(#service_label, #operation_label, async move {
                                        Ok(self
                                            .connection()
                                            .await?
                                            .#method_name(tonic::Request::new(#input_type {}))
                                            .await?
                                            .into_inner())
                                    }).await
                                }
                        }
                    } else {
                        quote! {
                            pub async fn #method_name<T: Into<#input_type>>(&self, request: T) -> Result<tonic::codec::Streaming<#output_type>, tonic::Status> {
                                    ::carbide_instrument::red::instrumented(#service_label, #operation_label, async move {
                                        Ok(self
                                            .connection()
                                            .await?
                                            .#method_name(tonic::Request::new(request.into()))
                                            .await?
                                            .into_inner())
                                    }).await
                                }
                        }
                    }
                };
                Ok(token_stream)
            }
            (false, false) => {
                // Unary - your existing code.
                let token_stream = if input_is_unit {
                    quote! {
                        pub async fn #method_name(&self) -> Result<#output_type, tonic::Status> {
                                ::carbide_instrument::red::instrumented(#service_label, #operation_label, async move {
                                    Ok(self
                                        .connection()
                                        .await?
                                        .#method_name(tonic::Request::new(()))
                                        .await?
                                        .into_inner())
                                }).await
                            }
                    }
                } else {
                    let has_zero_fields = method
                        .input_type
                        .as_ref()
                        .and_then(|t| self.message_types.get(t))
                        .is_some_and(|t| t.message.field.is_empty());

                    if has_zero_fields {
                        quote! {
                            pub async fn #method_name(&self) -> Result<#output_type, tonic::Status> {
                                    ::carbide_instrument::red::instrumented(#service_label, #operation_label, async move {
                                        Ok(self
                                            .connection()
                                            .await?
                                            .#method_name(tonic::Request::new(#input_type {}))
                                            .await?
                                            .into_inner())
                                    }).await
                                }
                        }
                    } else {
                        quote! {
                            pub async fn #method_name<T: Into<#input_type>>(&self, request: T) -> Result<#output_type, tonic::Status> {
                                    ::carbide_instrument::red::instrumented(#service_label, #operation_label, async move {
                                        Ok(self
                                            .connection()
                                            .await?
                                            .#method_name(tonic::Request::new(request.into()))
                                            .await?
                                            .into_inner())
                                    }).await
                                }
                        }
                    }
                };
                Ok(token_stream)
            }
        }
    }

    /// Convert tye protobuf type (which looks like `.forge.VersionRequest` or similar) to the proper
    /// rust type, by:
    ///
    /// - Converting it to a known base type (bool, (), etc) if it's a known base type
    ///
    /// or:
    ///
    /// - Stripping the leading `.`
    /// - Converting all but the last dot-separated components into snake_case
    /// - Converting the last dot-separated component into CamelCase
    /// - Joining the components with `::` instead of `.`
    /// - Prefixing the type with `crate::<generated_types_path_within_crate>::`, to make it a fully
    ///   qualified path.
    pub(crate) fn convert_protobuf_type_to_rust_type(
        &'a self,
        t: &str,
    ) -> Result<Cow<'a, syn::Type>> {
        if let Some(base_type) = base_type(t) {
            return Ok(Cow::Owned(base_type));
        }

        if let Some(extern_type) = self.extern_paths.get(t) {
            return Ok(Cow::Borrowed(extern_type));
        }

        let components = t
            .strip_prefix(".")
            .ok_or_else(|| Error::InvalidProtobufType(t.to_string()))?
            .split('.')
            .collect::<Vec<_>>();
        let result = if components.len() > 1 {
            let leading = components[0..components.len() - 1]
                .iter()
                .map(|s| s.to_snake_case())
                .collect::<Vec<_>>()
                .join("::");
            let last = components[components.len() - 1].to_upper_camel_case();
            [leading, last].join("::")
        } else if let Some(last_component) = components.last() {
            last_component.to_upper_camel_case()
        } else {
            return Err(Error::InvalidProtobufType(t.to_string()));
        };

        Ok(Cow::Owned(syn::parse_str(&format!(
            "crate::{}::{}",
            self.generated_types_path_within_crate, result
        ))?))
    }
}

#[derive(Debug)]
struct MessageWithPackage<'a> {
    package: Option<&'a str>,
    message: &'a prost_types::DescriptorProto,
}

impl MessageWithPackage<'_> {
    fn qualified_name(&self) -> String {
        if let Some(package) = &self.package {
            format!(".{}.{}", package, self.message.name())
        } else {
            format!(".{}", self.message.name())
        }
    }
}

fn write_token_stream_if_not_up_to_date<T: AsRef<Path>>(
    token_stream: TokenStream,
    out: T,
) -> Result<()> {
    let ast: syn::File = syn::parse2(token_stream)?;
    let code = prettyplease::unparse(&ast);

    let up_to_date = match fs::read_to_string(&out) {
        Ok(existing) => code == existing,
        Err(_) => false,
    };

    if !up_to_date {
        fs::write(&out, code)?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    struct TestExternPaths(Vec<(String, syn::Type)>);

    impl TestExternPaths {
        fn index(&self) -> ExternPathSearchIndex<'_> {
            self.0
                .iter()
                .map(|(protobuf_type, rust_type)| (protobuf_type.as_str(), rust_type))
                .collect()
        }
    }

    fn descriptor_set(proto_files: &[(&str, &str)]) -> FileDescriptorSet {
        let proto_dir = temp_dir::TempDir::new().expect("Could not create temporary directory");
        let proto_paths = proto_files
            .iter()
            .map(|(name, contents)| {
                let path = proto_dir.path().join(name);
                fs::write(&path, contents).expect("Could not write test proto file");
                path
            })
            .collect::<Vec<_>>();

        tonic_prost_build::Config::new()
            .protoc_arg("--experimental_allow_proto3_optional")
            .load_fds(proto_paths.as_slice(), &[proto_dir.path()])
            .expect("Could not compile test descriptors")
    }

    fn extern_paths() -> TestExternPaths {
        TestExternPaths(vec![(
            ".ExternType".to_owned(),
            syn::parse_quote!(crate::CustomExternType),
        )])
    }

    fn test_config<'a>(root_files: Vec<String>, extern_paths: &'a TestExternPaths) -> Config<'a> {
        Config {
            wrapper_name: "TestWrapper".to_string(),
            inner_rpc_client_type: "TestInnerClient".to_string(),
            generated_types_path_within_crate: "test".to_string(),
            root_files,
            extern_paths: extern_paths.index(),
        }
    }

    fn test_generator<'a>(
        descriptor_set: &'a FileDescriptorSet,
        extern_paths: &'a TestExternPaths,
    ) -> CodeGenerator<'a> {
        CodeGenerator::new(
            test_config(vec!["test.proto".to_string()], extern_paths),
            descriptor_set,
        )
        .expect("Could not build CodeGenerator")
    }

    #[test]
    fn validates_root_files() {
        let descriptor_set =
            descriptor_set(&[("test.proto", include_str!("test_fixtures/test.proto"))]);

        let extern_paths = extern_paths();
        match CodeGenerator::new(
            test_config(vec!["missing.proto".to_string()], &extern_paths),
            &descriptor_set,
        ) {
            Err(Error::MissingRootFile(root_file)) => assert_eq!(root_file, "missing.proto"),
            Err(error) => panic!("unexpected error: {error}"),
            Ok(_) => panic!("missing root file was accepted"),
        }

        match CodeGenerator::new(
            test_config(
                vec!["test.proto".to_string(), "test.proto".to_string()],
                &extern_paths,
            ),
            &descriptor_set,
        ) {
            Err(Error::DuplicateRootFile(root_file)) => assert_eq!(root_file, "test.proto"),
            Err(error) => panic!("unexpected error: {error}"),
            Ok(_) => panic!("duplicate configured root file was accepted"),
        }

        let mut duplicate_descriptor_set = descriptor_set.clone();
        duplicate_descriptor_set
            .file
            .push(descriptor_set.file[0].clone());
        match CodeGenerator::new(
            test_config(vec!["test.proto".to_string()], &extern_paths),
            &duplicate_descriptor_set,
        ) {
            Err(Error::DuplicateRootFile(root_file)) => assert_eq!(root_file, "test.proto"),
            Err(error) => panic!("unexpected error: {error}"),
            Ok(_) => panic!("duplicate descriptor root file was accepted"),
        }
    }

    #[test]
    fn selected_roots_isolate_services_but_share_message_types() {
        let descriptor_set = descriptor_set(&[
            (
                "selected.proto",
                r#"
                    syntax = "proto3";
                    package selected;
                    import "shared.proto";

                    service SelectedService {
                      rpc SelectedRpc(shared.SharedRequest) returns (SelectedResponse);
                    }

                    message SelectedResponse {}
                "#,
            ),
            (
                "shared.proto",
                r#"
                    syntax = "proto3";
                    package shared;

                    service ImportedService {
                      rpc ImportedRpc(SharedRequest) returns (SharedResponse);
                    }

                    message SharedRequest { string value = 1; }
                    message SharedResponse {}
                "#,
            ),
            (
                "unrelated.proto",
                r#"
                    syntax = "proto3";
                    package unrelated;

                    service UnrelatedService {
                      rpc UnrelatedRpc(UnrelatedRequest) returns (UnrelatedResponse);
                    }

                    message UnrelatedRequest {}
                    message UnrelatedResponse {}
                "#,
            ),
        ]);
        let extern_paths = extern_paths();
        let generator = CodeGenerator::new(
            test_config(vec!["selected.proto".to_string()], &extern_paths),
            &descriptor_set,
        )
        .expect("Could not build CodeGenerator");
        let output_dir = temp_dir::TempDir::new().expect("Could not create temporary directory");
        let wrapper_path = output_dir.path().join("wrapper.rs");
        let converters_path = output_dir.path().join("converters.rs");

        generator
            .write_rpc_client_wrapper(&wrapper_path)
            .expect("Could not generate wrapper");
        generator
            .write_rpc_convenience_converters(&converters_path)
            .expect("Could not generate converters");

        let wrapper = fs::read_to_string(wrapper_path).expect("Could not read generated wrapper");
        assert!(wrapper.contains("pub async fn selected_rpc"));
        assert!(!wrapper.contains("pub async fn imported_rpc"));
        assert!(!wrapper.contains("pub async fn unrelated_rpc"));

        let converters =
            fs::read_to_string(converters_path).expect("Could not read generated converters");
        assert!(converters.contains("crate::test::shared::SharedRequest"));
        assert!(!converters.contains("crate::test::unrelated::UnrelatedRequest"));
    }

    #[test]
    fn generated_files_match_golden() {
        let extern_paths = extern_paths();
        let descriptor_set =
            descriptor_set(&[("test.proto", include_str!("test_fixtures/golden.proto"))]);
        let generator = test_generator(&descriptor_set, &extern_paths);
        let output_dir = temp_dir::TempDir::new().expect("Could not create temporary directory");
        let wrapper_path = output_dir.path().join("wrapper.rs");
        let converters_path = output_dir.path().join("converters.rs");

        generator
            .write_rpc_client_wrapper(&wrapper_path)
            .expect("Could not generate wrapper");
        generator
            .write_rpc_convenience_converters(&converters_path)
            .expect("Could not generate converters");

        assert_eq!(
            fs::read_to_string(wrapper_path).expect("Could not read generated wrapper"),
            include_str!("test_fixtures/golden_wrapper.rs")
        );
        assert_eq!(
            fs::read_to_string(converters_path).expect("Could not read generated converters"),
            include_str!("test_fixtures/golden_converters.rs")
        );
    }

    #[test]
    fn test_rpc_wrapper_method() {
        let descriptor_set =
            descriptor_set(&[("test.proto", include_str!("test_fixtures/test.proto"))]);
        let extern_paths = extern_paths();
        let generator = test_generator(&descriptor_set, &extern_paths);

        let methods = generator
            .root_files
            .iter()
            .flat_map(|f| &f.service)
            .flat_map(|f| &f.method)
            .map(|m| (m.name(), m))
            .collect::<HashMap<_, _>>();

        {
            let rpc = methods.get("VoidRpc").unwrap();
            let wrapper = generator
                .make_rpc_wrapper_method("test_service", rpc)
                .unwrap();
            assert_eq!(
                wrapper.to_string(),
                quote! {
                    pub async fn void_rpc(&self) -> Result<crate::test::SomeResponse, tonic::Status> {
                        ::carbide_instrument::red::instrumented("test_service", "void_rpc", async move {
                            Ok(self.connection().await?.void_rpc(tonic::Request::new(crate::test::VoidRequest {})).await?.into_inner())
                        }).await
                    }
                }
                    .to_string()
            );
        }

        {
            let rpc = methods.get("SingleMessageRpc").unwrap();
            let wrapper = generator
                .make_rpc_wrapper_method("test_service", rpc)
                .unwrap();
            assert_eq!(
                wrapper.to_string(),
                quote! {
                    pub async fn single_message_rpc<T: Into<crate::test::SingleMessageRequest>>(&self, request: T) -> Result<crate::test::SomeResponse, tonic::Status> {
                        ::carbide_instrument::red::instrumented("test_service", "single_message_rpc", async move {
                            Ok(self.connection().await?.single_message_rpc(tonic::Request::new(request.into())).await?.into_inner())
                        }).await
                    }
                }
                .to_string()
            );
        }

        {
            let rpc = methods.get("SinglePrimitiveRpc").unwrap();
            let wrapper = generator
                .make_rpc_wrapper_method("test_service", rpc)
                .unwrap();
            assert_eq!(
                wrapper.to_string(),
                quote! {
                    pub async fn single_primitive_rpc<T: Into<crate::test::SinglePrimitiveRequest>>(&self, request: T) -> Result<crate::test::SomeResponse, tonic::Status> {
                        ::carbide_instrument::red::instrumented("test_service", "single_primitive_rpc", async move {
                            Ok(self.connection().await?.single_primitive_rpc(tonic::Request::new(request.into())).await?.into_inner())
                        }).await
                    }
                }
                    .to_string()
            );
        }

        {
            let rpc = methods.get("SingleOneOfMessageRpc").unwrap();
            let wrapper = generator
                .make_rpc_wrapper_method("test_service", rpc)
                .unwrap();
            assert_eq!(
                wrapper.to_string(),
                quote! {
                    pub async fn single_one_of_message_rpc<T: Into<crate::test::SingleOneOfMessageRequest>>(&self, request: T) -> Result<crate::test::SomeResponse, tonic::Status> {
                        ::carbide_instrument::red::instrumented("test_service", "single_one_of_message_rpc", async move {
                            Ok(self.connection().await?.single_one_of_message_rpc(tonic::Request::new(request.into())).await?.into_inner())
                        }).await
                    }
                }
                    .to_string()
            );
        }

        {
            let rpc = methods.get("SingleOneOfPrimitiveRpc").unwrap();
            let wrapper = generator
                .make_rpc_wrapper_method("test_service", rpc)
                .unwrap();
            assert_eq!(
                wrapper.to_string(),
                quote! {
                    pub async fn single_one_of_primitive_rpc<T: Into<crate::test::SingleOneOfPrimitiveRequest>>(&self, request: T) -> Result<crate::test::SomeResponse, tonic::Status> {
                        ::carbide_instrument::red::instrumented("test_service", "single_one_of_primitive_rpc", async move {
                            Ok(self.connection().await?.single_one_of_primitive_rpc(tonic::Request::new(request.into())).await?.into_inner())
                        }).await
                    }
                }
                    .to_string()
            );
        }

        {
            let rpc = methods.get("MultiRpc").unwrap();
            let wrapper = generator
                .make_rpc_wrapper_method("test_service", rpc)
                .unwrap();
            assert_eq!(
                wrapper.to_string(),
                quote! {
                    pub async fn multi_rpc<T: Into<crate::test::MultiRequest>>(&self, request: T) -> Result<crate::test::SomeResponse, tonic::Status> {
                        ::carbide_instrument::red::instrumented("test_service", "multi_rpc", async move {
                            Ok(self.connection().await?.multi_rpc(tonic::Request::new(request.into())).await?.into_inner())
                        }).await
                    }
                }
                    .to_string()
            );
        }

        {
            let rpc = methods.get("ExternRpc").unwrap();
            let wrapper = generator
                .make_rpc_wrapper_method("test_service", rpc)
                .unwrap();
            assert_eq!(
                wrapper.to_string(),
                quote! {
                    pub async fn extern_rpc<T: Into<crate::test::ExternRequest>>(&self, request: T) -> Result<crate::test::SomeResponse, tonic::Status> {
                        ::carbide_instrument::red::instrumented("test_service", "extern_rpc", async move {
                            Ok(self.connection().await?.extern_rpc(tonic::Request::new(request.into())).await?.into_inner())
                        }).await
                    }
                }
                    .to_string()
            );
        }

        {
            let rpc = methods.get("SingleStreamingMessageRpc").unwrap();
            let wrapper = generator
                .make_rpc_wrapper_method("test_service", rpc)
                .unwrap();
            assert_eq!(
                wrapper.to_string(),
                quote! {
                    pub async fn single_streaming_message_rpc<T: Into<crate::test::SingleMessageRequest>>(&self, request: T) -> Result<tonic::codec::Streaming<crate::test::SomeResponse>, tonic::Status> {
                        ::carbide_instrument::red::instrumented("test_service", "single_streaming_message_rpc", async move {
                            Ok(self.connection().await?.single_streaming_message_rpc(tonic::Request::new(request.into())).await?.into_inner())
                        }).await
                    }
                }
                .to_string()
            );
        }
    }

    #[test]
    fn test_convenience_wrapper_method() {
        let descriptor_set =
            descriptor_set(&[("test.proto", include_str!("test_fixtures/test.proto"))]);
        let extern_paths = extern_paths();
        let generator = test_generator(&descriptor_set, &extern_paths);

        {
            let message_with_package = generator.message_types.get(".VoidRequest").unwrap();
            let converter = generator
                .make_convenience_converter(message_with_package)
                .unwrap()
                .unwrap();
            assert_eq!(
                converter.to_string(),
                quote! {
                    impl From<()> for crate::test::VoidRequest {
                        fn from(_: ()) -> Self {
                            Self {}
                        }
                    }
                }
                .to_string()
            );
        }

        {
            let message_with_package = generator
                .message_types
                .get(".SingleMessageRequest")
                .unwrap();
            let converter = generator
                .make_convenience_converter(message_with_package)
                .unwrap()
                .unwrap();
            assert_eq!(
                converter.to_string(),
                quote! {
                    impl<T: Into<crate::test::SingleMessage>> From<T> for crate::test::SingleMessageRequest {
                        fn from(t: T) -> Self {
                            Self {
                                value: Some(t.into())
                            }
                        }
                    }
                }
                    .to_string()
            );
        }

        {
            let message_with_package = generator
                .message_types
                .get(".SinglePrimitiveRequest")
                .unwrap();
            let converter = generator
                .make_convenience_converter(message_with_package)
                .unwrap()
                .unwrap();
            assert_eq!(
                converter.to_string(),
                quote! {
                    impl<T: Into<::prost::alloc::string::String>> From<T> for crate::test::SinglePrimitiveRequest {
                        fn from(t: T) -> Self {
                            Self {
                                value: t.into()
                            }
                        }
                    }
                }
                .to_string()
            );
        }

        {
            let message_with_package = generator
                .message_types
                .get(".SingleOneOfMessageRequest")
                .unwrap();
            let converter = generator
                .make_convenience_converter(message_with_package)
                .unwrap()
                .unwrap();
            assert_eq!(
                converter.to_string(),
                quote! {
                    impl<T: Into<crate::test::SingleMessage>> From<T> for crate::test::SingleOneOfMessageRequest {
                        fn from(t: T) -> Self {
                            Self {
                                value: Some(crate::test::single_one_of_message_request::Value::Inner(t.into()))
                            }
                        }
                    }
                }
                    .to_string()
            );
        }

        {
            let message_with_package = generator
                .message_types
                .get(".SingleOneOfPrimitiveRequest")
                .unwrap();
            let converter = generator
                .make_convenience_converter(message_with_package)
                .unwrap()
                .unwrap();
            assert_eq!(
                converter.to_string(),
                quote! {
                    impl<T: Into<::prost::alloc::string::String>> From<T> for crate::test::SingleOneOfPrimitiveRequest {
                        fn from(t: T) -> Self {
                            Self {
                                value: Some(crate::test::single_one_of_primitive_request::Value::Inner(t.into()))
                            }
                        }
                    }
                }
                    .to_string()
            );
        }

        {
            let message_with_package = generator.message_types.get(".SingleMessage").unwrap();
            let converter = generator
                .make_convenience_converter(message_with_package)
                .unwrap()
                .unwrap();
            assert_eq!(
                converter.to_string(),
                quote! {
                    impl<T: Into<::prost::alloc::string::String>> From<T> for crate::test::SingleMessage {
                        fn from(t: T) -> Self {
                            Self {
                                value: t.into()
                            }
                        }
                    }
                }
                    .to_string()
            );
        }

        {
            let message_with_package = generator.message_types.get(".EmptyMessage").unwrap();
            let converter = generator
                .make_convenience_converter(message_with_package)
                .unwrap()
                .unwrap();
            assert_eq!(
                converter.to_string(),
                quote! {
                    impl From<()> for crate::test::EmptyMessage {
                        fn from(_: ()) -> Self {
                            Self {}
                        }
                    }
                }
                .to_string()
            );
        }

        {
            let message_with_package = generator.message_types.get(".MultiRequest").unwrap();
            let converter = generator
                .make_convenience_converter(message_with_package)
                .unwrap();
            assert!(
                converter.is_none(),
                "Messages with multiple elements don't get convenience converters"
            );
        }

        {
            let message_with_package = generator.message_types.get(".ExternRequest").unwrap();
            let converter = generator
                .make_convenience_converter(message_with_package)
                .unwrap()
                .unwrap();
            assert_eq!(
                converter.to_string(),
                quote! {
                    impl<T: Into<crate::CustomExternType>> From<T> for crate::test::ExternRequest {
                        fn from(t: T) -> Self {
                            Self {
                                value: Some(t.into())
                            }
                        }
                    }
                }
                .to_string()
            );
        }
    }
}
