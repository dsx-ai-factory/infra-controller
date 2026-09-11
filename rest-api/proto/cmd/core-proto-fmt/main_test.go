// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package main

import (
	"strings"
	"testing"
)

func TestRemoveCodegenAnnotations(t *testing.T) {
	input := `syntax = "proto3";

import "codegen/v1/derive.proto";
import "codegen/v1/extern_path.proto";
import "codegen/v1/rust_type.proto";

option (carbide.codegen.v1.imported_extern_path) = {
  protobuf_type: ".google.protobuf.Timestamp"
  rust_type: "crate::Timestamp"
};

message Example {
  option (carbide.codegen.v1.message_derive) = "serde::Serialize";
  option (carbide.codegen.v1.message_derive) = "serde::Deserialize";
  option (carbide.codegen.v1.message_extern_path) = "::example::External";

  enum State {
    option (carbide.codegen.v1.enum_derive) = "serde::Serialize";
    option (carbide.codegen.v1.enum_extern_path) = "::example::State";
    STATE_UNSPECIFIED = 0;
  }

  PublicId value = 1 [(carbide.codegen.v1.field_rust_type) = ".example.StrongId"];
  PublicId deprecated_value = 2 [deprecated = true, (carbide.codegen.v1.field_rust_type) = ".example.StrongId"];
}

service ExampleService {
  rpc Convert(PublicId) returns (PublicId) {
    option (carbide.codegen.v1.method_rust_input_type) = ".example.StrongId";
    option (carbide.codegen.v1.method_rust_output_type) = ".example.StrongId";
  }
}
`

	output := removeCodegenAnnotations(input)
	for _, removed := range []string{
		"codegen/v1/derive.proto",
		"codegen/v1/extern_path.proto",
		"codegen/v1/rust_type.proto",
		"carbide.codegen.v1.message_derive",
		"carbide.codegen.v1.enum_derive",
		"carbide.codegen.v1.message_extern_path",
		"carbide.codegen.v1.enum_extern_path",
		"carbide.codegen.v1.imported_extern_path",
		"crate::Timestamp",
		"carbide.codegen.v1.field_rust_type",
		"carbide.codegen.v1.method_rust_input_type",
		"carbide.codegen.v1.method_rust_output_type",
	} {
		if strings.Contains(output, removed) {
			t.Errorf("output still contains Rust codegen annotation %q", removed)
		}
	}
	if !strings.Contains(output, "PublicId value = 1;") {
		t.Error("output lost protobuf schema content")
	}
	if !strings.Contains(output, "PublicId deprecated_value = 2 [deprecated = true];") {
		t.Error("output lost an unrelated field option")
	}
	if !strings.Contains(output, "rpc Convert(PublicId) returns (PublicId);") {
		t.Error("output did not restore the public RPC declaration")
	}
	if strings.Contains(output, "\n\n\n") {
		t.Error("output contains extra blank lines after removing codegen annotations")
	}
}

func TestRemoveCodegenAnnotationsPreservesUnrelatedWhitespace(t *testing.T) {
	input := `message First {
  option (carbide.codegen.v1.message_extern_path) = "::example::First";
}


message Second {}
`
	want := `message First {
}


message Second {}
`

	if got := removeCodegenAnnotations(input); got != want {
		t.Errorf("removeCodegenAnnotations() changed unrelated whitespace:\n--- got ---\n%s--- want ---\n%s", got, want)
	}
}
