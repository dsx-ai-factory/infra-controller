// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package main

import (
	"bytes"
	"context"
	"fmt"
	"gopkg.in/yaml.v3"
	"io"
	"os"
	"os/exec"
	"path/filepath"
	"slices"
	"strings"
	"testing"
	"time"

	"github.com/NVIDIA/infra-controller/dev/k8s/scale-test/internal/config"
	"github.com/NVIDIA/infra-controller/dev/k8s/scale-test/internal/ingestion"
)

func TestRunPrintsVersionAndRefusesOtherCommands(t *testing.T) {
	for _, tc := range []struct {
		args   []string
		code   int
		stdout string
		stderr string
	}{
		{[]string{"version"}, 0, "scale-test dev\n", ""},
		{nil, exitNotRun, "", usage + "\n"},
		{[]string{"fleet-ups"}, exitNotRun, "", "scale-test: unknown command \"fleet-ups\"\n" + usage + "\n"},
		{[]string{"fleet-up", "--bogus"}, exitNotRun, "", usage + "\n"},
		{[]string{"fleet-up", "extra"}, exitNotRun, "", usage + "\n"},
		{[]string{"ingestion", "extra"}, exitNotRun, "", usage + "\n"},
		{[]string{"instance-lifecycle", "extra"}, exitNotRun, "", usage + "\n"},
		{[]string{"version", "extra"}, exitNotRun, "", "scale-test: unknown command \"version extra\"\n" + usage + "\n"},
	} {
		var stdout, stderr bytes.Buffer
		code := run(tc.args, &stdout, &stderr)
		if code != tc.code || stdout.String() != tc.stdout || stderr.String() != tc.stderr {
			t.Errorf("run(%q) = %d, stdout %q, stderr %q; want %d, %q, %q",
				tc.args, code, stdout.String(), stderr.String(), tc.code, tc.stdout, tc.stderr)
		}
	}
}

// TestFleetUpRefusesABadSetupBeforeTheRun proves a missing or refused config, a missing execution ID, a dryRun on a
// command that writes nothing, and bad settings exit 2 with the cause on stderr, before any cluster call.
func TestFleetUpRefusesABadSetupBeforeTheRun(t *testing.T) {
	const good = "runId: r1\ncommand: fleet-up\nnamespace: ns\ntimeouts: {wholeRunSeconds: 3600}\n"
	for _, tc := range []struct {
		config, executionID, stderr string
	}{
		{"", "pod", "read config"},
		{"runId: r1\ncommand: version\n", "pod", "config command"},
		{good, "", "config executionId"},
		{good + "dryRun: false\n", "pod", "config dryRun: must be missing"},
		{good + "settings: {mat: {port: 1}}\n", "pod", "config settings: unknown key"},
	} {
		path := filepath.Join(t.TempDir(), "config.yaml")
		if tc.config != "" {
			if err := os.WriteFile(path, []byte(tc.config), 0o600); err != nil {
				t.Fatal(err)
			}
		}
		t.Setenv("SCALE_TEST_EXECUTION_ID", tc.executionID)
		var stdout, stderr bytes.Buffer
		code := run([]string{"fleet-up", "--config", path}, &stdout, &stderr)
		if code != exitNotRun || stdout.Len() != 0 || !strings.HasPrefix(stderr.String(), "scale-test fleet-up: "+tc.stderr) {
			t.Errorf("%q: exit %d, stdout %q, stderr %q; want 2 and %q", tc.config, code, stdout.String(), stderr.String(), tc.stderr)
		}
	}
}

// TestIngestionRefusesABadSetupBeforeTheRun: ingestion writes, so a config without dryRun, or with a bad write concurrency, exits 2 before any cluster call.
func TestIngestionRefusesABadSetupBeforeTheRun(t *testing.T) {
	const good = "runId: r1\ncommand: ingestion\nnamespace: ns\ntimeouts: {wholeRunSeconds: 3600, finalizeSeconds: 1800}\n"
	for config, want := range map[string]string{good: "config dryRun: is required", good + "dryRun: true\nsettings: {writes: {concurrency: {host: 0}}}\n": "config settings.writes"} {
		path := filepath.Join(t.TempDir(), "config.yaml")
		if err := os.WriteFile(path, []byte(config), 0o600); err != nil {
			t.Fatal(err)
		}
		t.Setenv("SCALE_TEST_EXECUTION_ID", "pod")
		var stdout, stderr bytes.Buffer
		if code := run([]string{"ingestion", "--config", path}, &stdout, &stderr); code != exitNotRun || !strings.HasPrefix(stderr.String(), "scale-test ingestion: "+want) {
			t.Errorf("exit %d, stderr %q; want 2 and %q", code, stderr.String(), want)
		}
	}
}

