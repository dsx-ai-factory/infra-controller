// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package probeapi

import (
	"fmt"
	"maps"
	"reflect"
	"slices"
	"strings"
	"unicode"
)

// The probe's metric contract: the series a count reads back from the
// metrics store (metrics.rs in the Rust crate). v1 reads only the fixed
// ingestion-stage filters.

const (
	// Prefix starts every probe metric name.
	Prefix = "carbide_site_health_probe_"

	MetricSweepStart    = Prefix + "sweep_start_timestamp_seconds"    // labels reader, family
	MetricSweepComplete = Prefix + "sweep_complete_timestamp_seconds" // labels reader, family
	MetricProcessStart  = Prefix + "process_start_timestamp_seconds"  // no labels
	MetricSweepsTotal   = Prefix + "sweeps_total"                     // labels reader, family, result
)

// TimeDims are the labels of the sweep times and counters that name the
// sweep. Every other label on those series names the scrape source.
var TimeDims = [3]string{"reader", "family", "result"}

// Need is the one capability a v1 filter needs: the host is allocatable, read
// from the compact host read. A sweep meets it only where its gauge is 1.
type Need struct{}

// Capability is the series that says whether a sweep met a Need: it is met
// only where Gauge{Label=Value} is exactly 1. A missing series is unmet,
// never a known zero.
type Capability struct{ Gauge, Label, Value string }

func (Need) Capability() Capability {
	return Capability{Prefix + "host_predicates_known", "predicate", "allocatable"}
}

// SeriesSpec is the gauge and label values a count reads. Matchers are label
// equality pairs in order.
type SeriesSpec struct {
	Metric   string // without Prefix
	Family   Family
	Matchers [][2]string
	Needs    []Need
}

// FilterError is why a filter cannot be answered: UnsupportedFilter for a
// combination v1 does not count, FieldUnavailable for a field the probe
// cannot read yet.
type FilterError struct {
	Code   ErrorCode
	Field  string
	Reason string
}

func (e *FilterError) Error() string { return e.Field + ": " + e.Reason }

// SeriesSpecFor is the series a count of f reads, with fullHostRead when
// hosts are read with the full machine record. v1 supports the ten fixed
// ingestion-stage filters only: expected records linked, a unit type at a
// stage, and the host Ready-and-allocatable gate.
func SeriesSpecFor(f Filter, fullHostRead bool) (SeriesSpec, error) {
	switch {
	case f.Done != nil:
		return SeriesSpec{}, unsupported("done", "done is a /v1/units filter")
	case f.Source != nil && *f.Source != SourceNicoAPI:
		return SeriesSpec{}, unsupported("source", "v1 counts the native NICo source only")
	}
	switch f.Type {
	case UnitTypeExpectedRecord:
		if err := only(f, "recordType", "linked"); err != nil {
			return SeriesSpec{}, err
		}
		if f.RecordType == nil {
			return SeriesSpec{}, unsupported("recordType", "type=expected_record needs recordType")
		}
		return SeriesSpec{"expected_records", FamilyExpected, with(with(nil, "type", f.RecordType), "linked", f.Linked), nil}, nil
	case UnitTypeHost:
		return hostSpec(f, fullHostRead)
	case UnitTypeDpu, UnitTypeSwitch, UnitTypePowerShelf, UnitTypeRack:
		if f.Stage != nil && *f.Stage == StageExpected {
			return expectedStageSpec(f)
		}
		if err := only(f, "stage", "state"); err != nil {
			return SeriesSpec{}, err
		}
		return unitsSpec(f), nil
	}
	return SeriesSpec{}, unsupported("type", "type="+string(f.Type)+" is not an ingestion-stage filter")
}

// hostSpec is the host stage filter. allocatable is a capability only the
// compact host read answers, so the full host read fails it at once, before
// stage=expected routes to expectedStageSpec (mirrors the Rust host_spec
// order). Ready-and-allocatable reads the hosts gauge and needs the
// allocatable capability; any other host stage reads the units gauge.
func hostSpec(f Filter, fullHostRead bool) (SeriesSpec, error) {
	if f.Allocatable != nil && fullHostRead {
		return SeriesSpec{}, unavailable("allocatable", "allocatability is unknown until nico-api serves the compact host read")
	}
	if f.Stage != nil && *f.Stage == StageExpected {
		return expectedStageSpec(f)
	}
	if f.Allocatable == nil {
		if err := only(f, "stage", "state"); err != nil {
			return SeriesSpec{}, err
		}
		return unitsSpec(f), nil
	}
	if err := only(f, "stage", "allocatable"); err != nil {
		return SeriesSpec{}, err
	}
	var matchers [][2]string
	if f.Stage != nil && *f.Stage == StageReady {
		matchers = append(matchers, [2]string{"state", "Ready"})
	} else if f.Stage != nil && *f.Stage == StageError {
		matchers = append(matchers, [2]string{"state", "Failed"})
	}
	return SeriesSpec{"hosts", FamilyHost, with(matchers, "allocatable", f.Allocatable), []Need{{}}}, nil
}

