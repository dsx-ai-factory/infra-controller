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

use std::collections::{HashMap, HashSet};

use prost_reflect::{
    DescriptorPool, DynamicMessage, EnumDescriptor, ExtensionDescriptor, FieldDescriptor,
    FileDescriptor, Kind, MessageDescriptor, MethodDescriptor, ReflectMessage,
};
use prost_types::{
    DescriptorProto, FieldDescriptorProto, FileDescriptorSet, MethodDescriptorProto,
};

use crate::{Error, Schema};

pub(crate) struct Derive {
    // Fully qualified Prost matcher. Message matchers intentionally apply to
    // the message and all declarations nested within it.
    protobuf_type: String,
    derives: Vec<syn::Path>,
}

struct ExternPath {
    protobuf_type: String,
    rust_type: syn::Type,
}

#[derive(Clone, Copy)]
enum MethodType {
    Input,
    Output,
}

struct RustTypeOverride {
    file_name: String,
    descriptor_path: Vec<i32>,
    target: String,
    replacement: String,
    replacement_file_name: String,
    method_type: Option<MethodType>,
}

/// Lookup index for protobuf-to-Rust external type mappings.
pub type ExternPathSearchIndex<'a> = HashMap<&'a str, &'a syn::Type>;

/// Validated code-generation configuration collected from protobuf options.
pub struct Codegen {
    type_derives: Vec<Derive>,
    extern_paths: Vec<ExternPath>,
    rust_type_overrides: Vec<RustTypeOverride>,
}

impl Schema {
    /// Collects and validates all supported code-generation annotations.
    ///
    /// # Errors
    ///
    /// Returns an error when a required annotation extension is absent or has
    /// an unexpected shape, or when an annotation value is invalid.
    pub fn collect_codegen(&self) -> Result<Codegen, Error> {
        let message_derive_ext = self
            .descriptor_pool
            .derive_codegen_ext("carbide.codegen.v1.message_derive")?;
        let enum_derive_ext = self
            .descriptor_pool
            .derive_codegen_ext("carbide.codegen.v1.enum_derive")?;
        let message_extern_path_ext = self
            .descriptor_pool
            .extern_path_codegen_ext("carbide.codegen.v1.message_extern_path")?;
        let enum_extern_path_ext = self
            .descriptor_pool
            .extern_path_codegen_ext("carbide.codegen.v1.enum_extern_path")?;
        let imported_extern_path_ext = self
            .descriptor_pool
            .extern_path_codegen_ext("carbide.codegen.v1.imported_extern_path")?;
        let field_rust_type_ext = self.descriptor_pool.rust_type_codegen_ext(
            "carbide.codegen.v1.field_rust_type",
            "google.protobuf.FieldOptions",
        )?;
        let method_rust_input_type_ext = self.descriptor_pool.rust_type_codegen_ext(
            "carbide.codegen.v1.method_rust_input_type",
            "google.protobuf.MethodOptions",
        )?;
        let method_rust_output_type_ext = self.descriptor_pool.rust_type_codegen_ext(
            "carbide.codegen.v1.method_rust_output_type",
            "google.protobuf.MethodOptions",
        )?;

        // Type derives:
        let message_derives = self
            .descriptor_pool
            .all_messages()
            .map(|descriptor| descriptor.collect_derives(&message_derive_ext));
        let enum_derives = self
            .descriptor_pool
            .all_enums()
            .map(|descriptor| descriptor.collect_derives(&enum_derive_ext));

        let type_derives = message_derives
            .chain(enum_derives)
            .filter_map(|result| result.transpose())
            .collect::<Result<_, _>>()?;

        // Extern paths:
        let message_extern_paths = self
            .descriptor_pool
            .all_messages()
            .filter_map(|descriptor| descriptor.collect_extern_path(&message_extern_path_ext));
        let enum_extern_paths = self
            .descriptor_pool
            .all_enums()
            .filter_map(|descriptor| descriptor.collect_extern_path(&enum_extern_path_ext));
        let imported_extern_paths = self.descriptor_pool.files().flat_map(|descriptor| {
            descriptor
                .collect_imported_extern_paths(&imported_extern_path_ext, &self.descriptor_pool)
        });

        let extern_paths = message_extern_paths
            .chain(enum_extern_paths)
            .chain(imported_extern_paths)
            .collect::<Result<Vec<_>, _>>()?;

        // Ensure every protobuf type is mapped only once.
        extern_paths
            .iter()
            .try_fold(HashSet::new(), |mut declared, mapping| {
                if declared.insert(mapping.protobuf_type.as_str()) {
                    Ok(declared)
                } else {
                    Err(Error::RedeclaredExternPath {
                        protobuf_type: mapping.protobuf_type.clone(),
                    })
                }
            })
            .map(drop)?;

        let mut rust_type_overrides = Vec::new();
        for message in self.descriptor_pool.all_messages() {
            for field in message.fields() {
                if let Some(type_override) =
                    collect_field_override(&field, &field_rust_type_ext, &self.descriptor_pool)?
                {
                    rust_type_overrides.push(type_override);
                }
            }
        }
        for service in self.descriptor_pool.services() {
            for method in service.methods() {
                for (extension, method_type) in [
                    (&method_rust_input_type_ext, MethodType::Input),
                    (&method_rust_output_type_ext, MethodType::Output),
                ] {
                    if let Some(type_override) = collect_method_override(
                        &method,
                        extension,
                        method_type,
                        &self.descriptor_pool,
                    )? {
                        rust_type_overrides.push(type_override);
                    }
                }
            }
        }

        Ok(Codegen {
            type_derives,
            extern_paths,
            rust_type_overrides,
        })
    }
}

