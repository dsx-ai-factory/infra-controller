// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package util

import (
	"time"

	"google.golang.org/protobuf/types/known/timestamppb"
)

// IntPtrToUint32Ptr converts a `*int` to a `*uint32`. nil in, nil out.
// Callers must ensure the int sits in `[0, MaxUint32]`; the cast
// otherwise silently wraps. Under the proto-conversion convention,
// that bound is the responsibility of the request-side `Validate`
// (which rejects negatives and overflow with a 400) on the
// API-inbound path, or guaranteed by construction on the proto-inbound
// path (where values originate from a proto `uint32` field).
func IntPtrToUint32Ptr(i *int) *uint32 {
	if i == nil {
		return nil
	}
	u := uint32(*i) //nolint:gosec // bounded upstream by Validate / proto-source.
	return &u
}

// Uint32PtrToIntPtr converts a `*uint32` to a `*int`. nil in, nil out.
// The cast is always safe on 64-bit platforms (the only ones we target);
// on a hypothetical 32-bit build it would wrap on values above
// `MaxInt32`. Under the proto-conversion convention this is a trusted
// cast used inside `FromProto` mappers — the upstream value originates
// from a proto `uint32` field, and any bounds checks that would gate it
// belong in `Validate` upstream.
func Uint32PtrToIntPtr(u *uint32) *int {
	if u == nil {
		return nil
	}
	i := int(*u) //nolint:gosec // bounded by uint32 range; safe on 64-bit targets.
	return &i
}

// GetPtr returns a pointer to a copy of v. Generic helper that replaces
// the per-type `GetStrPtr` / `GetIntPtr` / `GetBoolPtr` / `GetUUIDPtr` /
// `GetTimePtr` helpers that previously lived in `db/pkg/db`.
func GetPtr[T any](v T) *T {
	return &v
}

// GetPtrIfNotZero returns nil for a type's zero value or a pointer to a copy
// of v otherwise.
func GetPtrIfNotZero[T comparable](v T) *T {
	var zero T
	if v == zero {
		return nil
	}

	return &v
}

// GetValueOrZero returns a type's zero value for nil or the dereferenced value
// otherwise.
func GetValueOrZero[T any](v *T) T {
	if v == nil {
		var zero T

		return zero
	}

	return *v
}

// StrPtrToProtoTimePtr converts a string pointer to a protobuf timestamp pointer
func StrPtrToProtoTimePtr(s *string) *timestamppb.Timestamp {
	if s == nil {
		return nil
	}

	t, err := time.Parse(time.RFC3339Nano, *s)
	if err != nil {
		return nil
	}

	return timestamppb.New(t)
}

// ProtoTimePtrToStrPtr converts a protobuf timestamp pointer to a string pointer
func ProtoTimePtrToStrPtr(t *timestamppb.Timestamp) *string {
	if t == nil {
		return nil
	}

	s := t.AsTime().Format(time.RFC3339Nano)

	return &s
}
