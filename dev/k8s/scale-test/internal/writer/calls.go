// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package writer

import (
	"math"
	"strconv"
	"strings"
	"time"

	"google.golang.org/grpc/codes"
	"google.golang.org/grpc/metadata"

	"github.com/NVIDIA/infra-controller/dev/k8s/scale-test/internal/journal"
)

// The admission pushback bounds, as the Rust rpc::admission_retry.
const minPushback, maxPushback = time.Second, 30 * time.Second

// Rejection is a rejected attempt, classified. RetryAfter is the server's
// hint, nil when it sent none.
type Rejection struct {
	Reason     journal.Reason
	Retryable  bool
	RetryAfter *time.Duration
}

func (r Rejection) pushback() bool { return strings.HasPrefix(string(r.Reason), "pushback-") }

// provesNotApplied: admission pushback with its hint (gRPC or a 429), a
// conflict or a quota. A 503 may come from a proxy after the write landed.
func (r Rejection) provesNotApplied() bool {
	what := strings.TrimPrefix(string(r.Reason), "pushback-")
	return r.Reason == journal.Conflict || r.Reason == journal.QuotaForbidden ||
		r.pushback() && r.RetryAfter != nil && (strings.HasPrefix(what, "grpc-") || what == "429")
}

// maybeApplied: the rejection may have come after the write landed, so the
// attempt is a lost reply, whether or not the call is idempotent or the
// rejection retryable: a timeout, any 5xx, a gRPC Internal, Unknown or
// DataLoss, a pushback that does not prove it rejected before applying (a
// stop hint forbids a retry but does not prove no write landed), or any other
// retryable rejection that does not prove the write was not applied.
func maybeApplied(r Rejection) bool {
	switch {
	case r.Reason == journal.Timeout, r.Reason == journal.ServerError,
		r.Reason == journal.Grpc("Internal"), r.Reason == journal.Grpc("Unknown"), r.Reason == journal.Grpc("DataLoss"):
		return true
	case r.pushback():
		return !r.provesNotApplied()
	}
	return r.Retryable && !r.provesNotApplied()
}

// ClassifyGRPC classifies a gRPC status. ResourceExhausted and Unavailable
// are pushback, with a single grpc-retry-pushback-ms value as the hint. A
// negative, broken or repeated hint means stop. A ResourceExhausted without
// the header is NICo refusing for a quota or a pool: definite. DeadlineExceeded
// and Canceled are a timeout: the write may have landed.
func ClassifyGRPC(code codes.Code, trailer metadata.MD) Rejection {
	name := strings.ReplaceAll(code.String(), "Canceled", "Cancelled") // tonic's names
	hints := trailer.Get("grpc-retry-pushback-ms")
	switch code {
	case codes.ResourceExhausted, codes.Unavailable:
		stop := Rejection{Reason: journal.Pushback("grpc-" + name)}
		switch {
		case len(hints) == 0:
			if code == codes.ResourceExhausted {
				return Rejection{Reason: journal.Grpc(name)} // no header: definite quota
			}
			return Rejection{Reason: stop.Reason, Retryable: true} // retryable, no hint
		case len(hints) > 1:
			// gRPC treats multiple pushback values as nonretryable, and they
			// are not an admission proof that the write did not land.
			return stop
		}
		ms, err := strconv.ParseInt(hints[0], 10, 64)
		if err != nil || ms < 0 {
			return stop
		}
		d := min(max(time.Duration(min(ms, math.MaxInt64/int64(time.Millisecond)))*time.Millisecond, minPushback), maxPushback)
		return Rejection{Reason: stop.Reason, Retryable: true, RetryAfter: &d}
	case codes.DeadlineExceeded, codes.Canceled:
		return Rejection{Reason: journal.Timeout, Retryable: true}
	}
	return Rejection{Reason: journal.Grpc(name), Retryable: code == codes.Aborted || code == codes.Internal || code == codes.Unknown}
}