impl Codegen {
    /// Builds a borrowed index of the resolved external type mappings.
    pub fn extern_paths(&self) -> ExternPathSearchIndex<'_> {
        self.extern_paths
            .iter()
            .map(|mapping| (mapping.protobuf_type.as_str(), &mapping.rust_type))
            .collect()
    }

    /// Returns the descriptor view consumed by Rust generators.
    ///
    /// The returned clone applies occurrence-specific message substitutions;
    /// the public descriptor and its serialized reflection bytes are unchanged.
    pub fn rust_file_descriptor_set(
        &self,
        descriptor_set: &FileDescriptorSet,
    ) -> Result<FileDescriptorSet, Error> {
        let mut rust_descriptor_set = descriptor_set.clone();
        for type_override in &self.rust_type_overrides {
            let file = rust_descriptor_set
                .file
                .iter_mut()
                .find(|file| file.name.as_deref() == Some(type_override.file_name.as_str()))
                .ok_or_else(|| {
                    Error::MissingRustTypeOverrideTarget(type_override.target.clone())
                })?;
            if file.name.as_deref() != Some(type_override.replacement_file_name.as_str())
                && !file
                    .dependency
                    .iter()
                    .any(|dependency| dependency == &type_override.replacement_file_name)
            {
                file.dependency
                    .push(type_override.replacement_file_name.clone());
            }

            match type_override.method_type {
                None => {
                    let field =
                        field_by_path(&mut file.message_type, &type_override.descriptor_path)
                            .ok_or_else(|| {
                                Error::MissingRustTypeOverrideTarget(type_override.target.clone())
                            })?;
                    field.type_name = Some(type_override.replacement.clone());
                }
                Some(method_type) => {
                    let method = method_by_path(&mut file.service, &type_override.descriptor_path)
                        .ok_or_else(|| {
                            Error::MissingRustTypeOverrideTarget(type_override.target.clone())
                        })?;
                    match method_type {
                        MethodType::Input => {
                            method.input_type = Some(type_override.replacement.clone())
                        }
                        MethodType::Output => {
                            method.output_type = Some(type_override.replacement.clone())
                        }
                    }
                }
            }
        }
        Ok(rust_descriptor_set)
    }
}

/// Applies collected protobuf code-generation annotations to a tonic builder.
pub trait TonicBuilderCodegenExt {
    /// Applies derives and external type mappings to this builder.
    fn apply_codegen(self, codegen: &Codegen) -> Self;
}

