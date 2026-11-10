// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package config

import (
	"errors"
	"os"
	"path/filepath"
	"reflect"
	"regexp"
	"strings"
	"testing"

	"gopkg.in/yaml.v3"
)

// minimal is a valid config with every required key.
const minimal = `runId: r1-burst
command: burst
dryRun: false
namespace: nico-mat
timeouts:
  wholeRunSeconds: 3600
settings:
  bursts: 1
`

func with(extra string) string { return minimal + extra }

func sub(from, to string) string {
	if !strings.Contains(minimal, from) {
		panic(from + " is not in minimal")
	}
	return strings.Replace(minimal, from, to, 1)
}

func timeouts(extra string) string {
	return sub("wholeRunSeconds: 3600\n", "wholeRunSeconds: 3600\n  "+extra+"\n")
}

// The chart's run ID pattern. Every accepted ID must match it.
var chartRunID = regexp.MustCompile(`^[a-z0-9]([-a-z0-9]{0,38}[a-z0-9])?$`)

// Each row is parsed for the command burst. field is the Error field and
// reason its exact reason. A row with no field is valid. A yaml row only
// needs the fixed YAML reason, ending in reason.
func TestParseRules(t *testing.T) {
	subdomain := "must be a DNS subdomain name"
	distinct := "run and result must be different names"
	ordering := "need readinessSeconds < finalizeSeconds < wholeRunSeconds"
	for _, tc := range []struct{ name, yaml, field, reason string }{
		{"minimal", minimal, "", ""},
		{"unknown top-level key", with("surprise: 1\n"), "yaml", " at line 9"},
		{"unknown probe key", with("probe:\n  urll: http://x:9010\n"), "yaml", ""},
		{"unknown timeouts key", timeouts("readinessSecond: 1"), "yaml", ""},
		{"unknown control plane key", with("controlPlane:\n- namespace: n\n  labelSelector: a=b\n  extra: 1\n"), "yaml", ""},
		{"duplicate key", with("namespace: other\n"), "yaml", ""},
		{"two documents", with("---\nrunId: r2\n"), "yaml", ""},
		{"empty file", "", "yaml", ""},
		{"number holds text", with("resultMaxBytes: s3cr3t\n"), "yaml", ""},
		{"negative seconds", with("probe:\n  pollSeconds: -1\n"), "yaml", ""},
		{"fractional seconds", with("probe:\n  pollSeconds: 5.5\n"), "yaml", ""},
		{"seconds over 64 bits", with("probe:\n  pollSeconds: 18446744073709551616\n"), "yaml", ""},
		{"seconds from an alias", with("probe:\n  pollSeconds: &a 5\n  contactSeconds: *a\n"), "yaml", ""},
		{"dryRun yes", sub("dryRun: false", "dryRun: yes"), "yaml", ""},
		{"dryRun true", sub("dryRun: false", "dryRun: true"), "", ""},
		{"dryRun missing", sub("dryRun: false\n", ""), "", ""},
		{"dryRun null", sub("dryRun: false", "dryRun: null"), "", ""},
		{"null probe", with("probe: null\n"), "yaml", ""},
		{"null site", with("site: ~\n"), "yaml", ""},
		{"null resultMaxBytes", with("resultMaxBytes: null\n"), "yaml", ""},
		{"null request timeout", with("probe:\n  requestTimeoutSeconds: null\n"), "yaml", ""},
		{"null controlPlane", with("controlPlane:\n"), "yaml", ""},
		{"null runId", sub("runId: r1-burst", "runId: null"), "yaml", ""},
		{"site mapping without lockName", with("site: {}\n"), "yaml", ""},
		{"journal mapping without dir", with("journal:\n  claim: c\n"), "yaml", ""},

		{"missing runId", sub("runId: r1-burst\n", ""), "runId", `"" must have 1 to 40 characters`},
		{"missing command", sub("command: burst\n", ""), "command", `"" does not match the CLI command "burst"`},
		{"missing namespace", sub("namespace: nico-mat\n", ""), "namespace", "must be a DNS label"},
		{"missing timeouts", sub("timeouts:\n  wholeRunSeconds: 3600\n", ""), "timeouts", ordering},
		{"missing wholeRunSeconds", sub("wholeRunSeconds: 3600", "readinessSeconds: 5"), "timeouts", ordering},
		{"command does not match the CLI", sub("command: burst", "command: ingestion"), "command", `"ingestion" does not match the CLI command "burst"`},

		{"run ID one character", sub("r1-burst", "a"), "", ""},
		{"run ID with digits and dashes", sub("r1-burst", "r1-burst-2"), "", ""},
		{"run ID 40 characters", sub("r1-burst", strings.Repeat("a", 40)), "", ""},
		{"run ID 41 characters", sub("r1-burst", strings.Repeat("a", 41)), "runId", `"` + strings.Repeat("a", 41) + `" must have 1 to 40 characters`},
		{"run ID empty", sub("r1-burst", `""`), "runId", `"" must have 1 to 40 characters`},
		{"run ID capital", sub("r1-burst", "R1"), "runId", `"R1" may hold only a-z, 0-9 and '-'`},
		{"run ID underscore", sub("r1-burst", "r1_x"), "runId", `"r1_x" may hold only a-z, 0-9 and '-'`},
		{"run ID dot", sub("r1-burst", "r1.x"), "runId", `"r1.x" may hold only a-z, 0-9 and '-'`},
		{"run ID newline", sub("r1-burst", `"r1\n"`), "runId", `"r1\n" may hold only a-z, 0-9 and '-'`},
		{"run ID leading dash", sub("r1-burst", `"-r1"`), "runId", `"-r1" must start and end with a letter or digit`},
		{"run ID trailing dash", sub("r1-burst", "r1-"), "runId", `"r1-" must start and end with a letter or digit`},

		{"namespace 63 bytes", sub("nico-mat", strings.Repeat("n", 63)), "", ""},
		{"namespace 64 bytes", sub("nico-mat", strings.Repeat("n", 64)), "namespace", "must be a DNS label"},
		{"namespace with a newline", sub("nico-mat", `"nico\n"`), "namespace", "must be a DNS label"},
		{"lock name with capitals", with("site:\n  lockName: Lock\n"), "site.lockName", subdomain},
		{"lock name with a dot", with("site:\n  lockName: a.lock\n"), "", ""},
		{"run ConfigMap name with capitals", with("configMaps:\n  run: Bad_Name\n"), "configMaps.run", subdomain},
		{"result ConfigMap name ends with a dot", with("configMaps:\n  result: res.\n"), "configMaps.result", subdomain},
		{"result ConfigMap name 253 bytes", with("configMaps:\n  result: " + strings.Repeat("r", 63) + strings.Repeat(".rrr", 47) + ".r\n"), "", ""},
		{"result ConfigMap name 254 bytes", with("configMaps:\n  result: " + strings.Repeat("r", 63) + strings.Repeat(".rrr", 47) + ".rr\n"), "configMaps.result", subdomain},
		{"the control ConfigMap is not a v1 key", with("configMaps:\n  control: ctl\n"), "yaml", ""},
		{"result ConfigMap takes the run name", with("configMaps:\n  run: shared\n  result: shared\n"), "configMaps", distinct},
		{"result ConfigMap set to the default run name", with("configMaps:\n  result: r1-burst-run\n"), "configMaps", distinct},
		{"distinct hand-set ConfigMap names", with("configMaps:\n  run: a.run\n  result: a.result\n"), "", ""},

		{"finalize not below whole run", sub("wholeRunSeconds: 3600", "wholeRunSeconds: 900"), "timeouts", ordering},
		{"readiness not below finalize", timeouts("readinessSeconds: 901"), "timeouts", ordering},
		{"readiness equals finalize", timeouts("readinessSeconds: 900"), "timeouts", ordering},
		{"finalize equals whole run", sub("wholeRunSeconds: 3600", "wholeRunSeconds: 900\n  readinessSeconds: 5"), "timeouts", ordering},
		{"readiness zero", timeouts("readinessSeconds: 0"), "timeouts.readinessSeconds", "must be over 0"},
		{"whole run seven days", sub("3600", "604800"), "", ""},
		{"whole run over seven days", sub("3600", "604801"), "timeouts.wholeRunSeconds", "must be at most 604800"},

		{"result budget at the lower bound", with("resultMaxBytes: 262144\n"), "", ""},
		{"result budget at the upper bound", with("resultMaxBytes: 16777216\n"), "", ""},
		{"result budget under the bound", with("resultMaxBytes: 262143\n"), "resultMaxBytes", "must be 262144 to 16777216"},
		{"result budget over the bound", with("resultMaxBytes: 16777217\n"), "resultMaxBytes", "must be 262144 to 16777216"},
		{"result budget over 32 bits", with("resultMaxBytes: 4294967296\n"), "yaml", ""},

		{"probe URL without scheme", with("probe:\n  url: nico-site-health-probe-api:9010\n"), "probe.url", "must be an http or https URL"},
		{"probe URL with another scheme", with("probe:\n  url: ftp://probe:9010\n"), "probe.url", "must be an http or https URL"},
		{"probe URL without host", with("probe:\n  url: http://:9010\n"), "probe.url", "must be an http or https URL"},
		{"probe URL with a bad port", with("probe:\n  url: http://probe:70000\n"), "probe.url", "must be an http or https URL"},
		{"probe URL that does not parse", with("probe:\n  url: 'http://probe:9010/%zz'\n"), "probe.url", "must be an http or https URL"},
		{"probe URL with a password", with("probe:\n  url: http://user:s3cr3t@probe:9010\n"), "probe.url", "must not hold a user name or password"},
		{"probe URL with a user name", with("probe:\n  url: http://user@probe:9010\n"), "probe.url", "must not hold a user name or password"},
		{"probe URL with a query", with("probe:\n  url: http://probe:9010/?token=s3cr3t\n"), "probe.url", "must not hold a query or fragment"},
		{"probe URL with an empty query", with("probe:\n  url: 'http://probe:9010/?'\n"), "probe.url", "must not hold a query or fragment"},
		{"probe URL with an empty fragment", with("probe:\n  url: 'http://probe:9010/#'\n"), "probe.url", "must not hold a query or fragment"},
		{"probe URL with a path", with("probe:\n  url: https://probe.example:9010/base\n"), "probe.url", "must not hold a path"},
		{"probe URL with only the root path", with("probe:\n  url: https://probe.example:9010/\n"), "", ""},
		{"probe URL without a path", with("probe:\n  url: http://probe\n"), "", ""},
		{"probe URL with an IPv6 host", with("probe:\n  url: 'http://[::1]:9010'\n"), "", ""},

		{"request timeout zero", with("probe:\n  requestTimeoutSeconds: 0\n"), "probe.requestTimeoutSeconds", "must be over 0"},
		{"request timeout at the cap", with("probe:\n  requestTimeoutSeconds: 600\n"), "", ""},
		{"request timeout over the cap", with("probe:\n  requestTimeoutSeconds: 601\n"), "probe.requestTimeoutSeconds", "must be at most 600"},
		{"poll zero", with("probe:\n  pollSeconds: 0\n"), "probe.pollSeconds", "must be over 0"},
		{"poll at the cap", with("probe:\n  pollSeconds: 3600\n  contactSeconds: 3600\n  maxContactGapSeconds: 3600\n"), "", ""},
		{"poll over the cap", with("probe:\n  pollSeconds: 3601\n  contactSeconds: 3601\n"), "probe.pollSeconds", "must be at most 3600"},
		{"contact limit below poll", with("probe:\n  pollSeconds: 30\n  contactSeconds: 10\n"), "probe.contactSeconds", "must be at least probe.pollSeconds"},
		{"max contact gap below contact limit", with("probe:\n  maxContactGapSeconds: 30\n"), "probe.maxContactGapSeconds", "must be at least probe.contactSeconds"},
		{"max contact gap a day", with("probe:\n  maxContactGapSeconds: 86400\n"), "", ""},
		{"max contact gap over a day", with("probe:\n  maxContactGapSeconds: 86401\n"), "probe.maxContactGapSeconds", "must be at most 86400"},

		{"control plane selector without labels", with("controlPlane:\n- namespace: nico\n  labelSelector: ''\n"), "controlPlane[0].labelSelector", "must not be empty"},
		{"valid control plane selector", with("controlPlane:\n- namespace: nico\n  labelSelector: app.kubernetes.io/name=nico-api\n"), "", ""},

		{"relative journal dir", with("journal:\n  dir: journal\n"), "journal.dir", "must be an absolute path"},
		{"journal dir 512 bytes", with("journal:\n  dir: /" + strings.Repeat("d", 511) + "\n"), "", ""},
		{"journal dir 513 bytes", with("journal:\n  dir: /" + strings.Repeat("d", 512) + "\n"), "journal.dir", "must be at most 512 bytes"},
		{"journal claim 253 bytes", with("journal:\n  dir: /j\n  claim: " + strings.Repeat("c", 253) + "\n"), "", ""},
		{"journal claim 254 bytes", with("journal:\n  dir: /j\n  claim: " + strings.Repeat("c", 254) + "\n"), "journal.claim", "must be at most 253 bytes"},

		{"settings a list", sub("settings:\n  bursts: 1\n", "settings: [1]\n"), "settings", "must be a mapping"},
		{"settings a string", sub("settings:\n  bursts: 1\n", "settings: s3cr3t\n"), "settings", "must be a mapping"},
		{"settings null", sub("settings:\n  bursts: 1\n", "settings:\n"), "", ""},
		{"settings missing", sub("settings:\n  bursts: 1\n", ""), "", ""},
	} {
		c, err := Parse([]byte(tc.yaml), "burst")
		var e *Error
		switch {
		case tc.field == "" && err != nil, tc.field != "" && !errors.As(err, &e):
			t.Errorf("%s: err %v, want field %q", tc.name, err, tc.field)
		case tc.field == "":
			if !chartRunID.MatchString(c.RunID) || c.Settings.Kind != yaml.MappingNode {
				t.Errorf("%s: run ID %q or settings kind %v is off", tc.name, c.RunID, c.Settings.Kind)
			}
		case e.Field != tc.field, tc.field == "yaml" && !strings.HasPrefix(e.Reason, "unknown key, wrong value type or bad YAML"), !strings.HasSuffix(e.Reason, tc.reason), tc.field != "yaml" && e.Reason != tc.reason:
			t.Errorf("%s: got %q %q, want %q %q", tc.name, e.Field, e.Reason, tc.field, tc.reason)
		}
		if err != nil && strings.Contains(err.Error(), "s3cr3t") {
			t.Errorf("%s: the error holds a config value: %v", tc.name, err)
		}
	}
}

