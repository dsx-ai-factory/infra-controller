// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

// Package acceptance holds no code: it guards the real-cluster test suites the
// runner chart runs as helm test Jobs.
package acceptance

import (
	"fmt"
	"go/ast"
	"go/build/constraint"
	"go/parser"
	"go/token"
	"io/fs"
	"os"
	"path/filepath"
	"runtime"
	"strings"
	"testing"
)

// realClusterTags are the build constraints the chart's helm test Jobs run
// behind: a file gated on any of them must fail, not skip, on missing inputs,
// because a skipped Job exits 0 and reports a false pass.
var realClusterTags = map[string]bool{
	"devsite":    true,
	"devcluster": true,
	"envtest":    true,
	"acceptance": true,
}

// scanNoSkip discovers every real-cluster suite under root by build tag and
// returns any t.Skip, t.SkipNow or t.Skipf call they make. It parses each
// regular _test.go once, skips vendor, testdata and nested modules, and errors
// on a read or parse failure or when nothing matches, so a rename or a dropped
// tag cannot pass by scanning nothing.
func scanNoSkip(root string) (skips []string, err error) {
	found := 0
	err = filepath.WalkDir(root, func(path string, d fs.DirEntry, err error) error {
		if err != nil {
			return err
		}
		if d.IsDir() {
			if path != root && (d.Name() == "vendor" || d.Name() == "testdata" || isModuleRoot(path)) {
				return fs.SkipDir
			}
			return nil
		}
		if !d.Type().IsRegular() || !strings.HasSuffix(path, "_test.go") {
			return nil
		}
		fset := token.NewFileSet()
		f, err := parser.ParseFile(fset, path, nil, parser.ParseComments)
		if err != nil {
			return err
		}
		tagged, err := realClusterTagged(f)
		if err != nil {
			return fmt.Errorf("%s: %w", path, err)
		}
		if !tagged {
			return nil
		}
		found++
		ast.Inspect(f, func(n ast.Node) bool {
			call, ok := n.(*ast.CallExpr)
			if !ok {
				return true
			}
			if sel, ok := call.Fun.(*ast.SelectorExpr); ok {
				switch sel.Sel.Name {
				case "Skip", "SkipNow", "Skipf":
					skips = append(skips, fmt.Sprintf("%s:%d calls t.%s",
						path, fset.Position(sel.Pos()).Line, sel.Sel.Name))
				}
			}
			return true
		})
		return nil
	})
	if err != nil {
		return nil, err
	}
	if found == 0 {
		return nil, fmt.Errorf("no real-cluster suites found under %s", root)
	}
	return skips, nil
}

// isModuleRoot reports whether dir is a nested module the scan must not enter.
func isModuleRoot(dir string) bool {
	_, err := os.Stat(filepath.Join(dir, "go.mod"))
	return err == nil
}

// realClusterTagged reports whether f's //go:build directive, read from the
// real comments before the package clause, references a realClusterTags tag.
// A tag spelled inside a block comment is not a directive and does not count.
func realClusterTagged(f *ast.File) (bool, error) {
	for _, g := range f.Comments {
		for _, c := range g.List {
			if c.Slash >= f.Package || !constraint.IsGoBuild(c.Text) {
				continue
			}
			expr, err := constraint.Parse(c.Text)
			if err != nil {
				return false, err
			}
			return mentionsTag(expr, true), nil
		}
	}
	return false, nil
}

// mentionsTag reports whether a realClusterTags tag appears positively in e.
// pos tracks the current polarity so a pure negation like !acceptance, which
// builds only when the tag is unset, does not count, while devcluster &&
// !foreign still does.
func mentionsTag(e constraint.Expr, pos bool) bool {
	switch x := e.(type) {
	case *constraint.TagExpr:
		return pos && realClusterTags[x.Tag]
	case *constraint.NotExpr:
		return mentionsTag(x.X, !pos)
	case *constraint.AndExpr:
		return mentionsTag(x.X, pos) || mentionsTag(x.Y, pos)
	case *constraint.OrExpr:
		return mentionsTag(x.X, pos) || mentionsTag(x.Y, pos)
	}
	return false
}

