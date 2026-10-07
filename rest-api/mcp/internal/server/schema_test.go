// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package server

import (
	"encoding/json"
	"testing"

	"github.com/getkin/kin-openapi/openapi3"
	"github.com/stretchr/testify/require"
)

func schemaOf(typ string) *openapi3.Schema {
	return &openapi3.Schema{Type: &openapi3.Types{typ}}
}

func param(name, in string, required bool, sch *openapi3.Schema) *openapi3.Parameter {
	p := &openapi3.Parameter{Name: name, In: in, Required: required}
	if sch != nil {
		p.Schema = &openapi3.SchemaRef{Value: sch}
	}
	return p
}

func paramRef(name, in string, required bool, sch *openapi3.Schema) *openapi3.ParameterRef {
	return &openapi3.ParameterRef{Value: param(name, in, required, sch)}
}

func TestBuildInput_OnlyConfigFields(t *testing.T) {
	schema := (&NicoOpenApiHandler{}).buildInput(&openapi3.PathItem{}, &openapi3.Operation{OperationID: "get-metadata"})
	require.Equal(t, "object", schema.Type)
	for _, c := range commonConfigDescriptions {
		require.Contains(t, schema.Properties, c.Name, "missing common config field %s", c.Name)
		require.Equal(t, "string", schema.Properties[c.Name].Type)
	}
	require.Empty(t, schema.Required, "common config fields should never be required")
}

func TestBuildInput_PathAndQuery(t *testing.T) {
	item := &openapi3.PathItem{
		Parameters: openapi3.Parameters{
			paramRef("org", "path", true, schemaOf("string")),
			paramRef("siteId", "path", true, schemaOf("string")),
		},
	}
	op := &openapi3.Operation{
		OperationID: "get-site-status-history",
		Parameters: openapi3.Parameters{
			paramRef("pageNumber", "query", false, schemaOf("integer")),
			paramRef("pageSize", "query", false, schemaOf("integer")),
			paramRef("status", "query", true, &openapi3.Schema{Type: &openapi3.Types{"string"}, Enum: []any{"ACTIVE", "INACTIVE"}}),
		},
	}

	schema := (&NicoOpenApiHandler{}).buildInput(item, op)

	require.Equal(t, "object", schema.Type)
	// org as a path parameter is dropped because Client.Do fills the
	// {org} segment from cfg.Org. It is then re-added by the common
	// config layer as an optional override, so the schema does carry
	// "org" -- but it must NOT be required and must carry the config-
	// override description, not the OpenAPI path-param description.
	require.Contains(t, schema.Properties, "org")
	require.NotContains(t, schema.Required, "org")
	require.Equal(t, commonConfigDescriptions[0].Desc, schema.Properties["org"].Description)
	require.Contains(t, schema.Properties, "siteId")
	require.Equal(t, "string", schema.Properties["siteId"].Type)
	require.Contains(t, schema.Properties, "pageNumber")
	require.Equal(t, "integer", schema.Properties["pageNumber"].Type)
	require.Contains(t, schema.Properties, "pageSize")
	require.Equal(t, "integer", schema.Properties["pageSize"].Type)
	require.Contains(t, schema.Properties, "status")
	require.Equal(t, []any{"ACTIVE", "INACTIVE"}, schema.Properties["status"].Enum)
	require.NotNil(t, schema.AdditionalProperties)
	require.NotNil(t, schema.AdditionalProperties.Not)

	// Path params + Required:true query params are required; pure
	// query params are not.
	require.ElementsMatch(t, []string{"siteId", "status"}, schema.Required)

	// Config-layer fields still merged in.
	for _, c := range commonConfigDescriptions {
		require.Contains(t, schema.Properties, c.Name)
	}
}

func TestBuildInput_OperationOverridesPathItemParam(t *testing.T) {
	item := &openapi3.PathItem{
		Parameters: openapi3.Parameters{
			paramRef("filter", "query", true, schemaOf("string")),
		},
	}
	op := &openapi3.Operation{
		OperationID: "get-foo",
		Parameters: openapi3.Parameters{
			paramRef("filter", "query", false, schemaOf("string")),
		},
	}

	schema := (&NicoOpenApiHandler{}).buildInput(item, op)
	require.NotContains(t, schema.Required, "filter")
}

func TestBuildInput_ConfigArgDoesNotOverrideOpenAPIParam(t *testing.T) {
	// If an OpenAPI spec accidentally declares a query param named
	// "token", the OpenAPI definition wins -- we never overwrite a
	// real parameter with the common-config placeholder.
	op := &openapi3.Operation{
		OperationID: "get-foo",
		Parameters: openapi3.Parameters{
			&openapi3.ParameterRef{Value: &openapi3.Parameter{
				Name:        "token",
				In:          "query",
				Description: "API-specific token query param",
				Schema:      &openapi3.SchemaRef{Value: schemaOf("string")},
			}},
		},
	}
	schema := (&NicoOpenApiHandler{}).buildInput(&openapi3.PathItem{}, op)
	require.Contains(t, schema.Properties, "token")
	require.Equal(t, "API-specific token query param", schema.Properties["token"].Description)
}

func TestFromParam_TypeMapping(t *testing.T) {
	cases := []struct {
		openapiType string
		want        string
	}{
		{"string", "string"},
		{"integer", "integer"},
		{"number", "number"},
		{"boolean", "boolean"},
		{"unknown", "string"},
		{"", "string"},
	}
	for _, c := range cases {
		t.Run(c.openapiType, func(t *testing.T) {
			s := (&NicoOpenApiHandler{}).fromParam(param("x", "query", false, schemaOf(c.openapiType)))
			require.Equal(t, c.want, s.Type)
		})
	}
}

func TestFromParam_NoSchemaDefaultsToString(t *testing.T) {
	s := (&NicoOpenApiHandler{}).fromParam(&openapi3.Parameter{Name: "x", Description: "no schema"})
	require.Equal(t, "string", s.Type)
	require.Equal(t, "no schema", s.Description)
}

func TestFromParam_PreservesScalarValidationHints(t *testing.T) {
	minLen := 3
	maxLen := 64
	maxLenU := uint64(64)
	minV := float64(1)
	maxV := float64(100)
	s := (&NicoOpenApiHandler{}).fromParam(&openapi3.Parameter{
		Name: "pageSize",
		Schema: &openapi3.SchemaRef{Value: &openapi3.Schema{
			Type:      &openapi3.Types{"integer"},
			Format:    "int32",
			MinLength: 3,
			MaxLength: &maxLenU,
			Min:       &minV,
			Max:       &maxV,
			Default:   20,
		}},
	})

	require.Equal(t, "integer", s.Type)
	require.Equal(t, "int32", s.Format)
	require.Equal(t, &minLen, s.MinLength)
	require.Equal(t, &maxLen, s.MaxLength)
	require.NotNil(t, s.Minimum)
	require.Equal(t, float64(1), *s.Minimum)
	require.NotNil(t, s.Maximum)
	require.Equal(t, float64(100), *s.Maximum)

	var defaultValue int
	require.NoError(t, json.Unmarshal(s.Default, &defaultValue))
	require.Equal(t, 20, defaultValue)
}