func TestParseDefaults(t *testing.T) {
	c, err := Parse([]byte(minimal), "burst")
	if err != nil {
		t.Fatal(err)
	}
	c.Settings = yaml.Node{}
	off := false
	want := Config{
		RunID: "r1-burst", Command: "burst", DryRun: &off, Namespace: "nico-mat",
		Site:           Site{LockName: "nico-scale-test-site-lock"},
		Probe:          Probe{URL: "http://nico-site-health-probe-api:9010", RequestTimeoutSeconds: 5, PollSeconds: 15, ContactSeconds: 60, MaxContactGapSeconds: 300},
		Timeouts:       Timeouts{WholeRunSeconds: 3600, ReadinessSeconds: 300, FinalizeSeconds: 900},
		Journal:        Journal{Dir: "/journal"},
		ConfigMaps:     ConfigMaps{Run: "r1-burst-run", Result: "r1-burst-result"},
		ResultMaxBytes: 8388608,
	}
	if !reflect.DeepEqual(*c, want) {
		t.Errorf("got %+v, want %+v", *c, want)
	}
}

func TestLoad(t *testing.T) {
	good := filepath.Join(t.TempDir(), "config.yaml")
	if err := os.WriteFile(good, []byte(minimal), 0o600); err != nil {
		t.Fatal(err)
	}
	if c, err := Load(good, "burst"); err != nil || c.RunID != "r1-burst" {
		t.Errorf("Load of a good file: %v, %v", c, err)
	}
	if _, err := Load(good, "ingestion"); err == nil {
		t.Error("Load accepted a command that does not match")
	}
	if _, err := Load(good+".missing", "burst"); !errors.Is(err, os.ErrNotExist) {
		t.Errorf("Load of a missing file: %v, want ErrNotExist", err)
	}
}

