// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package probeapi

import "github.com/NVIDIA/infra-controller/dev/k8s/scale-test/internal/jsonx"

// MaxCount is 2^53, the largest count a float64 holds exactly. No crossing
// target may exceed it.
const MaxCount = uint64(1) << 53

// FixedQuantiles are the six fractions of n a stage is timed at. v1 times
// stages up from a proven baseline. Quantiles and direction are fixed.
var FixedQuantiles = [6]float64{0.5, 0.9, 0.95, 0.99, 0.995, 1}

// SummaryParams are the parameters of one stage summary: the window and the
// planned count n. The stage counts up to base + ceil(q*n) for each
// FixedQuantiles q. Base is a runner-local history extension, omitted for
// existing zero-base probe contract requests.
// SummaryBase is an immutable count and source sweep from the planned burst.
// The stored baseline must match every field exactly before timing is usable.
type SummaryBase struct {
	Count               uint64     `json:"count"`
	ProcessStartSeconds float64    `json:"processStartSeconds"`
	SweepStart          jsonx.Time `json:"sweepStart"`
	ReadAt              jsonx.Time `json:"readAt"`
}

type SummaryParams struct {
	Base  *SummaryBase `json:"base,omitempty"`
	Start jsonx.Time   `json:"start"`
	End   jsonx.Time   `json:"end"`
	N     uint64       `json:"n"`
	// AddWindowSeconds is the dispatch window: how long the run took to send
	// the stage's units, added to the timing error so a crossing is never
	// reported earlier than a unit could have been dispatched.
	AddWindowSeconds float64 `json:"addWindowSeconds"`
}

func (SummaryParams) JSONClosed() {}

// SummaryResponse is a stage summary: when the ready count crossed each
// fixed target, and how far off that time can be. Without a proven zero
// baseline or a usable sweep every crossing keeps its entry with nil times
// and counts: a missing baseline is never read as zero.
type SummaryResponse struct {
	Family Family     `json:"family"`
	Start  jsonx.Time `json:"start"`
	End    jsonx.Time `json:"end"`
	N      uint64     `json:"n"`
	// Crossings has one entry per FixedQuantiles q, in ascending order.
	Crossings               []Crossing  `json:"crossings"`
	FinalCount              *uint64     `json:"finalCount"`
	FinalSweepAt            *jsonx.Time `json:"finalSweepAt"`
	MaxSweepGapSeconds      *float64    `json:"maxSweepGapSeconds"`
	MaxSweepDurationSeconds *float64    `json:"maxSweepDurationSeconds"`
	AddWindowSeconds        float64     `json:"addWindowSeconds"`
	TimingErrorSeconds      *float64    `json:"timingErrorSeconds"`
	FinalScrapeSeen         bool        `json:"finalScrapeSeen"`
	SweepsSeen              uint32      `json:"sweepsSeen"`
	// WorstCountAfterFirstCrossing is the lowest count seen after the
	// smallest quantile was reached: evidence of a regression after the
	// crossing.
	WorstCountAfterFirstCrossing *uint64 `json:"worstCountAfterFirstCrossing"`
}

type Crossing struct {
	Q         float64     `json:"q"`
	Target    uint64      `json:"target"`
	ReachedAt *jsonx.Time `json:"reachedAt"`
	Seconds   *float64    `json:"seconds"`
	// WorstCountAfter is the lowest count seen from this crossing to the end
	// of the window: a dip below the target after it was first met.
	WorstCountAfter *uint64 `json:"worstCountAfter"`
}

func (p *SummaryParams) UnmarshalJSON(data []byte) error   { return jsonx.Decode(data, p) }
func (r *SummaryResponse) UnmarshalJSON(data []byte) error { return jsonx.Decode(data, r) }