// expectedStageSpec counts a device type's expected records, the series
// stage=expected reads (mirrors the Rust expected_stage_spec). Only stage is
// allowed, so allocatable with stage=expected is unsupported; a type with no
// expected records (dpu, instance) is unsupported too.
func expectedStageSpec(f Filter) (SeriesSpec, error) {
	if err := only(f, "stage"); err != nil {
		return SeriesSpec{}, err
	}
	switch f.Type {
	case UnitTypeHost, UnitTypeSwitch, UnitTypePowerShelf, UnitTypeRack:
		return SeriesSpec{"expected_records", FamilyExpected, [][2]string{{"type", string(f.Type)}}, nil}, nil
	}
	return SeriesSpec{}, unsupported("stage", "type="+string(f.Type)+" has no expected records")
}

// unitsSpec counts a unit type at a stage from the units gauge.
func unitsSpec(f Filter) SeriesSpec {
	stage := StageRegistered
	if f.Stage != nil {
		stage = *f.Stage
	}
	return SeriesSpec{"units", Family(f.Type), with([][2]string{{"type", string(f.Type)}, {"stage", string(stage)}}, "state", f.State), nil}
}

// GaugeLabels is the label names of the gauge name (without Prefix), in the
// probe's order, or nil for an unknown gauge.
func GaugeLabels(name string) []string { return gauges[name] }

// SelectorProblems is why selector cannot select the probe's series, one
// text per problem, empty when it can. A text names the problem and never
// holds a label name or value.
func SelectorProblems(selector map[string]string) []string {
	var problems []string
	if len(selector) == 0 {
		problems = append(problems, "must name at least one label")
	}
	intrinsic := slices.Concat(TimeDims[:], []string{"metric"})
	for _, labels := range gauges {
		intrinsic = append(intrinsic, labels...)
	}
	for _, name := range slices.Sorted(maps.Keys(selector)) {
		switch {
		case name == "" || name[0] >= '0' && name[0] <= '9' || strings.ContainsFunc(name, func(r rune) bool {
			return r > unicode.MaxASCII || r != '_' && !unicode.IsLetter(r) && !unicode.IsDigit(r)
		}):
			problems = append(problems, "a label is not a label name")
		case strings.HasPrefix(name, "__"):
			problems = append(problems, "a label starting with __ is reserved")
		case slices.Contains(intrinsic, name):
			problems = append(problems, "a label the probe's series carry is set")
		}
		switch value := selector[name]; {
		case strings.TrimSpace(value) == "":
			problems = append(problems, "a value is blank")
		case strings.ContainsFunc(value, unicode.IsControl):
			problems = append(problems, "a value holds control characters")
		case len(value) > maxSelectorValue:
			problems = append(problems, "a value is longer than 256 bytes")
		}
	}
	return problems
}

// maxSelectorValue is the longest selector value, in bytes. Kubernetes names
// are at most 63.
const maxSelectorValue = 256

// gauges is the labels of every probe gauge, in the probe's order. It
// mirrors the Rust metric contract so SelectorProblems and GaugeLabels agree
// with it.
var gauges = map[string][]string{
	"units":                            {"type", "stage", "state"},
	"hosts":                            {"model", "instance_type", "state", "allocatable", "update_complete"},
	"hosts_update_eligible":            {"model", "eligible", "reason"},
	"host_firmware_requests":           {"model", "initiator"},
	"unresolved_units":                 {"type"},
	"expected_records":                 {"type", "linked"},
	"identity_mismatches":              {"type", "kind"},
	"rest_totals":                      {"resource"},
	"rest_totals_available":            {"resource"},
	"rest_site_match":                  {"result"},
	"sweep_start_timestamp_seconds":    {"reader", "family"},
	"sweep_complete_timestamp_seconds": {"reader", "family"},
	"sweep_duration_seconds":           {"reader", "family"},
	"last_attempt_failed":              {"reader", "family", "kind"},
	"process_start_timestamp_seconds":  {},
	"host_read_mode":                   {"mode"},
	"host_predicates_known":            {"predicate"},
}

func unsupported(field, reason string) error {
	return &FilterError{ErrorCodeUnsupportedFilter, field, reason}
}

func unavailable(field, reason string) error {
	return &FilterError{ErrorCodeFieldUnavailable, field, reason}
}

// setFields is the JSON names of the fields of f that are set, type aside,
// sorted as serde_json writes them.
func setFields(f Filter) []string {
	var set []string
	v := reflect.ValueOf(f)
	for i := 1; i < v.NumField(); i++ {
		if !v.Field(i).IsNil() {
			set = append(set, strings.Split(v.Type().Field(i).Tag.Get("json"), ",")[0])
		}
	}
	slices.Sort(set)
	return set
}

// only rejects every set field outside allowed. The native source is always
// allowed, like the Rust only(): it is the one source v1 counts and SeriesSpecFor
// has already rejected any other.
func only(f Filter, allowed ...string) error {
	for _, field := range setFields(f) {
		if field != "source" && !slices.Contains(allowed, field) {
			return unsupported(field, field+" is not a filter of type="+string(f.Type))
		}
	}
	return nil
}

// with appends label=v when v is set.
func with[T ~string | bool](matchers [][2]string, label string, v *T) [][2]string {
	if v == nil {
		return matchers
	}
	return append(matchers, [2]string{label, fmt.Sprint(*v)})
}
