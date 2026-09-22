// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package util

import (
	"math"
	"reflect"
	"testing"
	"time"

	"github.com/google/uuid"
	"github.com/stretchr/testify/assert"
	"github.com/stretchr/testify/require"
)

func TestIntPtrToUint32Ptr(t *testing.T) {
	t.Run("nil in yields nil out", func(t *testing.T) {
		assert.Nil(t, IntPtrToUint32Ptr(nil))
	})

	cases := []struct {
		name string
		in   int
		want uint32
	}{
		{"zero", 0, 0},
		{"typical", 42, 42},
		{"max uint32", math.MaxUint32, math.MaxUint32},
	}
	for _, tc := range cases {
		t.Run(tc.name, func(t *testing.T) {
			got := IntPtrToUint32Ptr(&tc.in)
			require.NotNil(t, got)
			assert.Equal(t, tc.want, *got)
		})
	}
}

func TestUint32PtrToIntPtr(t *testing.T) {
	t.Run("nil in yields nil out", func(t *testing.T) {
		assert.Nil(t, Uint32PtrToIntPtr(nil))
	})

	cases := []struct {
		name string
		in   uint32
		want int
	}{
		{"zero", 0, 0},
		{"typical", 42, 42},
		{"max uint32 fits in int on 64-bit", math.MaxUint32, math.MaxUint32},
	}
	for _, tc := range cases {
		t.Run(tc.name, func(t *testing.T) {
			got := Uint32PtrToIntPtr(&tc.in)
			require.NotNil(t, got)
			assert.Equal(t, tc.want, *got)
		})
	}
}

func TestGetPtr(t *testing.T) {
	t.Run("string", func(t *testing.T) {
		s := "test"
		got := GetPtr(s)
		if *got != s {
			t.Errorf("GetPtr(string) = %v, want %v", *got, s)
		}
	})
	t.Run("int", func(t *testing.T) {
		i := 10
		got := GetPtr(i)
		if *got != i {
			t.Errorf("GetPtr(int) = %v, want %v", *got, i)
		}
	})
	t.Run("bool", func(t *testing.T) {
		b := true
		got := GetPtr(b)
		if *got != b {
			t.Errorf("GetPtr(bool) = %v, want %v", *got, b)
		}
	})
	t.Run("uuid.UUID", func(t *testing.T) {
		u := uuid.New()
		got := GetPtr(u)
		if !reflect.DeepEqual(got, &u) {
			t.Errorf("GetPtr(uuid.UUID) = %v, want %v", *got, u)
		}
	})
	t.Run("time.Time", func(t *testing.T) {
		ti := time.Now().UTC().Round(time.Microsecond)
		got := GetPtr(ti)
		if *got != ti {
			t.Errorf("GetPtr(time.Time) = %v, want %v", *got, ti)
		}
	})
}

func TestGetPtrIfNotZero(t *testing.T) {
	tests := []struct {
		name    string
		value   string
		wantNil bool
	}{
		{name: "zero value", wantNil: true},
		{name: "non-zero value", value: "test"},
	}

	for _, test := range tests {
		t.Run(test.name, func(t *testing.T) {
			got := GetPtrIfNotZero(test.value)

			if test.wantNil {
				require.Nil(t, got)

				return
			}

			require.NotNil(t, got)
			require.Equal(t, test.value, *got)
		})
	}
}

func TestGetValueOrZero(t *testing.T) {
	tests := []struct {
		name  string
		value *string
		want  string
	}{
		{name: "nil"},
		{name: "pointer to zero value", value: GetPtr("")},
		{name: "pointer to non-zero value", value: GetPtr("test"), want: "test"},
	}

	for _, test := range tests {
		t.Run(test.name, func(t *testing.T) {
			require.Equal(t, test.want, GetValueOrZero(test.value))
		})
	}
}
