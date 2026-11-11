// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package probeapi

import (
	"encoding/json"
	"errors"
	"os"
	"path/filepath"
	"reflect"
	"slices"
	"strings"
	"testing"

	"github.com/NVIDIA/infra-controller/dev/k8s/scale-test/internal/jsonx"
)

// parityDir holds the metric and filter-to-metric vectors the Rust crate
// writes (tests/parity_vectors.rs). Go must reproduce them for the v1 filters,
// or a drift in either language's view of the metric contract fails.
const parityDir = "../../../../../crates/site-health-probe-api/testdata/parity"

func readParity(t *testing.T, name string, v any) {
	t.Helper()
	data, err := os.ReadFile(filepath.Join(parityDir, name))
	if err != nil {
		t.Fatalf("%s: %v (regenerate with UPDATE_PARITY=1 cargo test -p carbide-site-health-probe-api --test parity_vectors)", name, err)
	}
	if err := json.Unmarshal(data, v); err != nil {
		t.Fatalf("%s: %v", name, err)
	}
}

type rNeed struct {
	Gauge string `json:"gauge"`
	Label string `json:"label"`
	Value string `json:"value"`
}

type rSeries struct {
	Metric   string      `json:"metric"`
	Family   string      `json:"family"`
	Matchers [][2]string `json:"matchers"`
	Needs    []rNeed     `json:"needs"`
}

type rOutcome struct {
	Series *rSeries   `json:"series,omitempty"`
	Error  *[2]string `json:"error,omitempty"` // [code, field]
}

// goOutcome is the Go series_spec result in the Rust vector's shape.
func goOutcome(f Filter, full bool) rOutcome {
	spec, err := SeriesSpecFor(f, full)
	if err != nil {
		var fe *FilterError
		errors.As(err, &fe)
		return rOutcome{Error: &[2]string{string(fe.Code), fe.Field}}
	}
	needs := []rNeed{}
	for _, n := range spec.Needs {
		needs = append(needs, rNeed(n.Capability()))
	}
	matchers := spec.Matchers
	if matchers == nil {
		matchers = [][2]string{}
	}
	return rOutcome{Series: &rSeries{spec.Metric, string(spec.Family), matchers, needs}}
}

func TestSeriesSpecMatchesRust(t *testing.T) {
	var file struct {
		Filters []struct {
			Filter  json.RawMessage `json:"filter"`
			Compact rOutcome        `json:"compact"`
			Full    rOutcome        `json:"full"`
		} `json:"filters"`
		Selectors []struct {
			Selector map[string]string `json:"selector"`
			Problems []string          `json:"problems"`
		} `json:"selectors"`
	}
	readParity(t, "filter-selectors.json", &file)
	if len(file.Filters) < 10 {
		t.Fatalf("%d filter cases", len(file.Filters))
	}
	for _, row := range file.Filters {
		var f Filter
		if err := jsonx.Decode(row.Filter, &f); err != nil {
			t.Fatalf("%s: %v", row.Filter, err)
		}
		if got := goOutcome(f, false); !reflect.DeepEqual(got, row.Compact) {
			t.Errorf("%s compact: got %+v, want %+v", row.Filter, got, row.Compact)
		}
		if got := goOutcome(f, true); !reflect.DeepEqual(got, row.Full) {
			t.Errorf("%s full: got %+v, want %+v", row.Filter, got, row.Full)
		}
	}

	// Each selector has the problems the Rust crate finds, in its order. The
	// Go texts name no label or value, where the Rust texts do, so each Rust
	// text maps to the Go text of its kind.
	kinds := map[string]string{
		"must name at least one label": "must name at least one label",
		"is not a label name":          "a label is not a label name",
		"are reserved":                 "a label starting with __ is reserved",
		"series carry it":              "a label the probe's series carry is set",
		"must not be blank":            "a value is blank",
		"control characters":           "a value holds control characters",
		"is longer than":               "a value is longer than 256 bytes",
	}
	for _, row := range file.Selectors {
		var want []string
		for _, rust := range row.Problems {
			for suffix, text := range kinds {
				if strings.Contains(rust, suffix) {
					want = append(want, text)
				}
			}
		}
		got := SelectorProblems(row.Selector)
		if !slices.Equal(got, want) || len(want) != len(row.Problems) {
			t.Errorf("selector %q: got %q, want %q (rust %q)", row.Selector, got, want, row.Problems)
		}
		for k, v := range row.Selector {
			for _, input := range []string{k, strings.TrimSpace(v)} {
				if len(input) > 1 && slices.ContainsFunc(got, func(p string) bool { return strings.Contains(p, input) }) {
					t.Errorf("selector %q: a problem leaks %q", row.Selector, input)
				}
			}
		}
	}
}

func TestGaugeContractMatchesRust(t *testing.T) {
	var contract struct {
		ContractVersion uint32 `json:"contractVersion"`
		Gauges          []struct {
			Name   string   `json:"name"`
			Labels []string `json:"labels"`
		} `json:"gauges"`
	}
	readParity(t, "metrics-contract.json", &contract)
	if contract.ContractVersion != ContractVersion {
		t.Errorf("contract version: got %d, want %d", ContractVersion, contract.ContractVersion)
	}
	if len(contract.Gauges) != len(gauges) {
		t.Errorf("%d gauges, Go has %d", len(contract.Gauges), len(gauges))
	}
	for _, g := range contract.Gauges {
		want := g.Labels
		if want == nil {
			want = []string{}
		}
		got := GaugeLabels(g.Name)
		if got == nil {
			got = []string{}
		}
		if !reflect.DeepEqual(got, want) {
			t.Errorf("%s: got %v, want %v", g.Name, got, want)
		}
	}
	if GaugeLabels("not_a_gauge") != nil {
		t.Error("an unknown gauge has labels")
	}
}
