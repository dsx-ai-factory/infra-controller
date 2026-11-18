// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package probeapi

import (
	"bytes"
	"encoding/json"
	"math/big"
	"os"
	"path/filepath"
	"reflect"
	"slices"
	"strings"
	"testing"

	"github.com/NVIDIA/infra-controller/dev/k8s/scale-test/internal/jsonx"
)

// testdata holds the compact golden vectors the Rust test
// crates/site-health-probe-api/tests/golden_vectors.rs writes and reads.
const testdata = "../../../../../crates/site-health-probe-api/testdata"

// wireTypes maps the <type> of a vector file, <type>.<case>.json, to a new
// value of its Go type. reply is true for a reply, which skips unknown
// fields; a request is closed.
var wireTypes = map[string]struct {
	mk    func() any
	reply bool
}{
	"count-response":  {func() any { return new(CountResponse) }, true},
	"units-response":  {func() any { return new(UnitsResponse) }, true},
	"units-query":     {func() any { return new(UnitsQuery) }, false},
	"config-response": {func() any { return new(ConfigResponse) }, true},
	"error-body":      {func() any { return new(ErrorBody) }, true},
	"error-bodies":    {func() any { return new([]ErrorBody) }, false},
}

func vectors(t *testing.T, dir string) map[string][]byte {
	t.Helper()
	entries, err := os.ReadDir(filepath.Join(testdata, dir))
	if err != nil || len(entries) == 0 {
		t.Fatalf("no vectors in %s: %v", dir, err)
	}
	files := map[string][]byte{}
	for _, e := range entries {
		if !e.IsDir() {
			files[e.Name()] = readFile(t, filepath.Join(dir, e.Name()))
		}
	}
	return files
}

func readFile(t *testing.T, name string) []byte {
	t.Helper()
	data, err := os.ReadFile(filepath.Join(testdata, name))
	if err != nil {
		t.Fatal(err)
	}
	return data
}

func wireType(t *testing.T, name string) (func() any, bool) {
	t.Helper()
	typ, _, _ := strings.Cut(name, ".")
	w, ok := wireTypes[typ]
	if !ok {
		t.Fatalf("%s: no Go type for this vector", name)
	}
	return w.mk, w.reply
}

func parseJSON(t *testing.T, data []byte) any {
	t.Helper()
	dec := json.NewDecoder(bytes.NewReader(data))
	dec.UseNumber()
	var v any
	if err := dec.Decode(&v); err != nil {
		t.Fatalf("%v in %s", err, data)
	}
	return v
}

// sameJSON compares JSON values, numbers by exact decimal value and never
// through float64: 120 equals 120.0, and integers past 2^53 stay exact.
func sameJSON(a, b any) bool {
	switch a := a.(type) {
	case map[string]any:
		b, ok := b.(map[string]any)
		if !ok || len(a) != len(b) {
			return false
		}
		for k, av := range a {
			if bv, ok := b[k]; !ok || !sameJSON(av, bv) {
				return false
			}
		}
		return true
	case []any:
		b, ok := b.([]any)
		return ok && slices.EqualFunc(a, b, sameJSON)
	case json.Number:
		b, ok := b.(json.Number)
		ra, okA := new(big.Rat).SetString(string(a))
		rb, okB := new(big.Rat).SetString(string(b))
		return ok && okA && okB && ra.Cmp(rb) == 0
	}
	return a == b
}

// Every reply and request reads and writes back as the same JSON.
func TestVectorsRoundTrip(t *testing.T) {
	seen := map[string]bool{}
	for name, data := range vectors(t, "v1") {
		mk, _ := wireType(t, name)
		v := mk()
		if err := jsonx.Decode(data, v); err != nil {
			t.Errorf("v1/%s: %v", name, err)
			continue
		}
		out, err := jsonx.Encode(v)
		if err != nil {
			t.Fatalf("v1/%s: %v", name, err)
		}
		if !sameJSON(parseJSON(t, out), parseJSON(t, data)) {
			t.Errorf("v1/%s read as another value:\n%s", name, out)
		}
		seen[strings.Split(name, ".")[0]] = true
	}
	for typ := range wireTypes {
		if !seen[typ] {
			t.Errorf("no vector of %s", typ)
		}
	}
}

// A reply with fields a newer probe could add reads as the reply without
// them. The extra fields are injected here, so no extra files are kept.
func TestAdditiveFieldsAreIgnored(t *testing.T) {
	const extra = `"zzzAddedByANewerProbe":[1,{"nested":null},"x"],`
	for name, data := range vectors(t, "v1") {
		mk, reply := wireType(t, name)
		if !reply || !bytes.HasPrefix(bytes.TrimSpace(data), []byte("{")) {
			continue // requests are closed; a top-level array takes no field
		}
		aug := bytes.Replace(data, []byte("{"), []byte("{"+extra), 1)
		base, with := mk(), mk()
		if err := jsonx.Decode(data, base); err != nil {
			t.Fatalf("v1/%s: %v", name, err)
		}
		if err := jsonx.Decode(aug, with); err != nil {
			t.Errorf("v1/%s with an added field: %v", name, err)
			continue
		}
		if !reflect.DeepEqual(base, with) {
			t.Errorf("v1/%s read differently with an added field", name)
		}
	}
}

func TestInvalidVectorsAreRefused(t *testing.T) {
	for name, data := range vectors(t, "v1/invalid") {
		mk, _ := wireType(t, name)
		if err := jsonx.Decode(data, mk()); err == nil {
			t.Errorf("%s was accepted", name)
		}
	}
}

// The enum values match the file the Rust crate writes.
func TestEnumsMatchRust(t *testing.T) {
	var enums map[string][]string
	if err := json.Unmarshal(readFile(t, "enums.json"), &enums); err != nil {
		t.Fatal(err)
	}
	got := map[string][]string{}
	for typ, values := range jsonx.Enums() {
		if typ.PkgPath() == reflect.TypeFor[Filter]().PkgPath() {
			got[typ.Name()] = values
		}
	}
	if !reflect.DeepEqual(got, enums) {
		t.Errorf("Go enums differ from Rust:\n got %v\nwant %v", got, enums)
	}
}
