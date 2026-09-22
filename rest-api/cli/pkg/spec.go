// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package cli

import (
	"fmt"
	"strings"

	"gopkg.in/yaml.v3"
)

type Spec struct {
	Info       SpecInfo            `yaml:"info"`
	Servers    []Server            `yaml:"servers"`
	Tags       []Tag               `yaml:"tags"`
	Paths      map[string]PathItem `yaml:"paths"`
	Components Components          `yaml:"components"`
}

type SpecInfo struct {
	Title   string `yaml:"title"`
	Version string `yaml:"version"`
}

type Server struct {
	URL         string `yaml:"url"`
	Description string `yaml:"description"`
}

type Tag struct {
	Name        string `yaml:"name"`
	Description string `yaml:"description"`
}

type PathItem struct {
	Parameters []Parameter `yaml:"parameters"`
	Get        *Operation  `yaml:"get"`
	Post       *Operation  `yaml:"post"`
	Patch      *Operation  `yaml:"patch"`
	Put        *Operation  `yaml:"put"`
	Delete     *Operation  `yaml:"delete"`
}

type Operation struct {
	OperationID string       `yaml:"operationId"`
	Summary     string       `yaml:"summary"`
	Description string       `yaml:"description"`
	Tags        []string     `yaml:"tags"`
	Parameters  []Parameter  `yaml:"parameters"`
	RequestBody *RequestBody `yaml:"requestBody"`
}

type Parameter struct {
	Name        string  `yaml:"name"`
	In          string  `yaml:"in"`
	Required    bool    `yaml:"required"`
	Deprecated  bool    `yaml:"deprecated"`
	Description string  `yaml:"description"`
	Schema      *Schema `yaml:"schema"`
}

type RequestBody struct {
	Required bool                 `yaml:"required"`
	Content  map[string]MediaType `yaml:"content"`
}

type MediaType struct {
	Schema *Schema `yaml:"schema"`
}

// SchemaType handles OpenAPI 3.1 type fields that can be a string or a list of strings.
type SchemaType string

func (t *SchemaType) UnmarshalYAML(value *yaml.Node) error {
	if value.Kind == yaml.ScalarNode {
		*t = SchemaType(value.Value)
		return nil
	}
	if value.Kind == yaml.SequenceNode {
		for _, n := range value.Content {
			if n.Value != "null" {
				*t = SchemaType(n.Value)
				return nil
			}
		}
		*t = "string"
		return nil
	}
	return fmt.Errorf("unexpected type node kind: %d", value.Kind)
}

type Schema struct {
	Ref        string             `yaml:"$ref"`
	Type       SchemaType         `yaml:"type"`
	Format     string             `yaml:"format"`
	Enum       []string           `yaml:"enum"`
	Deprecated bool               `yaml:"deprecated"`
	Properties map[string]*Schema `yaml:"properties"`
	Required   []string           `yaml:"required"`
	Items      *Schema            `yaml:"items"`
	MaxItems   *int               `yaml:"maxItems"`
	MinLength  *int               `yaml:"minLength"`
	MaxLength  *int               `yaml:"maxLength"`
	Minimum    *int               `yaml:"minimum"`
	Maximum    *int               `yaml:"maximum"`
	Default    interface{}        `yaml:"default"`
}

type Components struct {
	Schemas   map[string]*Schema     `yaml:"schemas"`
	Responses map[string]interface{} `yaml:"responses"`
}

func ParseSpec(data []byte) (*Spec, error) {
	var spec Spec
	if err := yaml.Unmarshal(data, &spec); err != nil {
		return nil, fmt.Errorf("parsing spec: %w", err)
	}
	return &spec, nil
}

func (s *Spec) ResolveRef(ref string) *Schema {
	const prefix = "#/components/schemas/"
	if !strings.HasPrefix(ref, prefix) {
		return nil
	}
	name := ref[len(prefix):]
	return s.Components.Schemas[name]
}

func (s *Spec) ResolveSchema(schema *Schema) *Schema {
	if schema == nil {
		return nil
	}
	if schema.Ref != "" {
		return s.ResolveRef(schema.Ref)
	}
	return schema
}

func (s *Spec) RequestBodySchema(op *Operation) *Schema {
	if op.RequestBody == nil {
		return nil
	}
	mt, ok := op.RequestBody.Content["application/json"]
	if !ok {
		return nil
	}
	return s.ResolveSchema(mt.Schema)
}
