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

use std::path::Path;

use carbide_proto_compiler::{CompilerConfig, Error, Schema, TonicBuilderCodegenExt, compile, syn};
use prost::Message as _;
use prost_reflect::{DescriptorPool, DynamicMessage, Kind};
use syn::{Attribute, Item};

const FIXTURE_PACKAGE: &str = "carbide.proto.compiler.fixture";
const OPTION_NAME: &str = "carbide.proto.compiler.fixture.method_annotation";

fn compile_fixture(name: &str) -> Result<Schema, Error> {
    let fixtures = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures");
    compile(&CompilerConfig {
        proto_files: vec![fixtures.join(name)],
        include_paths: vec![fixtures],
        protoc_args: Vec::new(),
    })
}

fn compile_codegen_fixtures(names: &[&str]) -> Result<Schema, Error> {
    let fixtures = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures");
    let annotations = Path::new(env!("CARGO_MANIFEST_DIR")).join("../rpc/proto");
    let mut proto_files = names
        .iter()
        .map(|name| fixtures.join(name))
        .collect::<Vec<_>>();
    proto_files.push(annotations.join("codegen/v1/rust_type.proto"));
    if names.iter().any(|name| name.contains("rust_type_override")) {
        proto_files.push(annotations.join("codegen/v1/machine_id_types.proto"));
    }
    compile(&CompilerConfig {
        proto_files,
        include_paths: vec![fixtures, annotations],
        protoc_args: Vec::new(),
    })
}

fn generate_with_codegen(schema: Schema) -> tempfile::TempDir {
    let codegen = schema.collect_codegen().expect("codegen options are valid");
    let output = tempfile::tempdir().expect("temporary output directory is created");
    tonic_prost_build::configure()
        .out_dir(output.path())
        .apply_codegen(&codegen)
        .compile_fds(schema.file_descriptor_set)
        .expect("fixture Rust code is generated");
    output
}

fn parse_generated(output: &Path, package: &str) -> syn::File {
    let generated = std::fs::read_to_string(output.join(format!("{package}.rs")))
        .expect("generated package exists");
    syn::parse_file(&generated).expect("generated package is valid Rust syntax")
}

fn top_level_attrs<'a>(file: &'a syn::File, name: &str) -> &'a [Attribute] {
    file.items
        .iter()
        .find_map(|item| match item {
            Item::Struct(item) if item.ident == name => Some(item.attrs.as_slice()),
            Item::Enum(item) if item.ident == name => Some(item.attrs.as_slice()),
            _ => None,
        })
        .unwrap_or_else(|| panic!("generated item `{name}` exists"))
}

fn nested_item_attrs<'a>(file: &'a syn::File, module: &str, name: &str) -> &'a [Attribute] {
    let items = file
        .items
        .iter()
        .find_map(|item| match item {
            Item::Mod(item) if item.ident == module => {
                item.content.as_ref().map(|(_, items)| items.as_slice())
            }
            _ => None,
        })
        .unwrap_or_else(|| panic!("generated module `{module}` exists"));
    items
        .iter()
        .find_map(|item| match item {
            Item::Struct(item) if item.ident == name => Some(item.attrs.as_slice()),
            Item::Enum(item) if item.ident == name => Some(item.attrs.as_slice()),
            _ => None,
        })
        .unwrap_or_else(|| panic!("generated enum `{module}::{name}` exists"))
}

fn has_derive(attrs: &[Attribute], expected: &str) -> bool {
    attrs
        .iter()
        .filter(|attribute| attribute.path().is_ident("derive"))
        .flat_map(|attribute| {
            attribute
                .parse_args_with(
                    syn::punctuated::Punctuated::<syn::Path, syn::Token![,]>::parse_terminated,
                )
                .expect("generated derive attribute is valid")
        })
        .any(|path| {
            path.segments
                .iter()
                .map(|segment| segment.ident.to_string())
                .collect::<Vec<_>>()
                .join("::")
                == expected
        })
}

