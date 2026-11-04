// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

// Package jsonx reads and writes JSON by the serde rules of the Rust types
// the runner shares with the probe and the result contract.
package jsonx

import (
	"bytes"
	"cmp"
	"encoding/json"
	"errors"
	"fmt"
	"io"
	"maps"
	"reflect"
	"slices"
	"strconv"
	"strings"
	"sync"
	"time"
	"unicode/utf8"
)

// DecodeError is why a value does not read as its type. Path names the
// field by wire name and index. The text never holds input, so a reply
// cannot carry its own text, a URL or a secret into an error.
type DecodeError struct{ Path, Reason string }

func (e *DecodeError) Error() string { return "jsonx: " + cmp.Or(e.Path, "value") + " " + e.Reason }

func fail(path, reason string) error { return &DecodeError{Path: path, Reason: reason} }

// Decode reads exactly one JSON value into v, a non-nil pointer, by the
// serde rules of the Rust types, at every depth:
//   - A non-pointer field is required: missing or null is an error. A pointer
//     field (Rust Option) reads missing or null as nil. An omitempty field
//     (serde default) may be missing, but not null.
//   - Keys match exactly, case included. A field or map key given twice is
//     an error.
//     Unknown keys are skipped, unless the type has a JSONClosed method, and
//     a repeat of a skipped key is not checked, as in serde.
//   - Enum values outside their list are errors.
//   - Integers must fit their type, with a sign only for int64, and never
//     pass through float64. Floats must fit float64.
//   - Bytes that are not UTF-8 and lone UTF-16 surrogate escapes are errors,
//     where encoding/json would put U+FFFD in their place.
//   - Times are RFC 3339 with a zone, read into UTC.
func Decode(data []byte, v any) error {
	rv := reflect.ValueOf(v)
	if rv.Kind() != reflect.Pointer || rv.IsNil() {
		return errors.New("jsonx: Decode needs a non-nil pointer")
	}
	if !utf8.Valid(data) {
		return fail("", "is not UTF-8")
	}
	if !surrogatesPaired(data) {
		return fail("", "has a lone UTF-16 surrogate escape")
	}
	d := decoder{json.NewDecoder(bytes.NewReader(data))}
	d.UseNumber()
	if err := d.value("", rv.Elem()); err != nil {
		return err
	}
	if _, err := d.Token(); !errors.Is(err, io.EOF) {
		return fail("", "has data after it")
	}
	return nil
}

type decoder struct{ *json.Decoder }

// token reads the next token, with syntax errors reduced to their offset.
func (d decoder) token(path string) (json.Token, error) {
	tok, err := d.Token()
	var syntax *json.SyntaxError
	switch {
	case err == nil:
		return tok, nil
	case errors.As(err, &syntax):
		return nil, fail(path, fmt.Sprintf("is malformed JSON at byte %d", syntax.Offset))
	case errors.Is(err, io.EOF), errors.Is(err, io.ErrUnexpectedEOF):
		return nil, fail(path, "ends early")
	}
	return nil, fail(path, "is malformed JSON")
}

var (
	timeType  = reflect.TypeFor[Time]()
	errOffset = errors.New("offset out of range")
)

var enumValues = map[reflect.Type][]string{}

// Enum registers the wire values of an enum type, in Rust declaration
// order. Decode and Encode refuse a value outside them. Call it from init.
func Enum[T ~string](values ...T) {
	for _, v := range values {
		enumValues[reflect.TypeFor[T]()] = append(enumValues[reflect.TypeFor[T]()], string(v))
	}
}

// Enums returns the registered enum values by type.
func Enums() map[reflect.Type][]string { return maps.Clone(enumValues) }

// Time is an RFC 3339 time, read into UTC and written as chrono writes it:
// Z with 0, 3, 6 or 9 fraction digits.
type Time struct{ time.Time }

func (t Time) MarshalJSON() ([]byte, error) {
	u := t.UTC()
	if u.Year() < 0 || u.Year() > 9999 {
		return nil, errors.New("jsonx: a time outside the years 0000 to 9999 has no RFC 3339 form")
	}
	layout := "2006-01-02T15:04:05"
	switch ns := u.Nanosecond(); {
	case ns == 0:
	case ns%1_000_000 == 0:
		layout += ".000"
	case ns%1_000 == 0:
		layout += ".000000"
	default:
		layout += ".000000000"
	}
	return []byte(`"` + u.Format(layout) + `Z"`), nil
}

