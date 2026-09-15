# Carbide Protobuf Compiler

`carbide-proto-compiler` orchestrates Carbide's build-time protobuf frontend.
It is not a replacement implementation of `protoc`: it configures
`prost-build`, preserves `protoc`'s descriptor output, and exposes that schema
to downstream code-generation backends.

## What is inside

The crate provides:

- `CompilerConfig`, a passive description of the root `.proto` files, import
  paths, and additional `protoc` arguments for one compilation;
- `compile`, the explicit operation that invokes the frontend and returns a
  shared `Schema`;
- `Schema`, three complementary representations of one compiled protobuf
  schema;
- typed errors for temporary storage, protobuf compilation, descriptor reads,
  and descriptor-pool decoding;
- fixture tests for descriptor structure, proto3 optional fields, imports,
  custom-option fidelity, and malformed input.

Compilation remains separate from configuration:

```rust
use std::path::PathBuf;

use carbide_proto_compiler::{CompilerConfig, Error, Schema, compile};

fn compile_schema() -> Result<Schema, Error> {
    compile(&CompilerConfig {
        proto_files: vec![PathBuf::from("proto/forge.proto")],
        include_paths: vec![PathBuf::from("proto")],
        protoc_args: Vec::new(),
    })
}
```

## Build pipeline

In this pipeline, `protoc` is a protobuf frontend rather than a Rust code
generator. It parses and validates `.proto` files, resolves imports and type
references, and emits a serialized `FileDescriptorSet`. The descriptor set is
the protobuf schema intermediate representation (IR); Rust is generated later
from that IR.

```text
.proto files
    -> one protoc invocation
    -> serialized FileDescriptorSet
    -> shared Schema
        -> Rust-only descriptor substitutions
            -> prost/tonic Rust generation
            -> Forge wrapper generation
            -> NMX-C wrapper generation
        -> runtime gRPC reflection descriptor
```

[`rpc/build.rs`](../rpc/build.rs) performs those steps in an ownership-aware
order:

1. Compile the complete RPC schema once.
2. Write the exact serialized descriptor bytes to `OUT_DIR/forge.bin`.
3. Collect and validate Rust-only type substitutions, then apply them to a
   clone of the decoded descriptor set.
4. Let the Forge and NMX-C wrapper generators borrow the Rust descriptor view.
5. Transfer ownership of the Rust descriptor view to
   `tonic-prost-build::compile_fds`, which generates prost messages and tonic
   services without invoking `protoc` again.

The public descriptor continues to name the source-schema types. A field or
RPC method may use the options from `codegen/v1/rust_type.proto` to select a
wire-compatible message declaration for Rust generation only. This lets Rust
use domain-specific newtypes without changing reflection or generated clients
in other languages.

The wrapper generators need the complete descriptor set to resolve message
types imported from any schema file. Their `root_files` configuration has a
narrower purpose: it selects only the files whose services receive generated
wrapper methods. It does not limit import or type resolution.

## Why Schema has three representations

All three representations exposed by `Schema` describe the output of the same
`protoc` invocation:

- `raw_descriptor_set` is the exact serialized `FileDescriptorSet` emitted by
  `protoc`. It preserves all wire data and is copied unchanged to `forge.bin`.
- `file_descriptor_set` is that schema decoded into `prost_types` Rust
  structures. Existing prost, tonic, and wrapper APIs consume this convenient
  structural view.
- `descriptor_pool` is a semantic, indexed `prost-reflect` view decoded
  directly from the raw bytes. It resolves messages, services, and extensions
  and supports typed custom-option lookup.

Project-defined option values are encoded as extension fields in protobuf
descriptor option messages. Those fields are unknown to the statically
generated `prost_types` option structures. Decoding and re-encoding a
`FileDescriptorSet` through `prost_types` can therefore drop the values even
though ordinary schema structure and the extension declaration remain. The
raw bytes avoid that lossy round trip, while `DescriptorPool` interprets the
extensions from those bytes.

The fixture tests `raw_descriptor_pool_preserves_custom_option_values` and
`prost_reencoding_drops_custom_option_values` in
[`tests/compiler.rs`](tests/compiler.rs) demonstrate both sides of this
behavior.

## Build-script lifetime and reflection

All compilation and generation runs inside the RPC crate's Cargo `build.rs`.
Temporary descriptor storage exists only long enough to capture `protoc`'s
output, and the in-memory `Schema` is discarded when the build script exits.
Generated Rust files remain in Cargo's `OUT_DIR`; `forge.bin` also remains
there for runtime gRPC reflection.

`rpc/build.rs` writes the `Schema::raw_descriptor_set` field directly to
`forge.bin`.
Consequently, custom annotation values present in the schema are also present
in that artifact. There is no separate sanitization stage. This describes the
artifact's behavior, not a policy decision about whether internal annotations
should be exposed; a different policy would require an explicit sanitization
step.

## Why it is needed

Carbide's protobuf schema drives more than generated messages and services.
RPC wrappers, convenience conversions, Rust derives, type mappings, model
conversions, and admin CLI plumbing all need information from the same schema.

Previously, these generators loaded or compiled protobuf descriptors
independently. `crates/rpc/build.rs` compiled the schema for prost and tonic,
while each client-wrapper generator invoked `protoc` again. Besides repeating
work, this allowed generators to construct different views of the same schema
and encouraged schema-shaped configuration to accumulate in distant Rust
code.

The shared compiler gives every backend one schema and type model from one
frontend invocation. It also preserves both byte fidelity for annotation data
and convenient decoded views for existing generators.

## Future annotation support

The compiler will grow incrementally through typed protobuf annotations.
Annotations will declare supported generation intent next to the relevant
schema declarations, and each backend will validate the annotation vocabulary
it understands before generating code.

Planned extensions include:

- generating eligible admin CLI arguments, request construction, and direct
  unary RPC execution;
- generating straightforward conversions between RPC types and their
  `api-model` counterparts.

Complex validation, compatibility behavior, and other domain logic will
remain hand-written. Existing `crates/rpc/build.rs` customization will stay in
place until a focused annotation backend replaces it, allowing the migration
to proceed without an all-at-once rewrite or accidental public API changes.