func TestReadmeExamples(t *testing.T) {
	helm, chart, readme, values := readmeInputs(t)
	var yamlBlocks, commands []string
	for _, block := range strings.Split(string(readme), "```")[1:] {
		language, body, ok := strings.Cut(block, "\n")
		if !ok {
			continue
		}
		switch language {
		case "yaml":
			if strings.HasPrefix(body, "# Firmware overlay:") {
				continue
			}
			yamlBlocks = append(yamlBlocks, body)
		case "sh", "bash":
			for _, command := range strings.Split(strings.ReplaceAll(body, "\\\n", ""), "\n") {
				if strings.HasPrefix(command, "helm ") && !strings.HasPrefix(command, "helm install fw-") {
					commands = append(commands, command)
				}
			}
		}
	}
	// Helm reads YAML 1.1. Reject unquoted boolean-like strings, including keys.
	var scan func(*yaml.Node)
	scan = func(n *yaml.Node) {
		if n.Kind == yaml.ScalarNode && n.Style == 0 {
			switch strings.ToLower(n.Value) {
			case "y", "n", "yes", "no", "on", "off":
				t.Errorf("quote YAML 1.1 scalar %q at line %d", n.Value, n.Line)
			}
		}
		for _, child := range n.Content {
			scan(child)
		}
	}
	for _, body := range append(slices.Clone(yamlBlocks), string(values)) {
		var node yaml.Node
		if err := yaml.Unmarshal([]byte(body), &node); err != nil {
			t.Fatal(err)
		}
		scan(&node)
	}
	for _, changed := range []bool{false, true} {
		t.Run(fmt.Sprint(changed), func(t *testing.T) {
			dir := t.TempDir()
			site := readmeSiteValues
			replace := strings.NewReplacer("<ns>", "example", "<registry>", "registry.example", "<digest>", strings.Repeat("0", 64), "<class>", "fast", "<fresh-id>", "fresh", "<site UUID>", "00000000-0000-4000-8000-000000000001", "<capability name>", "gpu", "<probe repository>", "registry.example/probe", "<pinned simulator boot commands>", "exit")
			if len(yamlBlocks) != 1 {
				t.Fatal("update example values handling for new YAML blocks")
			}
			overlay := replace.Replace(yamlBlocks[0])
			if changed {
				overlay = strings.ReplaceAll(overlay, `"n": 5`, `"n": 7`)
			}
			for name, body := range map[string]string{"site-values.yaml": site, "instance-values.yaml": overlay} {
				if err := os.WriteFile(filepath.Join(dir, name), []byte(body), 0600); err != nil {
					t.Fatal(err)
				}
			}
			previous := map[string][]string{}
			renders, instances := 0, 0
			claim := ""
			jobs := map[string]bool{}
			for _, command := range commands {
				fields := strings.Fields(replace.Replace(command))
				if fields[1] == "test" {
					continue
				}
				if fields[1] != "install" && fields[1] != "upgrade" {
					t.Fatalf("unhandled example: %s", command)
				}
				flags := []string{}
				for _, flag := range fields[4:] {
					if flag == "--reuse-values" {
						flags = append(flags, previous[fields[2]]...)
						continue
					}
					if flag == "site-values.yaml" || flag == "instance-values.yaml" {
						flag = filepath.Join(dir, flag)
					}
					if changed {
						flag = strings.ReplaceAll(strings.ReplaceAll(flag, "runner.instance.n=6", "runner.instance.n=7"), "runner.instance.deleteCount=2", "runner.instance.deleteCount=3")
					}
					flags = append(flags, flag)
				}
				previous[fields[2]] = flags
				_, docs := renderReadme(t, helm, chart, fields[2], flags)
				renders++
				for _, doc := range docs {
					if fields[2] != "instance-test" {
						continue
					}
					if doc.Kind == "PersistentVolumeClaim" {
						if claim != "" && claim != doc.Metadata.Name {
							t.Fatal("examples change the retained PVC")
						}
						claim = doc.Metadata.Name
					}
					if doc.Kind == "Job" && strings.Contains(doc.Metadata.Name, "-runner-") {
						if jobs[doc.Metadata.Name] {
							t.Fatal("examples reuse a Job name")
						}
						jobs[doc.Metadata.Name] = true
					}
					if raw := doc.Data["config.yaml"]; raw != "" {
						var cfg struct {
							Command  string
							Settings struct {
								Instance struct {
									N           int `yaml:"n"`
									DeleteCount int `yaml:"deleteCount"`
								}
							}
						}
						if err := yaml.Unmarshal([]byte(raw), &cfg); err != nil {
							t.Fatal(err)
						}
						wantN, wantDelete := 5, 0
						if cfg.Command == "instance-lifecycle" {
							wantN = 6
						}
						if cfg.Command == "instance-delete" {
							wantDelete = 2
						}
						if changed {
							wantN = 7
							if wantDelete != 0 {
								wantDelete = 3
							}
						}
						if cfg.Settings.Instance.N != wantN || cfg.Settings.Instance.DeleteCount != wantDelete {
							t.Fatalf("%s: counts %+v, want n=%d deleteCount=%d", command, cfg.Settings.Instance, wantN, wantDelete)
						}
						instances++
					}
				}
			}
			if renders != 6 || instances != 3 || claim == "" || len(jobs) != 3 {
				t.Fatalf("incomplete examples: renders=%d instances=%d claim=%q jobs=%d", renders, instances, claim, len(jobs))
			}
		})
	}
}

