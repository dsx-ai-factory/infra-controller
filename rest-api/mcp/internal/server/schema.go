// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package server

import (
	"encoding/json"
	"maps"
	"slices"

	"github.com/getkin/kin-openapi/openapi3"
	"github.com/google/jsonschema-go/jsonschema"
)

// NicoOpenApiHandler builds the MCP tool input schema for a single OpenAPI
// GET operation, combining the path-item and operation-level parameters with
// the common per-call config fields.
type NicoOpenApiHandler struct {
	schema jsonschema.Schema
}

type paramKey struct {
	in   string
	name string
}

// mergeParameters combines path-item and operation parameters, with
// operation-level definitions overriding path-item-level ones that share the
// same {in,name} tuple per OpenAPI override semantics.
func mergeParameters(item *openapi3.PathItem, op *openapi3.Operation) []*openapi3.Parameter {
	merged := map[paramKey]*openapi3.Parameter{}
	add := func(refs openapi3.Parameters) {
		for _, ref := range refs {
			if ref == nil || ref.Value == nil {
				continue
			}
			p := ref.Value
			merged[paramKey{in: p.In, name: p.Name}] = p
		}
	}
	add(item.Parameters)
	add(op.Parameters)

	out := make([]*openapi3.Parameter, 0, len(merged))
	for _, p := range merged {
		out = append(out, p)
	}
	return out
}

// buildInput populates the handler's schema from the operation's path and
// query parameters merged with the four common config fields (org,
// base_url, api_name, token) and returns it. Path parameters are required;
// OpenAPI-required query parameters are required; the config fields are
// always optional.
func (h *NicoOpenApiHandler) buildInput(item *openapi3.PathItem, op *openapi3.Operation) *jsonschema.Schema {
	props := map[string]*jsonschema.Schema{}
	requiredSet := map[string]struct{}{}

	for _, p := range mergeParameters(item, op) {
		if p.Name == "org" {
			// Resolved from per-call args or server startup defaults.
			// The OpenAPI {org} segment is filled in by appcli.Client.Do.
			continue
		}
		if p.In != "path" && p.In != "query" {
			continue
		}
		props[p.Name] = h.fromParam(p)
		if p.In == "path" || p.Required {
			requiredSet[p.Name] = struct{}{}
		}
	}

	for _, c := range commonConfigDescriptions {
		if _, exists := props[c.Name]; exists {
			continue
		}
		props[c.Name] = &jsonschema.Schema{
			Type:        "string",
			Description: c.Desc,
		}
	}

	h.schema = jsonschema.Schema{
		Type:                 "object",
		Properties:           props,
		Required:             slices.Sorted(maps.Keys(requiredSet)),
		AdditionalProperties: falseJSONSchema(),
	}
	return &h.schema
}

func falseJSONSchema() *jsonschema.Schema {
	return &jsonschema.Schema{Not: &jsonschema.Schema{}}
}

// fromParam converts a single OpenAPI parameter to a JSON schema fragment.
// Types are normalised to integer/boolean/number/string; everything else
// falls back to string. Scalar validation hints such as format, min/max,
// length bounds, defaults, and enums are preserved where present so MCP
// clients get the same guardrails as the generated CLI flags.
func (*NicoOpenApiHandler) fromParam(p *openapi3.Parameter) *jsonschema.Schema {
	s := &jsonschema.Schema{Description: p.Description}
	if p.Schema == nil || p.Schema.Value == nil {
		s.Type = "string"
		return s
	}
	sch := p.Schema.Value
	switch {
	case sch.Type.Is("integer"):
		s.Type = "integer"
	case sch.Type.Is("boolean"):
		s.Type = "boolean"
	case sch.Type.Is("number"):
		s.Type = "number"
	default:
		s.Type = "string"
	}
	if len(sch.Enum) > 0 {
		s.Enum = slices.Clone(sch.Enum)
	}
	s.Format = sch.Format
	if sch.MinLength > 0 {
		v := int(sch.MinLength)
		s.MinLength = &v
	}
	if sch.MaxLength != nil {
		v := int(*sch.MaxLength)
		s.MaxLength = &v
	}
	if sch.Min != nil {
		v := *sch.Min
		s.Minimum = &v
	}
	if sch.Max != nil {
		v := *sch.Max
		s.Maximum = &v
	}
	if sch.Default != nil {
		if b, err := json.Marshal(sch.Default); err == nil {
			s.Default = b
		}
	}
	return s
}
