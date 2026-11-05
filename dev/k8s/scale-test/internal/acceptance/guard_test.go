// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

// Package acceptance holds no code: it guards the acceptance fixtures the
// runner chart runs as helm test Jobs.
package acceptance

import (
	"go/ast"
	"go/parser"
	"go/token"
	"path/filepath"
	"runtime"
	"testing"
)

// acceptanceFixtures are the tagged test files the chart runs as helm test
// Jobs. Each must fail, not skip, when its inputs are missing. Extend the list
// when another chart consumer adds a fixture.
var acceptanceFixtures = []string{
	"../probe/devsite_test.go",
	"../cluster/devcluster_test.go",
	"../../harness/acceptance_test.go",
	"../history/acceptance_test.go",
	"../lock/env_test.go",
	"../store/devsite_test.go",
}

// TestAcceptanceFixturesNeverSkip enforces the chart's promise that a required
// fixture fails, not skips, when its inputs are missing: a skipped Job exits 0
// and the run reports a false pass. It fails by name on any t.Skip, t.SkipNow
// or t.Skipf, and fatally when a listed fixture is missing, so a rename cannot
// make it pass by scanning nothing.
func TestAcceptanceFixturesNeverSkip(t *testing.T) {
	_, self, _, ok := runtime.Caller(0)
	if !ok {
		t.Fatal("cannot locate the guard source")
	}
	dir := filepath.Dir(self)
	for _, rel := range acceptanceFixtures {
		fset := token.NewFileSet()
		f, err := parser.ParseFile(fset, filepath.Join(dir, rel), nil, 0)
		if err != nil {
			t.Fatalf("%s: %v", rel, err)
		}
		ast.Inspect(f, func(n ast.Node) bool {
			call, ok := n.(*ast.CallExpr)
			if !ok {
				return true
			}
			if sel, ok := call.Fun.(*ast.SelectorExpr); ok {
				switch sel.Sel.Name {
				case "Skip", "SkipNow", "Skipf":
					t.Errorf("%s:%d calls t.%s; an acceptance fixture must fail, not skip",
						rel, fset.Position(sel.Pos()).Line, sel.Sel.Name)
				}
			}
			return true
		})
	}
}
