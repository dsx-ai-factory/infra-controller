// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

// Package probe is the site health probe client. Every state read of every
// test goes through Probe.
package probe

import (
	"context"
	"errors"
	"fmt"
	"slices"
	"strconv"
	"time"

	"github.com/NVIDIA/infra-controller/dev/k8s/scale-test/internal/jsonx"
	"github.com/NVIDIA/infra-controller/dev/k8s/scale-test/internal/probeapi"
)

// MaxPage is the largest page the probe serves.
const MaxPage = 1000

// Probe makes one attempt per call. Callers own retries.
type Probe interface {
	Count(ctx context.Context, f probeapi.Filter) (probeapi.CountResponse, error)
	Units(ctx context.Context, f probeapi.Filter, name *string, p Page) (probeapi.UnitsResponse, error)
	UnitsQuery(ctx context.Context, q probeapi.UnitsQuery) (probeapi.UnitsResponse, error)
	Config(ctx context.Context, f ConfigFilter) (probeapi.ConfigResponse, error)
}

type Page struct {
	Limit  *uint32
	Cursor *string
}

type ConfigFilter struct {
	Vendor, Model, InstanceType *string
	Component                   *probeapi.Component
}

// ErrorKind classifies a failed call (spec-A 6.5).
type ErrorKind string

const (
	Timeout         ErrorKind = "timeout"
	Transport       ErrorKind = "transport"
	InvalidParam    ErrorKind = "invalidParam"
	SnapshotExpired ErrorKind = "snapshotExpired"
	Unsupported     ErrorKind = "unsupported"
	Busy            ErrorKind = "busy"
	NoCompleteSweep ErrorKind = "noCompleteSweep"
	Status          ErrorKind = "status"
	Decode          ErrorKind = "decode"
	Unavailable     ErrorKind = "unavailable"
	Cancelled       ErrorKind = "cancelled"
)

// Error is a failed probe call. Its text is built locally and never holds
// reply text, a URL or a snapshot ID.
type Error struct {
	Kind       ErrorKind
	HTTPStatus int                 // Status
	Code       *probeapi.ErrorCode // Unsupported, Status
	Field      *string             // InvalidParam, Unsupported: a known parameter name only
	RetryAfter time.Duration       // Busy, NoCompleteSweep: 0 when absent
	Detail     string              // Transport, Decode, Unavailable: a fixed local text
}

func (e *Error) Error() string { return "probe: " + e.KindText() }

// KindText is the kind, with the code or status where there is one.
func (e *Error) KindText() string {
	switch {
	case e.Kind == Unsupported && e.Code != nil:
		return "unsupported " + string(*e.Code)
	case e.Kind == Status:
		return "status " + strconv.Itoa(e.HTTPStatus)
	}
	return string(e.Kind)
}

// Transient reports whether a retry may succeed. Unknown reports whether the
// call says nothing about state (all kinds but InvalidParam).
func (e *Error) Transient() bool {
	switch e.Kind {
	case Timeout, Transport, SnapshotExpired, Busy, NoCompleteSweep, Unavailable:
		return true
	case Unsupported: // field_unavailable comes back on a later sweep, unsupported_filter never
		return e.Code != nil && *e.Code == probeapi.ErrorCodeFieldUnavailable
	case Status:
		return e.HTTPStatus >= 500
	}
	return false
}

func (e *Error) Unknown() bool { return e.Kind != InvalidParam }

// knownFields are the request parameters an error may name. Any other field
// is dropped.
var knownFields = []string{"type", "source", "stage", "state", "model", "instanceType", "bmcVersion",
	"uefiVersion", "allocatable", "updateComplete", "updateEligible", "tenant", "status", "recordType", "linked",
	"resource", "initiator", "kind", "done", "name", "ids", "limit", "cursor", "vendor", "component"}

// fromReply maps a non-2xx reply. Only the code and a known field name of a
// whole error body are kept, never its text. A 422 with no usable body is
// unsupported_filter, never field_unavailable, which a caller may wait out.
func fromReply(status int, retryAfter time.Duration, body []byte) *Error {
	var b probeapi.ErrorBody
	ok := jsonx.Decode(body, &b) == nil
	var field *string
	if ok && b.Error.Field != nil && slices.Contains(knownFields, *b.Error.Field) {
		field = b.Error.Field
	}
	switch {
	case status == 400 && ok:
		return &Error{Kind: InvalidParam, Field: field}
	case status == 408:
		return &Error{Kind: Timeout}
	case status == 409:
		return &Error{Kind: SnapshotExpired}
	case status == 422 && ok:
		return &Error{Kind: Unsupported, Code: &b.Error.Code, Field: field}
	case status == 422:
		return &Error{Kind: Unsupported, Code: new(probeapi.ErrorCodeUnsupportedFilter)}
	case status == 429:
		return &Error{Kind: Busy, RetryAfter: retryAfter}
	case status == 503:
		return &Error{Kind: NoCompleteSweep, RetryAfter: retryAfter}
	case ok:
		return &Error{Kind: Status, HTTPStatus: status, Code: &b.Error.Code}
	}
	return &Error{Kind: Status, HTTPStatus: status}
}