func TestChecks(t *testing.T) {
	yes, no := true, false
	for _, tc := range []struct {
		dryRun     *bool
		writes, ok bool
	}{{&no, true, true}, {&yes, true, true}, {nil, true, false}, {nil, false, true}, {&no, false, false}, {&yes, false, false}} {
		if err := (&Config{DryRun: tc.dryRun}).CheckDryRun(tc.writes); (err == nil) != tc.ok {
			t.Errorf("dryRun %v, writes %v: %v", tc.dryRun, tc.writes, err)
		}
	}
	for id, ok := range map[string]bool{"": false, "r1-burst-abcde": true, strings.Repeat("x", 253): true, strings.Repeat("x", 254): false} {
		if err := (&Config{ExecutionID: id}).CheckExecutionID(); (err == nil) != ok {
			t.Errorf("executionId of %d bytes: %v", len(id), err)
		}
	}
}

// TestIngestionNeedsTheLongerTimeouts: ingestion's ten summaries and end reads run in the finalize budget.
func TestIngestionNeedsTheLongerTimeouts(t *testing.T) {
	for _, tc := range []struct{ timeouts, err string }{
		{"wholeRunSeconds: 3600\n  finalizeSeconds: 1800", ""},
		{"wholeRunSeconds: 1800\n  finalizeSeconds: 900", "config timeouts.wholeRunSeconds: must be over 1800 for ingestion"},
		{"wholeRunSeconds: 3600\n  finalizeSeconds: 1799", "config timeouts.finalizeSeconds: must be at least 1800 for ingestion"},
	} {
		raw := strings.Replace(strings.Replace(minimal, "command: burst", "command: ingestion", 1), "wholeRunSeconds: 3600", tc.timeouts, 1)
		if _, err := Parse([]byte(raw), "ingestion"); err == nil && tc.err != "" || err != nil && err.Error() != tc.err {
			t.Errorf("%q: %v, want %q", tc.timeouts, err, tc.err)
		}
	}
}
