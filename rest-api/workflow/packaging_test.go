// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package workflow_test

import (
	"bufio"
	"bytes"
	"os"
	"os/exec"
	"path/filepath"
	"strings"
	"testing"
)

// This stdlib-only boundary test needs neither PostgreSQL nor a workflow build:
// from rest-api/workflow, run go test -p 1 ./packaging_test.go -count=1 -v.
// Resolve the real workflow dependency graph with only the files its builder
// copies, so a new transitive local import cannot be hidden by a full checkout.
func TestWorkflowDockerfilePackaging(t *testing.T) {
	root, err := filepath.Abs("..") // Go runs package tests in workflow/.
	if err != nil {
		t.Fatal(err)
	}
	for _, variant := range []string{"local", "production"} {
		t.Run(variant, func(t *testing.T) {
			dockerfile := filepath.Join(root, "docker", variant, "Dockerfile.nico-rest-workflow")
			workspace := t.TempDir()
			copyWorkflowBuilderInputs(t, root, dockerfile, workspace)

			cmd := exec.CommandContext(t.Context(), "go", "list", "-deps", "-p=1", "-mod=readonly", "-f={{.ImportPath}}", "./workflow/cmd/workflow")
			cmd.Dir = workspace
			// Both Dockerfiles disable CGO and cross-compile; these are the
			// Linux/amd64 settings used by the tracked Makefile build target.
			cmd.Env = append(os.Environ(), "CGO_ENABLED=0", "GOOS=linux", "GOARCH=amd64", "GOWORK=off", "GOFLAGS=")
			var stderr bytes.Buffer
			cmd.Stderr = &stderr
			output, err := cmd.Output()
			if err != nil {
				t.Fatalf("%s builder COPY boundary: CGO_ENABLED=0 GOOS=linux GOARCH=amd64 GOWORK=off GOFLAGS= go list -deps -p=1 -mod=readonly -f={{.ImportPath}} ./workflow/cmd/workflow: %v\n%s", variant, err, stderr.String())
			}
			if len(strings.Fields(string(output))) == 0 {
				t.Fatal("go list returned no workflow dependencies")
			}
			t.Logf("%s builder COPY boundary resolves workflow dependencies", variant)
		})
	}
}

// The tracked builder uses plain COPY instructions into /workspace, retaining
// source paths. Fail on unsupported syntax or relocation instead of silently
// accepting an input that this narrow Dockerfile check cannot model.
func copyWorkflowBuilderInputs(t *testing.T, root, dockerfile, workspace string) {
	t.Helper()
	file, err := os.Open(dockerfile)
	if err != nil {
		t.Fatal(err)
	}
	defer file.Close()

	builder, copied := false, false
	workdir := ""
	scanner := bufio.NewScanner(file)
	for scanner.Scan() {
		line := strings.TrimSpace(scanner.Text())
		if line == "" || strings.HasPrefix(line, "#") {
			continue
		}
		fields := strings.Fields(line)
		switch strings.ToUpper(fields[0]) {
		case "FROM":
			if builder {
				if !copied {
					t.Fatal("builder has no context COPY inputs")
				}
				return // Never treat final-stage --from=builder as context input.
			}
			builder = len(fields) >= 4 && strings.EqualFold(fields[len(fields)-2], "AS") && fields[len(fields)-1] == "builder"
		case "WORKDIR":
			if builder && len(fields) == 2 {
				workdir = fields[1]
			}
		case "COPY":
			if !builder {
				continue
			}
			if workdir != "/workspace" || len(fields) < 3 || strings.ContainsAny(line, "\\\"'$*?[]") || strings.HasPrefix(fields[1], "--") {
				t.Fatalf("unsupported builder COPY: %s", line)
			}
			destination := filepath.Clean(fields[len(fields)-1])
			for _, source := range fields[1 : len(fields)-1] {
				source = filepath.Clean(source)
				if filepath.IsAbs(source) || source == "." || source == ".." || strings.HasPrefix(source, ".."+string(filepath.Separator)) {
					t.Fatalf("unsupported context source: %s", source)
				}
				info, err := os.Stat(filepath.Join(root, source))
				if err != nil {
					t.Fatal(err)
				}
				if info.IsDir() {
					if destination != source {
						t.Fatalf("COPY relocates directory: %s", line)
					}
					if err := os.CopyFS(filepath.Join(workspace, source), os.DirFS(filepath.Join(root, source))); err != nil {
						t.Fatal(err)
					}
				} else {
					if destination != filepath.Dir(source) || !strings.HasSuffix(fields[len(fields)-1], "/") {
						t.Fatalf("COPY relocates file: %s", line)
					}
					data, err := os.ReadFile(filepath.Join(root, source))
					if err != nil {
						t.Fatal(err)
					}
					if err := os.MkdirAll(filepath.Join(workspace, filepath.Dir(source)), 0o755); err != nil {
						t.Fatal(err)
					}
					if err := os.WriteFile(filepath.Join(workspace, source), data, 0o644); err != nil {
						t.Fatal(err)
					}
				}
				copied = true
			}
		}
	}
	if err := scanner.Err(); err != nil {
		t.Fatal(err)
	}
	t.Fatal("workflow Dockerfile must contain builder and final stages")
}