fn annotation(pool: &DescriptorPool) -> DynamicMessage {
    let extension = pool
        .get_extension_by_name(OPTION_NAME)
        .expect("fixture option resolves by fully qualified name");
    assert_eq!(
        extension.containing_message().full_name(),
        "google.protobuf.MethodOptions"
    );

    let Kind::Message(payload) = extension.kind() else {
        panic!("fixture option has a message payload");
    };
    assert!(matches!(
        payload
            .get_field_by_name("enabled")
            .expect("enabled field exists")
            .kind(),
        Kind::Bool
    ));
    assert!(matches!(
        payload
            .get_field_by_name("limit")
            .expect("limit field exists")
            .kind(),
        Kind::Uint32
    ));
    assert!(matches!(
        payload
            .get_field_by_name("label")
            .expect("label field exists")
            .kind(),
        Kind::String
    ));

    let service = pool
        .get_service_by_name(&format!("{FIXTURE_PACKAGE}.FixtureService"))
        .expect("fixture service exists");
    let method = service
        .methods()
        .find(|method| method.name() == "Annotated")
        .expect("annotated fixture method exists");
    let options = method.options();
    assert!(options.has_extension(&extension));
    options
        .get_extension(&extension)
        .as_message()
        .expect("fixture option value is a message")
        .clone()
}

#[test]
fn compilation_preserves_structural_descriptor_data() {
    let schema = compile_fixture("schema.proto").expect("fixture schema compiles");
    let descriptor_set = &schema.file_descriptor_set;
    let decoded = prost_types::FileDescriptorSet::decode(schema.raw_descriptor_set.as_slice())
        .expect("raw descriptor set decodes as prost types");
    assert_eq!(&decoded, descriptor_set);

    let fixture_files = descriptor_set
        .file
        .iter()
        .filter(|file| file.name.as_deref() == Some("schema.proto"))
        .collect::<Vec<_>>();
    assert_eq!(fixture_files.len(), 1, "root descriptor occurs once");
    let fixture = fixture_files[0];
    assert_eq!(fixture.package.as_deref(), Some(FIXTURE_PACKAGE));
    assert_eq!(
        descriptor_set
            .file
            .iter()
            .filter(|file| file.name.as_deref() == Some("google/protobuf/descriptor.proto"))
            .count(),
        1,
        "imported descriptor occurs once"
    );
    assert!(
        fixture
            .source_code_info
            .as_ref()
            .is_some_and(|info| !info.location.is_empty()),
        "source information is retained"
    );

    let service = fixture
        .service
        .iter()
        .find(|service| service.name.as_deref() == Some("FixtureService"))
        .expect("fixture service is structurally represented");
    let method = service
        .method
        .first()
        .expect("fixture method is represented");
    assert_eq!(method.name.as_deref(), Some("Annotated"));
    assert_eq!(
        method.input_type.as_deref(),
        Some(".carbide.proto.compiler.fixture.Request")
    );
    assert_eq!(
        method.output_type.as_deref(),
        Some(".carbide.proto.compiler.fixture.Response")
    );

    let request = fixture
        .message_type
        .iter()
        .find(|message| message.name.as_deref() == Some("Request"))
        .expect("request message is structurally represented");
    let query = request.field.first().expect("request field is represented");
    assert_eq!(query.name.as_deref(), Some("query"));
    assert_eq!(query.number, Some(1));
    assert_eq!(
        query.r#type,
        Some(prost_types::field_descriptor_proto::Type::String as i32)
    );
    assert_eq!(query.proto3_optional, Some(true));
}

#[test]
fn raw_descriptor_pool_preserves_custom_option_values() {
    let schema = compile_fixture("schema.proto").expect("fixture schema compiles");
    let annotation = annotation(&schema.descriptor_pool);

    assert_eq!(
        annotation
            .get_field_by_name("enabled")
            .and_then(|v| v.as_bool()),
        Some(true)
    );
    assert_eq!(
        annotation
            .get_field_by_name("limit")
            .and_then(|v| v.as_u32()),
        Some(73)
    );
    assert!(
        annotation
            .get_field_by_name("label")
            .is_some_and(|value| value.as_str() == Some("raw-option-sentinel"))
    );
}

