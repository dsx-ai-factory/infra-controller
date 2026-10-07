# Tonic Client Wrapper

Provides a convenience wrapper around Tonic's generated client type, for a few moderate quality-of-life improvements.
The aim is to avoid needing to make your own wrapper type, and use this one instead.

## Descriptor input and service roots

The `codegen` module is a descriptor-driven backend. It does not parse
`.proto` files or invoke `protoc`. It receives a complete
`prost_types::FileDescriptorSet`, the decoded form of protobuf's serialized
schema intermediate representation (IR), from an upstream frontend such as
`carbide-proto-compiler`.

In Carbide, one `protoc` invocation produces a shared `Schema`. The Forge and
NMX-C wrapper generators borrow its decoded `FileDescriptorSet`; tonic/prost
then takes ownership of that same descriptor set to generate Rust. The exact
serialized bytes take a separate branch to `forge.bin` for runtime gRPC
reflection.

The complete descriptor set and `Config::root_files` have different scopes:

- The complete descriptor set contains root and imported files, allowing the
  generator to resolve every request and response type used by selected
  services.
- `root_files` contains logical descriptor names, such as `forge.proto`, and
  selects only the files whose services receive wrapper methods and
  convenience converters. It is not a list of protobuf compilation inputs.

The names in `root_files` must exactly match `FileDescriptorProto.name`. A
physical compiler input such as `proto/my_service.proto` may therefore appear
as the logical descriptor name `my_service.proto` when `proto` is an include
path.

## Example

For a gRPC method `version`:

```protobuf
rpc Version(VersionRequest) returns (BuildInfo);
message VersionRequest {
  bool display_config = 1;
}
message BuildInfo {
  // ...
}
```

The `CodeGenerator::write_rpc_client_wrapper` method will provide wrapper functions like:

```rust
pub async fn version<T: Into<crate::protos::nico::VersionRequest>>(
    &self,
    request: T,
) -> Result<crate::protos::nico::BuildInfo, tonic::Status> {
    Ok(
        self
            .connection()
            .await?
            .version(tonic::Request::new(request.into()))
            .await?
            .into_inner(),
    )
}
```

And the `CodeGenerator::write_rpc_convenience_converters` method will provide convenience converters for given RPC types
like:

```rust
impl<T: Into<bool>> From<T> for crate::protos::nico::VersionRequest {
    fn from(t: T) -> Self {
        Self { display_config: t.into() }
    }
}
```

Allowing the `Version` rpc call to be reduced from:

```rust
wrapper.version(tonic::Request::new(VersionRequest { display_config: true } )).await?.into_inner()
```

to:

```rust
wrapper.version(true).await?
```

## Features

### Eliding tonic::Request and tonic::Response

Wrapper methods accept the request types directly, rather than `tonic::Request<T>`. They also return the response types
directly, rather than `tonic::Response<T>`. This is because in practice, callers almost never care about the tonic
request/response types.

### Automatic connecting and cert reloading

Every RPC wrapper ensures a connection is made to the gRPC service, connecting on-demand on first use. Or you can use
`your_wrapper.connection().await` to connect eagerly.

Cert reloading is accomplished by letting callers implement a `ConnectionProvider` trait, which contains a method
`connection_is_stale`:

```rust
async fn connection_is_stale(&self, last_connected: SystemTime) -> bool
```

Implementations can return `true` here if, for instance, the client certificate has a newer `mtime` than the
`last_connected` time.

### Convenience conversions

For every request type used by a gRPC method in the selected root files, if the
type has exactly one field, a `From<T: Into<FieldType>>` implementation is
generated that allows any type convertible into that field to be passed. So
for a request like:

```protobuf
rpc SomeMethod(SomeRequest) returns (SomeResponse);
message SomeRequest {
  required SomeId id = 1;
}
message SomeId {
  required string id = 1;
}
```

This combines nicely with each of the gRPC wrappers accepting `T: Into<RequestType>`, so that instead of having to type
out:

```rust
my_wrapper.some_method(tonic::Request::new(SomeRequest { id: SomeId { id: some_string }})).await.into_inner()
```

It becomes just:

```rust
my_wrapper.some_method(some_string).await
```

## How to use

In `build.rs`, compile the schema once, let the wrapper generator borrow the
complete descriptor set, and transfer ownership to tonic/prost only after the
wrapper output is complete:

```rust
use std::fs;
use std::path::PathBuf;

use carbide_proto_compiler::{CompilerConfig, compile};
use tonic_client_wrapper::codegen;

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let out_dir = PathBuf::from(std::env::var_os("OUT_DIR").unwrap());
    let schema = compile(&CompilerConfig {
        proto_files: vec![PathBuf::from("proto/my_service.proto")],
        include_paths: vec![PathBuf::from("proto")],
        protoc_args: Vec::new(),
    })?;

    // Retain exact frontend bytes if the application provides gRPC reflection.
    fs::write(out_dir.join("schema.bin"), &schema.raw_descriptor_set)?;

    let config = codegen::Config {
        wrapper_name: "MyApiClient".to_string(),
        inner_rpc_client_type: "crate::my_client::MyClientT".to_string(),
        root_files: vec!["my_service.proto".to_string()],
        generated_types_path_within_crate: "protos".to_string(),
        extern_paths: vec![],
    };

    let generator = codegen::CodeGenerator::new(config, &schema.file_descriptor_set)?;
    generator.write_rpc_client_wrapper(out_dir.join("my_api_client.rs"))?;
    generator.write_rpc_convenience_converters(out_dir.join("convenience_converters.rs"))?;

    // The borrowing backend is finished, so tonic/prost can consume this view.
    tonic_prost_build::configure().compile_fds(schema.file_descriptor_set)?;
    Ok(())
}
```

The in-memory schema is discarded when the build script exits. Generated Rust
files and any retained reflection descriptor remain in Cargo's `OUT_DIR`.

To use the client itself, you'll need to make an implementation of `ConnectionProvider` to yield an instance of
`my_client::MyClientT`, like:

```rust
struct MyConnectionProvider {
    url: String,
    client_config: MyClientConfig,
}

#[async_trait::async_trait]
impl tonic_client_wrapper::ConnectionProvider<MyClientT> for MyConnectionProvider {
    async fn provide_connection(&self) -> Result<MyClientT, Status> {
        // However you normally connect
        actually_connect(&self.url, &self.client_config).await
    }

    async fn connection_is_stale(&self, last_connected: SystemTime) -> bool {
        // Check if the client cert is newer than the last time we connected
        self.client_config.client_cert.as_ref().is_some_and(|client_cert| {
            if let Ok(mtime) = fs::metadata(&client_cert.cert_path).and_then(|m| m.modified()) {
                mtime > last_connected
            } else if let Ok(mtime) = fs::metadata(&client_cert.key_path).and_then(|m| m.modified())
            {
                mtime > last_connected
            } else {
                false
            }
        })
    }

    fn connection_url(&self) -> &str {
        self.url.as_str()
    }
}
```

Then you can create a client wrapper with:

```rust
MyApiClient::build(MyConnectionProvider::new(url, client_config))
```
