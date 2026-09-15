// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package openapi

import (
	"fmt"
	"sort"
	"strings"
	"testing"

	"gopkg.in/yaml.v3"
)

type securityRequirement map[string][]string

type response struct {
	Ref string `yaml:"$ref"`
}

type operation struct {
	OperationID string                 `yaml:"operationId"`
	Security    *[]securityRequirement `yaml:"security"`
	Responses   map[string]response    `yaml:"responses"`
}

type pathItem struct {
	Get     *operation `yaml:"get"`
	Post    *operation `yaml:"post"`
	Put     *operation `yaml:"put"`
	Patch   *operation `yaml:"patch"`
	Delete  *operation `yaml:"delete"`
	Options *operation `yaml:"options"`
	Head    *operation `yaml:"head"`
	Trace   *operation `yaml:"trace"`
}

func TestSpec_AuthenticatedOperationsDocumentUnauthorizedResponse(t *testing.T) {
	var document struct {
		Security []securityRequirement `yaml:"security"`
		Paths    map[string]pathItem   `yaml:"paths"`
	}

	if err := yaml.Unmarshal(Spec, &document); err != nil {
		t.Fatalf("parse embedded OpenAPI spec: %v", err)
	}

	const wantRef = "#/components/responses/UnauthorizedError"
	var checked int
	var failures []string
	for path, item := range document.Paths {
		operations := map[string]*operation{
			"GET":     item.Get,
			"POST":    item.Post,
			"PUT":     item.Put,
			"PATCH":   item.Patch,
			"DELETE":  item.Delete,
			"OPTIONS": item.Options,
			"HEAD":    item.Head,
			"TRACE":   item.Trace,
		}
		for method, operation := range operations {
			if operation == nil {
				continue
			}

			security := document.Security
			if operation.Security != nil {
				security = *operation.Security
			}
			if !usesSecurityScheme(security, "JWTBearerToken") {
				continue
			}

			checked++
			if got := operation.Responses["401"].Ref; got != wantRef {
				failures = append(failures, fmt.Sprintf(
					"%s %s (%s): 401 response ref = %q, want %q",
					method, path, operation.OperationID, got, wantRef,
				))
			}
		}
	}

	if checked == 0 {
		t.Fatal("no operations using JWTBearerToken were found")
	}
	if len(failures) > 0 {
		sort.Strings(failures)
		t.Errorf("%d of %d authenticated operations do not document UnauthorizedError:\n%s",
			len(failures), checked, strings.Join(failures, "\n"))
	}
}

func usesSecurityScheme(requirements []securityRequirement, scheme string) bool {
	for _, requirement := range requirements {
		if _, ok := requirement[scheme]; ok {
			return true
		}
	}
	return false
}