impl TonicBuilderCodegenExt for tonic_prost_build::Builder {
    fn apply_codegen(self, codegen: &Codegen) -> Self {
        let builder = codegen.extern_paths.iter().fold(self, |builder, mapping| {
            let rust_type = &mapping.rust_type;
            builder.extern_path(
                &mapping.protobuf_type,
                quote::quote! { #rust_type }.to_string(),
            )
        });
        codegen
            .type_derives
            .iter()
            .fold(builder, |builder, target| {
                target.derives.iter().fold(builder, |builder, attr| {
                    let attribute = quote::quote! {#[derive(#attr)]}.to_string();
                    builder.type_attribute(&target.protobuf_type, attribute)
                })
            })
    }
}

trait DescriptorPoolExt {
    fn derive_codegen_ext(&self, name: &'static str) -> Result<ExtensionDescriptor, Error>;
    fn extern_path_codegen_ext(&self, name: &'static str) -> Result<ExtensionDescriptor, Error>;
    fn rust_type_codegen_ext(
        &self,
        name: &'static str,
        containing_message: &'static str,
    ) -> Result<ExtensionDescriptor, Error>;
}

impl DescriptorPoolExt for DescriptorPool {
    fn derive_codegen_ext(&self, name: &'static str) -> Result<ExtensionDescriptor, Error> {
        self.get_extension_by_name(name)
            .ok_or(Error::MissingCodegenExtension(name))
            .and_then(|extension| {
                if extension.is_list() && extension.kind() == Kind::String {
                    Ok(extension)
                } else {
                    Err(Error::InvalidCodegenExtension(
                        extension.full_name().to_owned(),
                    ))
                }
            })
    }

    fn extern_path_codegen_ext(&self, name: &'static str) -> Result<ExtensionDescriptor, Error> {
        self.get_extension_by_name(name)
            .ok_or(Error::MissingCodegenExtension(name))
    }

    fn rust_type_codegen_ext(
        &self,
        name: &'static str,
        containing_message: &'static str,
    ) -> Result<ExtensionDescriptor, Error> {
        self.get_extension_by_name(name)
            .ok_or(Error::MissingCodegenExtension(name))
            .and_then(|extension| {
                if !extension.is_list()
                    && extension.kind() == Kind::String
                    && extension.containing_message().full_name() == containing_message
                {
                    Ok(extension)
                } else {
                    Err(Error::InvalidCodegenExtension(
                        extension.full_name().to_owned(),
                    ))
                }
            })
    }
}

fn collect_field_override(
    field: &FieldDescriptor,
    extension: &ExtensionDescriptor,
    pool: &DescriptorPool,
) -> Result<Option<RustTypeOverride>, Error> {
    let Some(replacement) = annotation_string(field.options(), extension) else {
        return Ok(None);
    };
    let Kind::Message(original) = field.kind() else {
        return Err(Error::IncompatibleRustTypeOverride {
            target: field.full_name().to_owned(),
            original: format!("{:?}", field.field_descriptor_proto().r#type()),
            replacement,
        });
    };
    make_rust_type_override(
        field.full_name(),
        field.parent_file(),
        field.path(),
        &original,
        replacement,
        None,
        pool,
    )
    .map(Some)
}

fn collect_method_override(
    method: &MethodDescriptor,
    extension: &ExtensionDescriptor,
    method_type: MethodType,
    pool: &DescriptorPool,
) -> Result<Option<RustTypeOverride>, Error> {
    let Some(replacement) = annotation_string(method.options(), extension) else {
        return Ok(None);
    };
    let original = match method_type {
        MethodType::Input => method.input(),
        MethodType::Output => method.output(),
    };
    make_rust_type_override(
        method.full_name(),
        method.parent_file(),
        method.path(),
        &original,
        replacement,
        Some(method_type),
        pool,
    )
    .map(Some)
}

fn annotation_string(options: DynamicMessage, extension: &ExtensionDescriptor) -> Option<String> {
    options
        .get_extension(extension)
        .as_str()
        .filter(|value| !value.is_empty())
        .map(normalize_protobuf_name)
}

fn normalize_protobuf_name(name: &str) -> String {
    if name.starts_with('.') {
        name.to_owned()
    } else {
        format!(".{name}")
    }
}

fn make_rust_type_override(
    target: &str,
    file: FileDescriptor,
    descriptor_path: &[i32],
    original: &MessageDescriptor,
    replacement: String,
    method_type: Option<MethodType>,
    pool: &DescriptorPool,
) -> Result<RustTypeOverride, Error> {
    let replacement_descriptor = pool
        .get_message_by_name(replacement.trim_start_matches('.'))
        .ok_or_else(|| Error::UnknownRustTypeOverride {
            target: target.to_owned(),
            replacement: replacement.clone(),
        })?;
    if !messages_are_wire_compatible(original, &replacement_descriptor, &mut HashSet::new()) {
        return Err(Error::IncompatibleRustTypeOverride {
            target: target.to_owned(),
            original: format!(".{}", original.full_name()),
            replacement,
        });
    }
    Ok(RustTypeOverride {
        file_name: file.name().to_owned(),
        descriptor_path: descriptor_path.to_vec(),
        target: target.to_owned(),
        replacement,
        replacement_file_name: replacement_descriptor.parent_file().name().to_owned(),
        method_type,
    })
}

fn messages_are_wire_compatible(
    left: &MessageDescriptor,
    right: &MessageDescriptor,
    visited: &mut HashSet<(String, String)>,
) -> bool {
    if !visited.insert((left.full_name().to_owned(), right.full_name().to_owned())) {
        return true;
    }
    if left.is_map_entry() != right.is_map_entry() || left.fields().len() != right.fields().len() {
        return false;
    }
    left.fields().all(|left_field| {
        let Some(right_field) = right.get_field(left_field.number()) else {
            return false;
        };
        left_field.cardinality() == right_field.cardinality()
            && left_field.is_group() == right_field.is_group()
            && left_field.is_packed() == right_field.is_packed()
            && kinds_are_wire_compatible(left_field.kind(), right_field.kind(), visited)
    })
}

fn kinds_are_wire_compatible(
    left: Kind,
    right: Kind,
    visited: &mut HashSet<(String, String)>,
) -> bool {
    match (left, right) {
        (Kind::Message(left), Kind::Message(right)) => {
            messages_are_wire_compatible(&left, &right, visited)
        }
        (Kind::Enum(left), Kind::Enum(right)) => left.full_name() == right.full_name(),
        (left, right) => left == right,
    }
}

fn field_by_path<'a>(
    messages: &'a mut [DescriptorProto],
    path: &[i32],
) -> Option<&'a mut FieldDescriptorProto> {
    let [4, message_index, rest @ ..] = path else {
        return None;
    };
    field_in_message(messages.get_mut(*message_index as usize)?, rest)
}

fn field_in_message<'a>(
    message: &'a mut DescriptorProto,
    path: &[i32],
) -> Option<&'a mut FieldDescriptorProto> {
    match path {
        [2, field_index] => message.field.get_mut(*field_index as usize),
        [3, nested_index, rest @ ..] => {
            field_in_message(message.nested_type.get_mut(*nested_index as usize)?, rest)
        }
        _ => None,
    }
}

fn method_by_path<'a>(
    services: &'a mut [prost_types::ServiceDescriptorProto],
    path: &[i32],
) -> Option<&'a mut MethodDescriptorProto> {
    let [6, service_index, 2, method_index] = path else {
        return None;
    };
    services
        .get_mut(*service_index as usize)?
        .method
        .get_mut(*method_index as usize)
}