// TestAcceptanceFixturesNeverSkip discovers the real-cluster suites by build
// tag across the module and fails on any t.Skip, so a newly tagged file is
// covered without a registration edit.
func TestAcceptanceFixturesNeverSkip(t *testing.T) {
	_, self, _, ok := runtime.Caller(0)
	if !ok {
		t.Fatal("cannot locate the guard source")
	}
	skips, err := scanNoSkip(filepath.Join(filepath.Dir(self), "..", ".."))
	if err != nil {
		t.Fatal(err)
	}
	for _, s := range skips {
		t.Errorf("%s; a real-cluster suite must fail, not skip", s)
	}
}

const (
	nestedMod   = "module nested\n\ngo 1.27\n"
	compound    = "//go:build devcluster && foreign\n\npackage x\nfunc F() { t.Skip(); t.Skipf(\"\"); t.SkipNow() }\n"
	taggedClean = "//go:build envtest\n\npackage x\nfunc F() {}\n"
	untagged    = "package x\nfunc F() { t.Skip() }\n"
	negated     = "//go:build !acceptance\n\npackage x\nfunc F() { t.Skip() }\n"
	blockTag    = "/*\n//go:build devcluster\n*/\npackage x\nfunc F() { t.Skip() }\n"
	afterBlock  = "/*\npackage foo\n*/\n//go:build devcluster\n\npackage x\nfunc F() { t.Skip() }\n"
)

// write creates dir/rel and its parents with content.
func write(t *testing.T, dir, rel, content string) {
	t.Helper()
	p := filepath.Join(dir, rel)
	if err := os.MkdirAll(filepath.Dir(p), 0o700); err != nil {
		t.Fatal(err)
	}
	if err := os.WriteFile(p, []byte(content), 0o600); err != nil {
		t.Fatal(err)
	}
}

// TestScanNoSkip exercises discovery boundaries against temporary fixtures.
// Row 3 folds the zero-fixture cases: removing any exclusion, or the
// comment-aware parse, makes one of its candidates discovered and the row fail.
func TestScanNoSkip(t *testing.T) {
	tests := []struct {
		name       string
		files      map[string]string
		extSymlink bool
		wantErr    string
		wantSkips  int
	}{
		{
			name:      "compound tag, every skip method caught",
			files:     map[string]string{"a_test.go": compound},
			wantSkips: 3,
		},
		{
			name:  "tagged clean plus untagged skip",
			files: map[string]string{"a_test.go": taggedClean, "b_test.go": untagged},
		},
		{
			name:    "pure negation tag ignored",
			files:   map[string]string{"neg_test.go": negated},
			wantErr: "no real-cluster suites found",
		},
		{
			name: "untagged, fake tag and excluded candidates yield no suites",
			files: map[string]string{
				"b_test.go":          untagged,
				"block_test.go":      blockTag,
				"vendor/a_test.go":   taggedClean,
				"testdata/a_test.go": taggedClean,
				"nested/go.mod":      nestedMod,
				"nested/a_test.go":   taggedClean,
			},
			extSymlink: true,
			wantErr:    "no real-cluster suites found",
		},
		{
			name:      "real tag after block-comment package detected",
			files:     map[string]string{"a_test.go": afterBlock},
			wantSkips: 1,
		},
	}
	for _, tt := range tests {
		t.Run(tt.name, func(t *testing.T) {
			dir := t.TempDir()
			for rel, content := range tt.files {
				write(t, dir, rel, content)
			}
			if tt.extSymlink {
				ext := t.TempDir()
				write(t, ext, "c_test.go", taggedClean)
				if err := os.Symlink(filepath.Join(ext, "c_test.go"), filepath.Join(dir, "link_test.go")); err != nil {
					t.Fatal(err)
				}
			}
			skips, err := scanNoSkip(dir)
			if tt.wantErr == "" {
				if err != nil {
					t.Fatalf("unexpected err: %v", err)
				}
			} else if err == nil || !strings.Contains(err.Error(), tt.wantErr) {
				t.Fatalf("err = %v, want containing %q", err, tt.wantErr)
			}
			if len(skips) != tt.wantSkips {
				t.Fatalf("skips = %v, want %d", skips, tt.wantSkips)
			}
		})
	}
}
