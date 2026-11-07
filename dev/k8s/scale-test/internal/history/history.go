// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

// Package history computes when a probe count crossed each target, from
// the probe's own series in the metrics store.
//
// The site health probe publishes its counts and sweep times as metrics and
// never reads them back. This package holds the math a client runs on the
// samples it read from the store: the source and sweep join, the targets
// and crossings, the timing error, the rates and the longest stall. It
// never calls the probe or NICo. A call is prepared from a filter and the
// summary parameters before any store read (Prepare), then summarized from
// the samples read for it (Summarize).
package history

import (
	"context"
	"errors"
	"fmt"
	"math"
	"time"

	"github.com/NVIDIA/infra-controller/dev/k8s/scale-test/internal/probeapi"
)

// The six reasons a history call has no answer. Each is its own type, so a
// caller keeps it: a timeout is never a failure, and a request that can
// never be answered is never retried. Every text is this package's own,
// never store or probe reply text.
type (
	// InvalidInput is a request that can never be answered as asked: a bad
	// window, quantile or target. Field names the parameter at fault.
	InvalidInput struct{ Field, Reason string }
	// Unsupported is a filter or probe metadata this client cannot read.
	Unsupported struct{ Text string }
	// Unavailable is a store whose series cannot answer: not the probe's, or a
	// sweep the summary reads lacks a capability its filter needs.
	Unavailable struct{ Text string }
	// Busy is another history read holding the store's read gate. RetryAfter
	// is when to ask again, 0 when unknown.
	Busy struct{ RetryAfter time.Duration }
	// Timeout is a call that did not answer by its deadline.
	Timeout struct{}
	// Cancelled is a call its caller cancelled.
	Cancelled struct{}
)

func (e InvalidInput) Error() string { return "invalid " + e.Field + ": " + e.Reason }
func (e Unsupported) Error() string  { return "history cannot serve this request: " + e.Text }
func (e Unavailable) Error() string  { return "history unavailable: " + e.Text }
func (Busy) Error() string           { return "history is busy" }
func (Timeout) Error() string        { return "history request timed out" }
func (Cancelled) Error() string      { return "history request cancelled" }

func (InvalidInput) KindText() string { return "invalid input" }
func (Unsupported) KindText() string  { return "unsupported" }
func (Unavailable) KindText() string  { return "unavailable" }
func (Busy) KindText() string         { return "busy" }
func (Timeout) KindText() string      { return "timeout" }
func (Cancelled) KindText() string    { return "cancelled" }

// badStore is a store whose series are not what the probe publishes.
func badStore(reason string) error {
	return Unavailable{"the stored series are not the probe's: " + reason}
}

// ProbeMetadata is the probe's /v1/config metrics block, checked. The
// caller reads it from the probe and injects it.
type ProbeMetadata struct {
	Info           probeapi.MetricsInfo
	ScrapeInterval time.Duration
	// ReaderIntervals holds the interval of each family an enabled reader
	// sweeps.
	ReaderIntervals map[probeapi.Family]time.Duration
}

// NewProbeMetadata checks the metadata a probe serves. Values this client
// cannot read are Unsupported, and the error never repeats them.
func NewProbeMetadata(info probeapi.MetricsInfo) (ProbeMetadata, error) {
	m := ProbeMetadata{Info: info, ReaderIntervals: map[probeapi.Family]time.Duration{}}
	if info.ContractVersion != probeapi.ContractVersion {
		return ProbeMetadata{}, Unsupported{fmt.Sprintf("the probe publishes metric contract %d, this client reads %d",
			info.ContractVersion, probeapi.ContractVersion)}
	}
	if len(probeapi.SelectorProblems(info.Selector)) > 0 {
		return ProbeMetadata{}, Unsupported{"the probe's series selector cannot select its series"}
	}
	var ok bool
	if m.ScrapeInterval, ok = interval(info.ScrapeIntervalSeconds); !ok {
		return ProbeMetadata{}, Unsupported{"the probe's scrape interval is not a positive time"}
	}
	for family, seconds := range info.ReaderIntervalsSeconds {
		if m.ReaderIntervals[family], ok = interval(seconds); !ok {
			return ProbeMetadata{}, Unsupported{"a reader interval of the probe is not a positive time"}
		}
	}
	if !IsTime(info.ProcessStartSeconds) {
		return ProbeMetadata{}, Unsupported{"the probe's process start is not a Unix time"}
	}
	return m, nil
}

// interval is seconds as a positive time.Duration, false when none holds
// it: NaN, at most 0, or past the longest Duration.
func interval(seconds float64) (time.Duration, bool) {
	d := time.Duration(seconds * 1e9)
	return d, seconds*1e9 < math.MaxInt64 && d > 0
}

// Context is what one history call runs with. The caller's
// context.Context is its cancel signal, and Until its deadline: every step
// checks both before it starts, and its reply once more.
type Context struct {
	Probe ProbeMetadata
	// Now is the wall clock, injectable for tests.
	Now func() time.Time
	// Until is the deadline, read against time.Now so its monotonic clock
	// counts.
	Until time.Time
}

// Check is Cancelled once ctx is cancelled, else Timeout once a deadline has
// passed. Cancel is checked first and wins ties. A ctx deadline counts once
// its instant has passed, even before the Done notification fires, so a
// delayed notification never lets a call run past its deadline.
func (c Context) Check(ctx context.Context) error {
	if errors.Is(ctx.Err(), context.Canceled) {
		return Cancelled{}
	}
	if ctx.Err() != nil || !time.Now().Before(c.Until) {
		return Timeout{}
	}
	if d, ok := ctx.Deadline(); ok && !time.Now().Before(d) {
		return Timeout{}
	}
	return nil
}