func TestFirmwareChartExamples(t *testing.T) {
	helm, chart, b, _ := readmeInputs(t)
	_, tail, ok := strings.Cut(string(b), "# Firmware overlay:")
	if !ok {
		t.Fatal("firmware overlay missing")
	}
	overlay, _, ok := strings.Cut(tail, "```")
	if !ok {
		t.Fatal("firmware overlay incomplete")
	}
	_, overlay, _ = strings.Cut(overlay, "\n")
	replace := strings.NewReplacer("ADMIN_CLI_SECRET", "admin-writer", "SITE_LOCK_NAME", "site-lock", "CORE_NAMESPACE", "core", "UEFI_TARGET_VERSION", "1.30", "TARGET_VERSION", "1.20", "UEFI_ARTIFACT_URL", "https://artifact.invalid/uefi", "UEFI_ARTIFACT_SHA256", strings.Repeat("ab", 32))
	commands := []string{}
	for _, line := range strings.Split(strings.ReplaceAll(string(b), "\\\n", ""), "\n") {
		if strings.HasPrefix(line, "helm install fw-") {
			commands = append(commands, line)
		}
	}
	if len(commands) != 3 {
		t.Fatalf("firmware examples=%d", len(commands))
	}
	for _, changed := range []bool{false, true} {
		for _, command := range commands {
			t.Run(fmt.Sprintf("%v/%s", changed, strings.Fields(command)[2]), func(t *testing.T) {
				dir := t.TempDir()
				body := replace.Replace(overlay)
				n := uint64(5)
				if changed {
					body = strings.ReplaceAll(body, "'n': 5", "'n': 7")
					n = 7
				}
				file := filepath.Join(dir, "firmware-values.yaml")
				if err := os.WriteFile(file, []byte(body), 0600); err != nil {
					t.Fatal(err)
				}
				if err := os.WriteFile(filepath.Join(dir, "site-values.yaml"), []byte(readmeSiteValues), 0600); err != nil {
					t.Fatal(err)
				}
				fields := strings.Fields(command)
				args := []string{}
				for _, arg := range fields[4:] {
					if arg == "--wait" || arg == "--wait-for-jobs" || strings.HasPrefix(arg, "--timeout=") {
						continue
					}
					arg = replace.Replace(arg)
					switch arg {
					case "firmware-values.yaml":
						arg = file
					case "site-values.yaml":
						arg = filepath.Join(dir, arg)
					}
					args = append(args, arg)
				}
				rendered, docs := renderReadme(t, helm, chart, fields[2], args)
				found := false
				for _, doc := range docs {
					if doc.Kind != "ConfigMap" || doc.Data["config.yaml"] == "" {
						continue
					}
					var cfg config.Config
					if err := yaml.Unmarshal([]byte(doc.Data["config.yaml"]), &cfg); err != nil {
						t.Fatal(err)
					}
					if cfg.Journal.Claim != fields[2]+"-scale-test-run-data" {
						t.Fatal("firmware must use its own release claim", cfg.Journal.Claim)
					}
					s, err := ingestion.ParseFirmwareSettings(cfg.Settings)
					if err != nil {
						t.Fatal(err)
					}
					wantComponent, wantVersion := "bmc", "1.20"
					if strings.Contains(command, "component=uefi") {
						wantComponent, wantVersion = "uefi", "1.30"
						if s.Firmware.ArtifactURL != "https://artifact.invalid/uefi" || s.Firmware.ArtifactSHA256 != strings.Repeat("ab", 32) {
							t.Fatal("UEFI must use its own artifact", s.Firmware)
						}
					}
					if s.Firmware.TargetVersion != wantVersion {
						t.Fatal("target version lost precision", s.Firmware.TargetVersion)
					}
					if s.Firmware.N != n || string(s.Firmware.Component) != wantComponent || strings.Contains(doc.Data["config.yaml"], "rest:") {
						t.Fatalf("wrong rendered firmware settings: %+v", s.Firmware)
					}
					found = true
				}

				if !found || !bytes.Contains(rendered, []byte("terminationGracePeriodSeconds: 1080")) || !bytes.Contains(rendered, []byte(`secretName: "admin-writer"`)) || bytes.Contains(rendered, []byte("fixture-probe")) {
					t.Fatal("firmware native config, admin Secret or grace missing")
				}
			})
		}
	}
}