impl ExternPath {
    fn from_mapping(mapping: &DynamicMessage, pool: &DescriptorPool) -> Result<Self, Error> {
        let protobuf_type = mapping.required_string("protobuf_type")?;
        let rust_type = mapping.required_string("rust_type")?;

        let protobuf_name = protobuf_type.trim_start_matches('.');
        match pool
            .get_message_by_name(protobuf_name)
            .map(drop)
            .or_else(|| pool.get_enum_by_name(protobuf_name).map(drop))
        {
            Some(()) => Self::parse(protobuf_type, rust_type),
            None => Err(Error::UnknownExternPathTarget { protobuf_type }),
        }
    }

    fn parse(protobuf_type: String, rust_type: String) -> Result<Self, Error> {
        let protobuf_type = if protobuf_type.starts_with('.') {
            protobuf_type
        } else {
            format!(".{protobuf_type}")
        };
        match syn::parse_str::<syn::TypePath>(&rust_type) {
            Ok(rust_type) => Ok(Self {
                protobuf_type,
                rust_type: syn::Type::Path(rust_type),
            }),
            Err(source) => Err(Error::InvalidRustExternPath {
                protobuf_type,
                rust_type,
                source,
            }),
        }
    }
}

trait DynamicMessageExt {
    fn required_string(&self, name: &str) -> Result<String, Error>;
}