const maxSnapshotUnits = 200_000

// One read may fetch two whole snapshots, its restart included.
var readPages, readUnits = 2*(maxSnapshotUnits/MaxPage) + 2, 2 * maxSnapshotUnits

// AllUnits reads every unit of one fresh snapshot, page by page, restarting
// once on an expired snapshot. ctx ends it with Cancelled and until with
// Timeout, cancel first.
func AllUnits(ctx context.Context, p Probe, until time.Time, f probeapi.Filter, name *string) ([]probeapi.Unit, error) {
	return allUnits(ctx, p, until, f, name, readPages, readUnits)
}

// allUnits fails a read that does not move on (a cursor twice in one pass,
// an empty page that asks for more) or needs more than maxPages and maxUnits
// over both passes.
func allUnits(ctx context.Context, p Probe, until time.Time, f probeapi.Filter, name *string, maxPages, maxUnits int) ([]probeapi.Unit, error) {
	unavailable := func(format string, a ...any) error {
		return &Error{Kind: Unavailable, Detail: fmt.Sprintf(format, a...)}
	}
	limit, pages, total := uint32(MaxPage), 0, 0
read:
	for pass := 0; ; pass++ {
		units, cursors, page := []probeapi.Unit{}, map[string]bool{}, Page{Limit: &limit}
		var snapshot *string
		for {
			// Bounded checks before and after the call, so a page ready at
			// once past a stop is not taken.
			reply, err := bounded(ctx, until, func(ctx context.Context) (probeapi.UnitsResponse, error) {
				return p.Units(ctx, f, name, page)
			})
			var pe *Error
			switch {
			case errors.Is(err, errCancelled):
				return nil, &Error{Kind: Cancelled}
			case errors.Is(err, errDeadline):
				return nil, &Error{Kind: Timeout}
			case errors.As(err, &pe) && pe.Kind == SnapshotExpired && pass == 0:
				continue read
			case err != nil:
				return nil, err
			case !Fresh(reply.SweepInfo): // the age is per reply, so any page can go stale
				return nil, unavailable("the %s sweep is %.0f s old, interval %.0f s",
					reply.Family, reply.SweepAgeSeconds, reply.SweepIntervalSeconds)
			case snapshot != nil && *snapshot != reply.SnapshotID: // an ID is reply text, so it is not named
				return nil, unavailable("pages came from two snapshots")
			}
			snapshot, pages, total = &reply.SnapshotID, pages+1, total+len(reply.Units)
			units = append(units, reply.Units...)
			switch next := reply.NextCursor; {
			case total > maxUnits:
				return nil, unavailable("the read holds over %d units", maxUnits)
			case next == nil:
				return units, nil
			case len(reply.Units) == 0:
				return nil, unavailable("an empty page asked for more")
			case cursors[*next]:
				return nil, unavailable("a cursor came back twice")
			case pages >= maxPages:
				return nil, unavailable("the read needs over %d pages", maxPages)
			default:
				cursors[*next], page.Cursor = true, next
			}
		}
	}
}

// The bound-call and freshness helpers live here; fleet-up's empty-site check
// reads Fresh too. A run-wide clock and the sweep target math arrive with
// their second users in later changes.

var (
	errCancelled = errors.New("cancelled")
	errDeadline  = errors.New("deadline passed")
)

// check returns errCancelled when ctx is done, else errDeadline at or past
// until, else nil.
func check(ctx context.Context, until time.Time) error {
	if ctx.Err() != nil {
		return errCancelled
	}
	if !time.Now().Before(until) {
		return errDeadline
	}
	return nil
}

// bounded runs call under a context that ctx or the time until ends, so a
// cooperative read stops at until. It checks before and after the call, so a
// cancel seen at any point wins and a reply ready past until is errDeadline.
func bounded[T any](ctx context.Context, until time.Time, call func(context.Context) (T, error)) (T, error) {
	var zero T
	if err := check(ctx, until); err != nil {
		return zero, err
	}
	child, cancel := context.WithCancelCause(ctx)
	defer cancel(nil)
	defer time.AfterFunc(time.Until(until), func() { cancel(errDeadline) }).Stop()
	v, err := call(child)
	if cerr := check(ctx, until); cerr != nil {
		return zero, cerr
	}
	return v, err
}

// Fresh reports whether a sweep is at most two intervals old. NaN is never
// fresh.
func Fresh(s probeapi.SweepInfo) bool {
	return s.SweepIntervalSeconds > 0 && s.SweepAgeSeconds >= 0 && s.SweepAgeSeconds <= 2*s.SweepIntervalSeconds
}