const readmeSiteValues = `image: {repository: registry.example/scale-test, digest: sha256:0000000000000000000000000000000000000000000000000000000000000000}
inventory: {clientSecret: nico-cert, expected: {hosts: 1000000, dpus: 4, racks: 2, switches: 0, powerShelves: 0, release: v2026.1}, spiffe: {trustDomain: example.org, serviceBasePaths: "/a/,/b/"}}
persistence: {storageClass: fast}
probeUrl: http://probe.example:9010
store: {url: http://store.example:9090, tokenSecret: store-token}
runner: {controlPlane: [{namespace: core, labelSelector: app=nico-api}], instance: {apiPoolSelector: {namespace: core, job: api}, apiPoolReplicas: 1}}
`

type readmeDocument struct {
	Kind     string
	Metadata struct{ Name string }
	Data     map[string]string
}

func readmeInputs(t *testing.T) (string, string, []byte, []byte) {
	t.Helper()
	helm, err := exec.LookPath("helm")
	if err != nil {
		t.Skip("helm is required to render documentation examples")
	}
	chart, err := filepath.Abs("../../../../../helm/scale-test")
	if err != nil {
		t.Fatal(err)
	}
	readme, err := os.ReadFile(filepath.Join(chart, "README.md"))
	if err != nil {
		t.Fatal(err)
	}
	values, err := os.ReadFile(filepath.Join(chart, "values.yaml"))
	if err != nil {
		t.Fatal(err)
	}
	return helm, chart, readme, values
}
func renderReadme(t *testing.T, helm, chart, release string, flags []string) ([]byte, []readmeDocument) {
	t.Helper()
	ctx, cancel := context.WithTimeout(context.Background(), 30*time.Second)
	defer cancel()
	output, err := exec.CommandContext(ctx, helm, append([]string{"template", release, chart}, flags...)...).CombinedOutput()
	if err != nil {
		t.Fatalf("%v: %s", err, output)
	}
	decoder := yaml.NewDecoder(bytes.NewReader(output))
	docs := []readmeDocument{}
	for {
		var doc readmeDocument
		if err := decoder.Decode(&doc); err == io.EOF {
			break
		} else if err != nil {
			t.Fatal(err)
		}
		docs = append(docs, doc)
	}
	return output, docs
}