#[test]
fn prost_reencoding_drops_custom_option_values() {
    let schema = compile_fixture("schema.proto").expect("fixture schema compiles");
    let reencoded = schema.file_descriptor_set.encode_to_vec();
    let reencoded_pool =
        DescriptorPool::decode(reencoded.as_slice()).expect("prost descriptor set decodes");

    let extension = reencoded_pool
        .get_extension_by_name(OPTION_NAME)
        .expect("extension declaration remains represented");
    let service = reencoded_pool
        .get_service_by_name(&format!("{FIXTURE_PACKAGE}.FixtureService"))
        .expect("fixture service remains represented");
    let method = service
        .methods()
        .find(|method| method.name() == "Annotated")
        .expect("fixture method remains represented");

    assert!(
        annotation(&schema.descriptor_pool)
            .get_field_by_name("enabled")
            .is_some()
    );
    assert!(
        !method.options().has_extension(&extension),
        "prost_types cannot retain unknown custom option values"
    );
    assert_ne!(schema.raw_descriptor_set, reencoded);
}

#[test]
fn malformed_protobuf_returns_typed_compilation_error() {
    let error = compile_fixture("malformed.proto").expect_err("malformed fixture must fail");
    assert!(matches!(error, Error::CompileProtobuf { .. }));
}

#[test]
fn applies_annotated_derives_to_messages_enums_and_nested_types_only() {
    let schema = compile_codegen_fixtures(&["derives.proto"]).expect("derive fixture compiles");
    let output = generate_with_codegen(schema);
    let generated = parse_generated(output.path(), "carbide.proto.compiler.fixture.derives");

    for attrs in [
        top_level_attrs(&generated, "AnnotatedMessage"),
        top_level_attrs(&generated, "AnnotatedEnum"),
        nested_item_attrs(&generated, "annotated_message", "Payload"),
    ] {
        assert!(has_derive(attrs, "serde::Serialize"));
        assert!(has_derive(attrs, "serde::Deserialize"));
    }

    for attrs in [
        top_level_attrs(&generated, "UnannotatedMessage"),
        top_level_attrs(&generated, "UnannotatedEnum"),
        nested_item_attrs(&generated, "unannotated_message", "Payload"),
    ] {
        assert!(!has_derive(attrs, "serde::Serialize"));
        assert!(!has_derive(attrs, "serde::Deserialize"));
    }

    assert!(has_derive(
        top_level_attrs(&generated, "AnnotatedParentOnly"),
        "serde::Serialize"
    ));
    for attrs in [
        nested_item_attrs(&generated, "annotated_parent_only", "NestedMessage"),
        nested_item_attrs(&generated, "annotated_parent_only", "NestedEnum"),
        nested_item_attrs(&generated, "annotated_parent_only", "Payload"),
    ] {
        assert!(has_derive(attrs, "serde::Serialize"));
    }
}

#[test]
fn fully_qualified_matcher_does_not_match_suffix_compatible_type() {
    let schema = compile_codegen_fixtures(&["derives.proto", "derive_suffix.proto"])
        .expect("derive fixtures compile");
    let output = generate_with_codegen(schema);
    let annotated = parse_generated(output.path(), "carbide.proto.compiler.fixture.derives");
    let suffix_compatible = parse_generated(
        output.path(),
        "prefix.carbide.proto.compiler.fixture.derives",
    );

    assert!(has_derive(
        top_level_attrs(&annotated, "AnnotatedMessage"),
        "serde::Serialize"
    ));
    assert!(!has_derive(
        top_level_attrs(&suffix_compatible, "AnnotatedMessage"),
        "serde::Serialize"
    ));
}

#[test]
fn missing_codegen_extension_is_reported() {
    let schema = compile_fixture("schema.proto").expect("fixture schema compiles");
    assert!(matches!(
        schema.collect_codegen(),
        Err(Error::MissingCodegenExtension(
            "carbide.codegen.v1.message_derive"
        ))
    ));
}

#[test]
fn invalid_codegen_extension_shape_is_reported() {
    let schema = compile_codegen_fixtures(&["invalid_codegen_extension.proto"])
        .expect("invalid extension shape is valid protobuf");
    assert!(matches!(
        schema.collect_codegen(),
        Err(Error::InvalidCodegenExtension(name))
            if name == "carbide.codegen.v1.message_derive"
    ));
}