// value reads the next JSON value into v.
func (d decoder) value(path string, v reflect.Value) error {
	tok, err := d.token(path)
	if err != nil {
		return err
	}
	if v.Kind() == reflect.Pointer {
		if tok == nil {
			v.SetZero()
			return nil
		}
		v.Set(reflect.New(v.Type().Elem()))
		v = v.Elem()
	}
	if tok == nil {
		return fail(path, "is null")
	}
	s, isString := tok.(string)
	n, isNumber := tok.(json.Number)
	switch kind := v.Kind(); {
	case v.Type() == timeType:
		t, err := time.Parse(time.RFC3339, s)
		// time.Parse takes an offset of +24:00 or +00:60. RFC 3339 does not.
		if z := s[max(0, len(s)-6):]; len(z) == 6 && strings.ContainsAny(z[:1], "+-") && (z[1:3] > "23" || z[4:6] > "59") {
			err = errOffset
		}
		// time.Parse takes a comma as the fraction separator; chrono and RFC
		// 3339 take only a dot, so a comma reply must not read as a time.
		if strings.IndexByte(s, ',') >= 0 {
			err = errOffset
		}
		if !isString || err != nil {
			return fail(path, "is not an RFC 3339 time")
		}
		v.Set(reflect.ValueOf(Time{t.UTC()}))
	case kind == reflect.String:
		if !isString {
			return fail(path, "is not a string")
		}
		if values, ok := enumValues[v.Type()]; ok && !slices.Contains(values, s) {
			return fail(path, "is not a known value")
		}
		v.SetString(s)
	case kind == reflect.Bool:
		b, ok := tok.(bool)
		if !ok {
			return fail(path, "is not a bool")
		}
		v.SetBool(b)
	case kind == reflect.Uint32 || kind == reflect.Uint64:
		u, err := strconv.ParseUint(string(n), 10, v.Type().Bits())
		if !isNumber || err != nil {
			return fail(path, fmt.Sprintf("is not a whole number in uint%d", v.Type().Bits()))
		}
		v.SetUint(u)
	case kind == reflect.Int || kind == reflect.Int64:
		i, err := strconv.ParseInt(string(n), 10, v.Type().Bits())
		if !isNumber || err != nil {
			return fail(path, fmt.Sprintf("is not a whole number in int%d", v.Type().Bits()))
		}
		v.SetInt(i)
	case kind == reflect.Float64:
		f, err := strconv.ParseFloat(string(n), 64)
		if !isNumber || err != nil {
			return fail(path, "is not a number in the float64 range")
		}
		v.SetFloat(f)
	case kind == reflect.Slice && tok == json.Delim('['):
		v.Set(reflect.MakeSlice(v.Type(), 0, 0))
		for i := 0; d.More(); i++ {
			elem := reflect.New(v.Type().Elem()).Elem()
			if err := d.value(fmt.Sprintf("%s[%d]", path, i), elem); err != nil {
				return err
			}
			v.Set(reflect.Append(v, elem))
		}
		_, err = d.token(path)
		return err
	case kind == reflect.Map && tok == json.Delim('{'):
		return d.mapValue(path, v)
	case kind == reflect.Struct && tok == json.Delim('{'):
		return d.object(path, v)
	case kind == reflect.Slice:
		return fail(path, "is not an array")
	default:
		return fail(path, "is not an object")
	}
	return nil
}

// mapValue reads the entries of an object with string keys. Entries are
// named by index in errors, as a key is input text.
func (d decoder) mapValue(path string, v reflect.Value) error {
	v.Set(reflect.MakeMap(v.Type()))
	for i := 0; d.More(); i++ {
		entry := fmt.Sprintf("%s{%d}", path, i)
		keyTok, err := d.token(entry)
		if err != nil {
			return err
		}
		key := reflect.New(v.Type().Key()).Elem()
		key.SetString(keyTok.(string))
		if values, ok := enumValues[key.Type()]; ok && !slices.Contains(values, key.String()) {
			return fail(entry, "has a key that is not a known value")
		}
		if v.MapIndex(key).IsValid() {
			return fail(entry, "repeats a key")
		}
		elem := reflect.New(v.Type().Elem()).Elem()
		if err := d.value(entry, elem); err != nil {
			return err
		}
		v.SetMapIndex(key, elem)
	}
	_, err := d.token(path)
	return err
}