impl DynamicMessageExt for DynamicMessage {
    fn required_string(&self, name: &str) -> Result<String, Error> {
        self.get_field_by_name(name)
            .and_then(|value| value.as_str().map(str::to_owned))
            .ok_or_else(|| Error::InvalidCodegenExtension(self.descriptor().full_name().to_owned()))
    }
}

trait CodegenTypeDescriptor {
    fn options(&self) -> DynamicMessage;
    fn full_name(&self) -> &str;
}

trait CollectDerives: CodegenTypeDescriptor {
    fn collect_derives(&self, ext: &ExtensionDescriptor) -> Result<Option<Derive>, Error> {
        let derives = self
            .options()
            .get_extension(ext)
            .as_list()
            .into_iter()
            .flatten()
            .flat_map(|value| {
                value.as_str().map(|str_value| {
                    syn::parse_str::<syn::Path>(str_value).map_err(|source| {
                        Error::InvalidRustDerive {
                            protobuf_type: self.full_name().to_owned(),
                            derive: str_value.to_owned(),
                            source,
                        }
                    })
                })
            })
            .collect::<Result<Vec<_>, _>>()?;
        Ok(if derives.is_empty() {
            None
        } else {
            Some(Derive {
                protobuf_type: format!(".{}", self.full_name()),
                derives,
            })
        })
    }
}

impl<T: CodegenTypeDescriptor> CollectDerives for T {}

trait CollectExternPath: CodegenTypeDescriptor {
    fn collect_extern_path(
        &self,
        extension: &ExtensionDescriptor,
    ) -> Option<Result<ExternPath, Error>> {
        self.options()
            .get_extension(extension)
            .as_str()
            .filter(|rust_type| !rust_type.is_empty())
            .map(str::to_owned)
            .map(|rust_type| ExternPath::parse(format!(".{}", self.full_name()), rust_type))
    }
}

impl<T: CodegenTypeDescriptor> CollectExternPath for T {}

trait CollectImportedExternPaths {
    fn collect_imported_extern_paths(
        &self,
        extension: &ExtensionDescriptor,
        pool: &DescriptorPool,
    ) -> Vec<Result<ExternPath, Error>>;
}

impl CollectImportedExternPaths for FileDescriptor {
    fn collect_imported_extern_paths(
        &self,
        extension: &ExtensionDescriptor,
        pool: &DescriptorPool,
    ) -> Vec<Result<ExternPath, Error>> {
        self.options()
            .get_extension(extension)
            .as_list()
            .into_iter()
            .flatten()
            .map(|mapping| {
                mapping
                    .as_message()
                    .ok_or_else(|| Error::InvalidCodegenExtension(extension.full_name().to_owned()))
                    .and_then(|mapping| ExternPath::from_mapping(mapping, pool))
            })
            .collect()
    }
}

impl CodegenTypeDescriptor for MessageDescriptor {
    fn full_name(&self) -> &str {
        MessageDescriptor::full_name(self)
    }
    fn options(&self) -> DynamicMessage {
        MessageDescriptor::options(self)
    }
}

impl CodegenTypeDescriptor for EnumDescriptor {
    fn full_name(&self) -> &str {
        EnumDescriptor::full_name(self)
    }
    fn options(&self) -> DynamicMessage {
        EnumDescriptor::options(self)
    }
}