#[test]
fn invalid_rust_derive_is_reported_with_its_protobuf_type() {
    let schema = compile_codegen_fixtures(&["invalid_derive.proto"])
        .expect("invalid Rust derive is valid protobuf");
    assert!(matches!(
        schema.collect_codegen(),
        Err(Error::InvalidRustDerive {
            protobuf_type,
            derive,
            ..
        }) if protobuf_type == "carbide.proto.compiler.fixture.invalid.InvalidMessage"
            && derive == "serde::Serialize +"
    ));
}

#[test]
fn applies_owned_enum_and_imported_external_type_mappings() {
    let schema =
        compile_codegen_fixtures(&["extern_paths.proto"]).expect("external path fixture compiles");
    let codegen = schema.collect_codegen().expect("external paths are valid");
    let extern_paths = codegen.extern_paths();

    for (protobuf_type, expected_rust_type) in [
        (
            ".carbide.proto.compiler.fixture.extern_paths.ExternalMessage",
            ":: fixture_types :: ExternalMessage",
        ),
        (
            ".carbide.proto.compiler.fixture.extern_paths.ExternalEnum",
            ":: fixture_types :: ExternalEnum",
        ),
        (".google.protobuf.Timestamp", "crate :: Timestamp"),
    ] {
        let rust_type = extern_paths
            .get(protobuf_type)
            .unwrap_or_else(|| panic!("mapping for `{protobuf_type}` exists"));
        assert_eq!(quote::quote!(#rust_type).to_string(), expected_rust_type);
    }

    let output = generate_with_codegen(schema);
    let generated = std::fs::read_to_string(
        output
            .path()
            .join("carbide.proto.compiler.fixture.extern_paths.rs"),
    )
    .expect("generated package exists");
    for rust_type in ["::fixture_types::ExternalMessage", "crate::Timestamp"] {
        assert!(
            generated.contains(rust_type),
            "generated field uses external type `{rust_type}`"
        );
    }
    assert!(!generated.contains("pub struct ExternalMessage"));
    assert!(!generated.contains("pub enum ExternalEnum"));
}

#[test]
fn invalid_rust_extern_path_is_reported_with_its_protobuf_type() {
    let schema = compile_codegen_fixtures(&["invalid_extern_path.proto"])
        .expect("invalid Rust path is valid protobuf");
    assert!(matches!(
        schema.collect_codegen(),
        Err(Error::InvalidRustExternPath {
            protobuf_type,
            rust_type,
            ..
        }) if protobuf_type
            == ".carbide.proto.compiler.fixture.invalid_extern_path.InvalidMessage"
            && rust_type == "::invalid::Type +"
    ));
}

#[test]
fn unknown_external_mapping_target_is_reported() {
    let schema = compile_codegen_fixtures(&["unknown_extern_path.proto"])
        .expect("unknown mapping target is valid protobuf");
    assert!(matches!(
        schema.collect_codegen(),
        Err(Error::UnknownExternPathTarget { protobuf_type })
            if protobuf_type == ".unknown.Missing"
    ));
}

#[test]
fn redeclared_external_mapping_is_reported() {
    for (fixture, protobuf_type) in [
        (
            "duplicate_extern_path.proto",
            ".carbide.proto.compiler.fixture.duplicate_extern_path.ExternalMessage",
        ),
        (
            "conflicting_extern_path.proto",
            ".carbide.proto.compiler.fixture.conflicting_extern_path.ExternalMessage",
        ),
    ] {
        let schema =
            compile_codegen_fixtures(&[fixture]).expect("redeclared mapping is valid protobuf");
        assert!(matches!(
            schema.collect_codegen(),
            Err(Error::RedeclaredExternPath { protobuf_type: actual }) if actual == protobuf_type
        ));
    }
}

#[test]
fn invalid_external_mapping_extension_shape_is_reported() {
    let schema = compile_codegen_fixtures(&["invalid_extern_path_extension.proto"])
        .expect("invalid extension shape is valid protobuf");
    assert!(matches!(
        schema.collect_codegen(),
        Err(Error::InvalidCodegenExtension(name))
            if name == "carbide.codegen.v1.imported_extern_path"
    ));
}

#[test]
fn rust_type_overrides_change_only_the_rust_descriptor_view() {
    let schema = compile_codegen_fixtures(&["rust_type_overrides.proto"])
        .expect("Rust type override fixture compiles");
    let public_request = schema
        .descriptor_pool
        .get_message_by_name(&format!("{FIXTURE_PACKAGE}.rust_type_overrides.Request"))
        .expect("public request exists");
    for field_name in ["singular", "optional_id", "repeated_ids", "list"] {
        let Kind::Message(message) = public_request
            .get_field_by_name(field_name)
            .expect("public field exists")
            .kind()
        else {
            panic!("public field is a message");
        };
        assert!(
            message.full_name().contains("PublicMachineId"),
            "the public descriptor remains unchanged"
        );
    }

    let codegen = schema.collect_codegen().expect("overrides are valid");
    let rust_descriptor_set = codegen
        .rust_file_descriptor_set(&schema.file_descriptor_set)
        .expect("Rust descriptor is transformed");
    let rust_pool = DescriptorPool::from_file_descriptor_set(rust_descriptor_set)
        .expect("transformed descriptor resolves");
    let rust_request = rust_pool
        .get_message_by_name(&format!("{FIXTURE_PACKAGE}.rust_type_overrides.Request"))
        .expect("Rust request exists");
    for (field_name, expected) in [
        ("singular", "common.DpuMachineId"),
        ("optional_id", "common.HostMachineId"),
        ("repeated_ids", "common.StableHostMachineId"),
        ("list", "common.HostMachineIdList"),
    ] {
        let Kind::Message(actual) = rust_request
            .get_field_by_name(field_name)
            .expect("Rust field exists")
            .kind()
        else {
            panic!("Rust field is a message");
        };
        assert_eq!(actual.full_name(), expected);
    }
    let nested = rust_pool
        .get_message_by_name(&format!(
            "{FIXTURE_PACKAGE}.rust_type_overrides.Request.Nested"
        ))
        .expect("nested message exists");
    let Kind::Message(nested_id) = nested.get_field_by_name("id").unwrap().kind() else {
        panic!("nested field is a message");
    };
    assert_eq!(nested_id.full_name(), "common.DpuMachineId");

    let method = rust_pool
        .get_service_by_name(&format!(
            "{FIXTURE_PACKAGE}.rust_type_overrides.FixtureService"
        ))
        .expect("service exists")
        .methods()
        .next()
        .expect("method exists");
    assert_eq!(method.input().full_name(), "common.StableHostMachineId");
    assert_eq!(method.output().full_name(), "common.DpuMachineId");
}

#[test]
fn unknown_rust_type_override_is_reported() {
    let schema = compile_codegen_fixtures(&["invalid_rust_type_overrides.proto"])
        .expect("unknown target is valid protobuf");
    assert!(matches!(
        schema.collect_codegen(),
        Err(Error::UnknownRustTypeOverride { target, replacement })
            if target.ends_with("UnknownTarget.id") && replacement == ".missing.MachineId"
    ));
}

#[test]
fn wire_incompatible_rust_type_override_is_reported() {
    let schema = compile_codegen_fixtures(&["incompatible_rust_type_override.proto"])
        .expect("incompatible target is valid protobuf");
    assert!(matches!(
        schema.collect_codegen(),
        Err(Error::IncompatibleRustTypeOverride { target, .. })
            if target.ends_with("Target.id")
    ));
}

#[test]
fn non_message_rust_type_override_is_reported() {
    let schema = compile_codegen_fixtures(&["non_message_rust_type_override.proto"])
        .expect("non-message target is valid protobuf");
    assert!(matches!(
        schema.collect_codegen(),
        Err(Error::IncompatibleRustTypeOverride { target, .. })
            if target.ends_with("Target.id")
    ));
}

#[test]
fn duplicate_rust_type_override_is_rejected_by_protoc() {
    assert!(matches!(
        compile_codegen_fixtures(&["duplicate_rust_type_override.proto"]),
        Err(Error::CompileProtobuf { .. })
    ));
}