func (d decoder) object(path string, v reflect.Value) error {
	info := structInfoOf(v.Type())
	v.SetZero()
	seen := make([]bool, len(info.fields))
	for d.More() {
		keyTok, err := d.token(path)
		if err != nil {
			return err
		}
		i, known := info.byName[keyTok.(string)]
		switch {
		case !known && info.rejectsUnknown:
			return fail(path, "has a field that is not in the contract")
		case !known:
			if err := d.skip(path); err != nil {
				return err
			}
			continue
		case seen[i]:
			return fail(join(path, info.fields[i].name), "is given twice")
		}
		seen[i] = true
		if err := d.value(join(path, info.fields[i].name), v.FieldByIndex(info.fields[i].index)); err != nil {
			return err
		}
	}
	if _, err := d.token(path); err != nil {
		return err
	}
	for i, f := range info.fields {
		if !seen[i] && !f.optional {
			return fail(join(path, f.name), "is missing")
		}
	}
	return nil
}

// skip reads past the value of an unknown field.
func (d decoder) skip(path string) error {
	for depth := 0; ; {
		tok, err := d.token(path)
		if err != nil {
			return err
		}
		switch tok {
		case json.Delim('{'), json.Delim('['):
			depth++
		case json.Delim('}'), json.Delim(']'):
			depth--
		}
		if depth == 0 {
			return nil
		}
	}
}

func join(path, name string) string {
	if path == "" {
		return name
	}
	return path + "." + name
}

// surrogatesPaired reports whether every UTF-16 surrogate escape in the
// strings of data is a high one followed by a low one. Malformed escapes are
// left for the decoder to report.
func surrogatesPaired(data []byte) bool {
	inString := false
	for i := 0; i < len(data); i++ {
		switch {
		case !inString:
			inString = data[i] == '"'
		case data[i] == '"':
			inString = false
		case data[i] == '\\':
			i++
			r, ok := escape(data, i)
			if !ok || r < 0xD800 || r > 0xDFFF {
				continue
			}
			// A high surrogate needs a low one in the next escape.
			low, ok := escape(data, i+6)
			if r > 0xDBFF || !ok || data[i+5] != '\\' || low < 0xDC00 || low > 0xDFFF {
				return false
			}
			i += 10
		}
	}
	return true
}

// escape reads the \uXXXX escape whose u is at data[at].
func escape(data []byte, at int) (rune, bool) {
	if at+5 > len(data) || data[at] != 'u' {
		return 0, false
	}
	n, err := strconv.ParseUint(string(data[at+1:at+5]), 16, 16)
	return rune(n), err == nil
}

// field is one wire field of a struct, embedded structs flattened.
type field struct {
	name      string
	index     []int
	optional  bool // a pointer, or omitempty (serde default)
	omitEmpty bool
}

type structInfo struct {
	fields         []field
	byName         map[string]int
	rejectsUnknown bool
}

var structInfos sync.Map

func structInfoOf(t reflect.Type) *structInfo {
	if info, ok := structInfos.Load(t); ok {
		return info.(*structInfo)
	}
	info := &structInfo{byName: map[string]int{}}
	info.rejectsUnknown = t.Implements(reflect.TypeFor[interface{ JSONClosed() }]())
	var walk func(t reflect.Type, index []int)
	walk = func(t reflect.Type, index []int) {
		for i := range t.NumField() {
			sf := t.Field(i)
			at := append(slices.Clone(index), i)
			if sf.Anonymous {
				walk(sf.Type, at)
				continue
			}
			name, opts, _ := strings.Cut(sf.Tag.Get("json"), ",")
			if _, dup := info.byName[name]; dup || name == "" {
				panic("jsonx: " + t.Name() + "." + sf.Name + " needs a unique json name")
			}
			info.byName[name] = len(info.fields)
			info.fields = append(info.fields, field{name, at, sf.Type.Kind() == reflect.Pointer || opts == "omitempty", opts == "omitempty"})
		}
	}
	walk(t, nil)
	actual, _ := structInfos.LoadOrStore(t, info)
	return actual.(*structInfo)
}
