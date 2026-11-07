// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package cli

import (
	"testing"

	"github.com/NVIDIA/infra-controller/rest-api/openapi"
)

func TestParseSpec_EmbeddedSpec(t *testing.T) {
	spec, err := ParseSpec(openapi.Spec)
	if err != nil {
		t.Fatalf("ParseSpec failed: %v", err)
	}
	if spec.Info.Title == "" {
		t.Error("spec.Info.Title is empty")
	}
	if len(spec.Paths) == 0 {
		t.Error("spec.Paths is empty")
	}
	if len(spec.Tags) == 0 {
		t.Error("spec.Tags is empty")
	}
}

func TestParseSpec_SchemaTypeNullable(t *testing.T) {
	yaml := `
openapi: "3.1.0"
info:
  title: test
  version: "1.0"
paths: {}
components:
  schemas:
    Test:
      type: object
      properties:
        nullable_field:
          type:
            - string
            - "null"
        simple_field:
          type: string
`
	spec, err := ParseSpec([]byte(yaml))
	if err != nil {
		t.Fatalf("ParseSpec failed: %v", err)
	}
	schema := spec.Components.Schemas["Test"]
	if schema == nil {
		t.Fatal("Test schema not found")
	}

	nullable := schema.Properties["nullable_field"]
	if nullable == nil {
		t.Fatal("nullable_field not found")
	}
	if nullable.Type != "string" {
		t.Errorf("nullable_field type = %q, want %q", nullable.Type, "string")
	}

	simple := schema.Properties["simple_field"]
	if simple == nil {
		t.Fatal("simple_field not found")
	}
	if simple.Type != "string" {
		t.Errorf("simple_field type = %q, want %q", simple.Type, "string")
	}
}

func TestResolveRef(t *testing.T) {
	spec := &Spec{
		Components: Components{
			Schemas: map[string]*Schema{
				"SiteCreateRequest": {
					Type: "object",
					Properties: map[string]*Schema{
						"name": {Type: "string"},
					},
				},
			},
		},
	}

	resolved := spec.ResolveRef("#/components/schemas/SiteCreateRequest")
	if resolved == nil {
		t.Fatal("ResolveRef returned nil")
	}
	if resolved.Type != "object" {
		t.Errorf("resolved type = %q, want %q", resolved.Type, "object")
	}

	missing := spec.ResolveRef("#/components/schemas/NonExistent")
	if missing != nil {
		t.Error("ResolveRef should return nil for missing schema")
	}

	invalid := spec.ResolveRef("not-a-ref")
	if invalid != nil {
		t.Error("ResolveRef should return nil for non-ref string")
	}
}

func TestResolveSchema(t *testing.T) {
	spec := &Spec{
		Components: Components{
			Schemas: map[string]*Schema{
				"Site": {Type: "object"},
			},
		},
	}

	// Follow $ref
	refSchema := &Schema{Ref: "#/components/schemas/Site"}
	resolved := spec.ResolveSchema(refSchema)
	if resolved == nil || resolved.Type != "object" {
		t.Error("ResolveSchema should follow $ref")
	}

	// Return direct schema
	direct := &Schema{Type: "string"}
	resolved = spec.ResolveSchema(direct)
	if resolved != direct {
		t.Error("ResolveSchema should return direct schema when no $ref")
	}

	// Nil input
	resolved = spec.ResolveSchema(nil)
	if resolved != nil {
		t.Error("ResolveSchema(nil) should return nil")
	}
}

func TestBuildCommands_EmbeddedSpec(t *testing.T) {
	spec, err := ParseSpec(openapi.Spec)
	if err != nil {
		t.Fatalf("ParseSpec failed: %v", err)
	}

	commands := BuildCommands(spec)
	if len(commands) == 0 {
		t.Fatal("BuildCommands returned no commands")
	}

	// Verify expected resource commands exist.
	cmdNames := make(map[string]bool)
	for _, cmd := range commands {
		cmdNames[cmd.Name] = true
	}

	expected := []string{
		"site", "instance", "machine", "vpc", "subnet", "allocation",
		"operating-system", "ssh-key", "ssh-key-group", "ip-block",
		"infrastructure-provider", "tenant", "metadata", "user",
		"tenant-identity",
	}
	for _, name := range expected {
		if !cmdNames[name] {
			t.Errorf("missing expected command %q", name)
		}
	}
}
