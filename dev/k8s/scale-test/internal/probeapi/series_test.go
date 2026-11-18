// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package probeapi

import (
	"errors"
	"reflect"
	"strings"
	"testing"
)

func TestSeriesSpecForRejects(t *testing.T) {
	for _, c := range []struct {
		name  string
		f     Filter
		full  bool
		code  ErrorCode
		field string
	}{
		{"done is a units filter", Filter{Type: UnitTypeHost, Done: new(true)}, false, ErrorCodeUnsupportedFilter, "done"},
		{"the REST source is not counted", Filter{Type: UnitTypeHost, Source: new(SourceRest)}, false, ErrorCodeUnsupportedFilter, "source"},
		{"a REST total is not a stage", Filter{Type: UnitTypeRestTotal, Resource: new(RestResourceRack)}, false, ErrorCodeUnsupportedFilter, "type"},
		{"an identity mismatch is not a stage", Filter{Type: UnitTypeIdentityMismatch, RecordType: new(RecordTypeHost)}, false, ErrorCodeUnsupportedFilter, "type"},
		{"a firmware request is not a stage", Filter{Type: UnitTypeHostFirmwareRequest}, false, ErrorCodeUnsupportedFilter, "type"},
		{"an expected record needs a record type", Filter{Type: UnitTypeExpectedRecord}, false, ErrorCodeUnsupportedFilter, "recordType"},
		{"a host version is not a stage field", Filter{Type: UnitTypeHost, BmcVersion: new("v1")}, false, ErrorCodeUnsupportedFilter, "bmcVersion"},
		{"allocatability is unknown under the full host read", Filter{Type: UnitTypeHost, Stage: new(StageReady), Allocatable: new(true)}, true, ErrorCodeFieldUnavailable, "allocatable"},
		{"allocatable with stage=expected is unsupported", Filter{Type: UnitTypeHost, Stage: new(StageExpected), Allocatable: new(true)}, false, ErrorCodeUnsupportedFilter, "allocatable"},
		{"allocatable with stage=expected is unavailable under the full host read", Filter{Type: UnitTypeHost, Stage: new(StageExpected), Allocatable: new(true)}, true, ErrorCodeFieldUnavailable, "allocatable"},
		{"a dpu has no expected records", Filter{Type: UnitTypeDpu, Stage: new(StageExpected)}, false, ErrorCodeUnsupportedFilter, "stage"},
	} {
		t.Run(c.name, func(t *testing.T) {
			_, err := SeriesSpecFor(c.f, c.full)
			var fe *FilterError
			if !errors.As(err, &fe) || fe.Code != c.code || fe.Field != c.field {
				t.Errorf("got %v, want %s on %s", err, c.code, c.field)
			}
		})
	}
}

func TestSelectorProblems(t *testing.T) {
	for _, c := range []struct {
		name     string
		selector map[string]string
		want     []string
	}{
		{"a real selector has no problem", map[string]string{"job": "j", "namespace": "ns"}, nil},
		{"empty names no label", map[string]string{}, []string{"must name at least one label"}},
		{"an intrinsic label collides", map[string]string{"family": "host"}, []string{"a label the probe's series carry is set"}},
		{"a reserved label", map[string]string{"__name__": "x"}, []string{"a label starting with __ is reserved"}},
		{"a bad label name", map[string]string{"1bad": "x"}, []string{"a label is not a label name"}},
		{"a blank value", map[string]string{"job": "  "}, []string{"a value is blank"}},
		{"a control character", map[string]string{"job": "a\tb"}, []string{"a value holds control characters"}},
	} {
		t.Run(c.name, func(t *testing.T) {
			if got := SelectorProblems(c.selector); !reflect.DeepEqual(got, c.want) {
				t.Errorf("got %q, want %q", got, c.want)
			}
		})
	}
	// A problem text never repeats a label name or value.
	for _, p := range SelectorProblems(map[string]string{"secret_label": "s3cr3t-value"}) {
		for _, leak := range []string{"secret_label", "s3cr3t-value"} {
			if strings.Contains(p, leak) {
				t.Errorf("problem %q leaks %q", p, leak)
			}
		}
	}
}

func TestGaugeLabels(t *testing.T) {
	for name, want := range map[string][]string{
		"units":                            {"type", "stage", "state"},
		"hosts":                            {"model", "instance_type", "state", "allocatable", "update_complete"},
		"expected_records":                 {"type", "linked"},
		"host_predicates_known":            {"predicate"},
		"process_start_timestamp_seconds":  {},
		"sweep_complete_timestamp_seconds": {"reader", "family"},
	} {
		if got := GaugeLabels(name); !reflect.DeepEqual(got, want) {
			t.Errorf("%s: got %v, want %v", name, got, want)
		}
	}
	if GaugeLabels("not_a_gauge") != nil {
		t.Error("an unknown gauge has labels")
	}
}
