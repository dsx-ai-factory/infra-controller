// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package writer

import (
	"testing"
	"time"

	"google.golang.org/grpc/codes"
	"google.golang.org/grpc/metadata"

	"github.com/NVIDIA/infra-controller/dev/k8s/scale-test/internal/journal"
)

func TestClassifyGRPC(t *testing.T) {
	md := func(kv ...string) metadata.MD {
		m := metadata.MD{}
		for _, v := range kv {
			m.Append("grpc-retry-pushback-ms", v)
		}
		return m
	}
	for _, c := range []struct {
		name      string
		code      codes.Code
		trailer   metadata.MD
		reason    journal.Reason
		retryable bool
		after     *time.Duration // the hint after the floor and cap
	}{
		{"resource_exhausted_no_header_is_a_definite_quota", codes.ResourceExhausted, md(), journal.Grpc("ResourceExhausted"), false, nil},
		{"resource_exhausted_with_a_hint_is_pushback", codes.ResourceExhausted, md("500"), journal.Pushback("grpc-ResourceExhausted"), true, sec(1)},
		{"unavailable_without_a_header_is_pushback_no_hint", codes.Unavailable, md(), journal.Pushback("grpc-Unavailable"), true, nil},
		{"a_hint_is_floored_at_one_second", codes.Unavailable, md("10"), journal.Pushback("grpc-Unavailable"), true, sec(1)},
		{"a_hint_is_capped_at_thirty_seconds", codes.Unavailable, md("99999"), journal.Pushback("grpc-Unavailable"), true, sec(30)},
		{"a_negative_hint_means_stop", codes.Unavailable, md("-1"), journal.Pushback("grpc-Unavailable"), false, nil},
		{"a_broken_hint_means_stop", codes.Unavailable, md("soon"), journal.Pushback("grpc-Unavailable"), false, nil},
		{"several_hints_are_malformed_and_stop", codes.Unavailable, md("1000", "-1"), journal.Pushback("grpc-Unavailable"), false, nil},
		{"several_hints_on_a_quota_are_not_a_definite_quota", codes.ResourceExhausted, md("1000", "2000"), journal.Pushback("grpc-ResourceExhausted"), false, nil},
		{"deadline_exceeded_is_a_timeout_that_may_have_landed", codes.DeadlineExceeded, md(), journal.Timeout, true, nil},
		{"cancelled_is_a_timeout", codes.Canceled, md(), journal.Timeout, true, nil},
		{"aborted_is_retryable", codes.Aborted, md(), journal.Grpc("Aborted"), true, nil},
		{"internal_is_retryable", codes.Internal, md(), journal.Grpc("Internal"), true, nil},
		{"unknown_is_retryable", codes.Unknown, md(), journal.Grpc("Unknown"), true, nil},
		{"invalid_argument_is_permanent", codes.InvalidArgument, md(), journal.Grpc("InvalidArgument"), false, nil},
		{"not_found_is_permanent", codes.NotFound, md(), journal.Grpc("NotFound"), false, nil},
	} {
		t.Run(c.name, func(t *testing.T) {
			got := ClassifyGRPC(c.code, c.trailer)
			if got.Reason != c.reason || got.Retryable != c.retryable {
				t.Fatalf("got %+v, want reason %s retryable %t", got, c.reason, c.retryable)
			}
			switch {
			case (got.RetryAfter == nil) != (c.after == nil):
				t.Fatalf("got after %v, want %v", got.RetryAfter, c.after)
			case c.after != nil && *got.RetryAfter != *c.after:
				t.Fatalf("got after %v, want %v", *got.RetryAfter, *c.after)
			}
		})
	}
}

func TestMaybeApplied(t *testing.T) {
	after := sec(1)
	for _, c := range []struct {
		name string
		r    Rejection
		want bool
	}{
		{"a_timeout_may_have_landed", Rejection{Reason: journal.Timeout, Retryable: true}, true},
		{"a_grpc_internal_may_have_landed", Rejection{Reason: journal.Grpc("Internal"), Retryable: true}, true},
		{"a_grpc_dataloss_may_have_landed", Rejection{Reason: journal.Grpc("DataLoss")}, true},
		{"a_grpc_pushback_with_a_hint_proves_not_applied", Rejection{Reason: journal.Pushback("grpc-Unavailable"), Retryable: true, RetryAfter: after}, false},
		{"a_grpc_pushback_without_a_hint_may_have_landed", Rejection{Reason: journal.Pushback("grpc-Unavailable"), Retryable: true}, true},
		{"a_grpc_pushback_stop_hint_may_have_landed", Rejection{Reason: journal.Pushback("grpc-Unavailable")}, true},
		{"a_definite_quota_did_not_land", Rejection{Reason: journal.Grpc("ResourceExhausted")}, false},
		{"a_retryable_aborted_may_have_landed", Rejection{Reason: journal.Grpc("Aborted"), Retryable: true}, true},
		{"a_permanent_definite_rejection_did_not_land", Rejection{Reason: journal.Grpc("InvalidArgument")}, false},
	} {
		t.Run(c.name, func(t *testing.T) {
			if got := maybeApplied(c.r); got != c.want {
				t.Fatalf("got %t, want %t", got, c.want)
			}
		})
	}
}
