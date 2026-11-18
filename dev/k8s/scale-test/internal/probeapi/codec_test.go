// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package probeapi

import (
	"encoding/json"
	"errors"
	"math"
	"strings"
	"testing"
	"time"

	"github.com/NVIDIA/infra-controller/dev/k8s/scale-test/internal/jsonx"
)

const secret = "s3cr3t-token"

func count(snapshotID string) string {
	return `{"count":1,"snapshotId":"` + snapshotID + `","family":"host","sweepStart":"2026-10-23T10:00:00Z",` +
		`"readAt":"2026-10-23T10:00:00Z","sweepAgeSeconds":0,"sweepIntervalSeconds":1}`
}

func metrics(selector, intervals string) string {
	return `{"nicoApi":null,"rest":null,"restSite":null,"metrics":{"contractVersion":1,"selector":{` + selector +
		`},"scrapeIntervalSeconds":1,"readerIntervalsSeconds":{` + intervals + `},"processStartSeconds":1}}`
}

// A comma fraction time (which Go's time.Parse takes but chrono rejects) is
// refused, at the field that held it, so the two decoders stay aligned.
func TestCommaFractionTimeIsRejectedAtItsField(t *testing.T) {
	body := strings.Replace(count("s"), `"readAt":"2026-10-23T10:00:00Z"`, `"readAt":"2026-10-23T10:00:00,500Z"`, 1)
	var de *jsonx.DecodeError
	if err := jsonx.Decode([]byte(body), new(CountResponse)); !errors.As(err, &de) || de.Path != "readAt" {
		t.Errorf("got %v, want a readAt decode error", err)
	}
}

// Each row decodes body into v. want is the JSON it then encodes to, or ""
// when Decode must refuse it, with an error that holds no input.
func TestDecodeRules(t *testing.T) {
	for _, tc := range []struct {
		name string
		v    any
		body string
		want string
	}{
		// Replies skip unknown keys, also one that differs only by case.
		{"reply skips unknown keys", new(CountResponse),
			strings.Replace(count("s"), `"count":1`, `"count":1,"COUNT":9,"new":{"a":[1,{"b":null}]}`, 1), count("s")},
		{"request refuses unknown key", new(UnitsQuery), `{"type":"host","` + secret + `":1}`, ""},
		{"request refuses case-only key", new(UnitsQuery), `{"type":"host","Done":false}`, ""},
		{"filter refuses unknown key", new(Filter), `{"type":"host","bogus":1}`, ""},
		{"older config reads as unknown", new(ConfigResponse), `{"nicoApi":null,"rest":null}`,
			`{"nicoApi":null,"rest":null,"restSite":null}`},
		{"map keys", new(ConfigResponse), metrics(`"job":"a","ns":"a"`, `"host":1,"dpu":2`), metrics(`"job":"a","ns":"a"`, `"host":1,"dpu":2`)},
		// serde reads a repeated map key as the last one. A repeated
		// selector label is ambiguous, so the runner refuses it.
		{"repeated selector label", new(ConfigResponse), metrics(`"job":"a","job":"b"`, `"host":1`), ""},
		{"repeated family key", new(ConfigResponse), metrics(`"job":"a"`, `"host":1,"host":2`), ""},
		{"unknown family key", new(ConfigResponse), metrics(`"job":"a"`, `"`+secret+`":1`), ""},
		{"unknown enum value", new(CountResponse), strings.Replace(count("s"), `"host"`, `"`+secret+`"`, 1), ""},
		{"string count", new(CountResponse), strings.Replace(count("s"), `"count":1`, `"count":"`+secret+`"`, 1), ""},
		{"malformed", new(CountResponse), `{"count":1,` + secret + `}`, ""},
		{"bytes not UTF-8", new(CountResponse), count("\xff"), ""},
		{"high surrogate then text", new(CountResponse), count(`\ud800x`), ""},
		{"high surrogate then other escape", new(CountResponse), count(`\ud800\u0041`), ""},
		{"two high surrogates", new(CountResponse), count(`\ud800\ud800`), ""},
		{"surrogate pair", new(CountResponse), count(`\ud83d\ude80`), count("🚀")},
		{"escaped backslash before u", new(CountResponse), count(`\\ud800`), count(`\\ud800`)},
		{"other escapes", new(CountResponse), count(`\u00e9\n`), count("é\\n")},
		{"count past 2^53 stays exact", new(CountResponse), strings.Replace(count("s"), `"count":1`, `"count":9007199254740993`, 1),
			strings.Replace(count("s"), `"count":1`, `"count":9007199254740993`, 1)},
		// Signed and unsigned bounds are in the shared vectors.
		{"negative uint32", new(UnitsQuery), `{"type":"host","limit":-1}`, ""},
	} {
		err := jsonx.Decode([]byte(tc.body), tc.v)
		switch {
		case tc.want == "" && err == nil:
			t.Errorf("%s: accepted", tc.name)
		case tc.want == "" && strings.Contains(err.Error(), secret):
			t.Errorf("%s: error %q holds input", tc.name, err)
		case tc.want != "" && err != nil:
			t.Errorf("%s: %v", tc.name, err)
		case tc.want != "":
			out, err := jsonx.Encode(tc.v)
			if err != nil || !sameJSON(parseJSON(t, out), parseJSON(t, []byte(tc.want))) {
				t.Errorf("%s: wrote %s, %v; want %s", tc.name, out, err, tc.want)
			}
		}
	}
}

