// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package jsonx

import (
	"bytes"
	"cmp"
	"encoding/json"
	"errors"
	"fmt"
	"math"
	"reflect"
	"slices"
	"unicode/utf8"
)

// Encode writes v as compact JSON with encoding/json, without HTML escapes.
// Fields keep Rust order and nil pointers write null, so the value reads the
// same in Rust. Encode refuses what the Rust type cannot hold: a float that
// is not finite, an enum value outside its list, a string that is not UTF-8
// (encoding/json would replace it), a nil slice or map that is not
// omitempty (serde writes [] or {}, encoding/json null) and a time outside
// the years 0000 to 9999, and negative zero: callers normalize the zeros
// they compute, so no -0 is written.
func Encode(v any) ([]byte, error) {
	if err := check("", reflect.ValueOf(v)); err != nil {
		return nil, err
	}
	var b bytes.Buffer
	enc := json.NewEncoder(&b)
	enc.SetEscapeHTML(false)
	if err := enc.Encode(v); err != nil {
		return nil, err
	}
	return bytes.TrimSuffix(b.Bytes(), []byte("\n")), nil
}

func check(path string, v reflect.Value) error {
	bad := func(reason string) error {
		return fmt.Errorf("jsonx: cannot encode %s: %s", cmp.Or(path, "value"), reason)
	}
	switch v.Kind() {
	case reflect.Pointer, reflect.Interface:
		if !v.IsNil() {
			return check(path, v.Elem())
		}
	case reflect.Float64:
		if f := v.Float(); math.IsNaN(f) || math.IsInf(f, 0) || f == 0 && math.Signbit(f) {
			return bad("not finite, or negative zero")
		}
	case reflect.String:
		if values, ok := enumValues[v.Type()]; ok && !slices.Contains(values, v.String()) {
			return bad("not a known value")
		}
		if !utf8.ValidString(v.String()) {
			return bad("not UTF-8")
		}
	case reflect.Slice:
		for i := range v.Len() {
			if err := check(fmt.Sprintf("%s[%d]", path, i), v.Index(i)); err != nil {
				return err
			}
		}
	case reflect.Map:
		for i, iter := 0, v.MapRange(); iter.Next(); i++ {
			entry := fmt.Sprintf("%s{%d}", path, i)
			if err := errors.Join(check(entry, iter.Key()), check(entry, iter.Value())); err != nil {
				return err
			}
		}
	case reflect.Struct:
		if v.Type() == timeType {
			return nil // Time.MarshalJSON checks the year.
		}
		for _, f := range structInfoOf(v.Type()).fields {
			fv := v.FieldByIndex(f.index)
			if !f.omitEmpty && (fv.Kind() == reflect.Slice || fv.Kind() == reflect.Map) && fv.IsNil() {
				return fmt.Errorf("jsonx: cannot encode %s: nil", join(path, f.name))
			}
			if err := check(join(path, f.name), fv); err != nil {
				return err
			}
		}
	}
	return nil
}