func TestEncodingJSONUsesDecode(t *testing.T) {
	var c CountResponse
	err := json.Unmarshal([]byte(strings.Replace(count("s"), `"sweepAgeSeconds":0,`, "", 1)), &c)
	if err == nil || !strings.Contains(err.Error(), "sweepAgeSeconds is missing") {
		t.Errorf("got %v, want sweepAgeSeconds is missing", err)
	}
}

// Each row encodes v. want is the exact JSON, or "" when Encode must refuse
// with an error that names the field.
func TestEncodeRules(t *testing.T) {
	at := jsonx.Time{Time: time.Date(2026, 10, 23, 10, 0, 0, 0, time.UTC)}
	sweep := func(s SweepInfo) CountResponse {
		s.SweepStart, s.ReadAt = at, at
		return CountResponse{Count: 1, SweepInfo: s}
	}
	done := false
	for _, tc := range []struct {
		name, field string
		v           any
		want        string
	}{
		{"no HTML escapes", "", sweep(SweepInfo{SnapshotID: "<&>", Family: FamilyHost}),
			`{"count":1,"snapshotId":"<&>","family":"host","sweepStart":"2026-10-23T10:00:00Z","readAt":"2026-10-23T10:00:00Z","sweepAgeSeconds":0,"sweepIntervalSeconds":0}`},
		{"request leaves out unset fields", "", UnitsQuery{Filter: Filter{Type: UnitTypeHost, Done: &done}}, `{"type":"host","done":false}`},
		{"NaN", "sweepAgeSeconds", sweep(SweepInfo{Family: FamilyHost, SweepAgeSeconds: math.NaN()}), ""},
		{"negative zero", "sweepAgeSeconds", sweep(SweepInfo{Family: FamilyHost, SweepAgeSeconds: math.Copysign(0, -1)}), ""},
		{"infinity", "sweepIntervalSeconds", sweep(SweepInfo{Family: FamilyHost, SweepIntervalSeconds: math.Inf(-1)}), ""},
		{"unknown enum", "family", sweep(SweepInfo{Family: "hosts"}), ""},
		{"empty enum", "family", sweep(SweepInfo{}), ""},
		{"not UTF-8", "snapshotId", sweep(SweepInfo{Family: FamilyHost, SnapshotID: "\xff"}), ""},
		{"nil required slice", "units", UnitsResponse{SweepInfo: sweep(SweepInfo{Family: FamilyHost}).SweepInfo, MissingIDs: []string{}}, ""},
		{"year before 0000", "", CountResponse{SweepInfo: SweepInfo{Family: FamilyHost, SweepStart: jsonx.Time{Time: time.Date(-1, 1, 1, 0, 0, 0, 0, time.UTC)}}}, ""},
		{"year after 9999", "", CountResponse{SweepInfo: SweepInfo{Family: FamilyHost, SweepStart: jsonx.Time{Time: time.Date(10000, 1, 1, 0, 0, 0, 0, time.UTC)}}}, ""},
	} {
		out, err := jsonx.Encode(tc.v)
		if string(out) != tc.want || (err == nil) != (tc.want != "") || err != nil && !strings.Contains(err.Error(), tc.field) {
			t.Errorf("%s: wrote %s, %v; want %s", tc.name, out, err, tc.want)
		}
	}
}

// The shared vectors hold times with 0, 3, 6 and 9 fraction digits.
func TestTimeWritesAsChrono(t *testing.T) {
	base := time.Date(2026, 10, 23, 10, 0, 0, 0, time.UTC)
	for at, want := range map[time.Time]string{
		base.In(time.FixedZone("east", 7200)):                      `"2026-10-23T10:00:00Z"`,
		time.Date(9999, 12, 31, 23, 59, 59, 999_999_999, time.UTC): `"9999-12-31T23:59:59.999999999Z"`,
	} {
		if got, err := (jsonx.Time{Time: at}).MarshalJSON(); string(got) != want || err != nil {
			t.Errorf("%v: got %s, %v; want %s", at, got, err, want)
		}
	}
}

func TestSameJSONComparesIntegersExactly(t *testing.T) {
	if sameJSON(parseJSON(t, []byte(`18446744073709551615`)), parseJSON(t, []byte(`18446744073709551614`))) {
		t.Error("u64::MAX equals u64::MAX - 1")
	}
	if !sameJSON(parseJSON(t, []byte(`[120, 1e21, 0.5]`)), parseJSON(t, []byte(`[120.0, 1e+21, 5e-1]`))) {
		t.Error("equal numbers in other forms differ")
	}
}

func TestQuerySetsOnlySetFields(t *testing.T) {
	done, model, stage := false, "GB200 NVL", StageReady
	q, err := Filter{Type: UnitTypeHost, Stage: &stage, Model: &model, Done: &done}.Query()
	if got, want := q.Encode(), "done=false&model=GB200+NVL&stage=ready&type=host"; got != want || err != nil {
		t.Errorf("got %s, %v; want %s", got, err, want)
	}
	if _, err := (Filter{Type: "hosts"}).Query(); err == nil {
		t.Error("an unknown type made a query")
	}
}
