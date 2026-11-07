// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package perfstat

import (
	"fmt"
	"sync/atomic"
	"time"
)

// PerfStat defines the information to track the performance of an operation.
type PerfStat struct {
	ops         uint64
	usecs       uint64
	max         uint64
	description string
}

// Instance defines an instance of the operation to be tracked.
type Instance struct {
	stime       time.Time
	pstat       *PerfStat
	description string
}

// NewInstance sets up and returns a new Instance.
func NewInstance(des string) *Instance {
	return &Instance{
		description: des,
	}
}

// Start restarts the timer for the Instance.
func (it *Instance) Start() {
	it.stime = time.Now()
}

// End ends the timer for the Instance and commit it to the associated PerfStat.
func (it *Instance) End() {
	if it.pstat == nil {
		return
	}

	usec := uint64(it.Elapsed())
	it.pstat.commit(usec)
}

// Elapsed returns the elapsed time for the instance in usec.
func (it *Instance) Elapsed() int64 {
	return time.Since(it.stime).Nanoseconds() / 1000
}

// Show displays the elapsed time of the instance.
func (it *Instance) Show(limit uint64) {
	var unit string

	elapsed := uint64(it.Elapsed())
	if elapsed <= limit {
		return
	}

	if elapsed > 10000 {
		elapsed /= 1000
		unit = "ms"
	} else {
		unit = "us"
	}

	fmt.Printf("%s elapsed %v%s\n", it.description, elapsed, unit)
}

// WebShow returns the elapsed time of the instance in string format.
func (it *Instance) WebShow(limit uint64) string {
	var unit string

	elapsed := uint64(it.Elapsed())
	if elapsed <= limit {
		return ""
	}

	if elapsed > 10000 {
		elapsed /= 1000
		unit = "ms"
	} else {
		unit = "us"
	}

	return fmt.Sprintf("%s elapsed %v%s\n", it.description, elapsed, unit)
}

// Setup sets up instance with the given parameters.
func (it *Instance) Setup(ps *PerfStat, des string) {
	it.pstat = ps
	it.description = des
}

// New sets up and returns a new PerfStat. The caller needs to pass in a string to
// describe the performance stat for an opeartion.
func New(des string) *PerfStat {
	return &PerfStat{
		description: des,
	}
}

// NewInstance creates and returns a new tracking instance for the specified PerfStat.
func (ps *PerfStat) NewInstance(des string) *Instance {
	return &Instance{
		pstat:       ps,
		description: des,
	}
}

func (ps *PerfStat) commit(usec uint64) {
	atomic.AddUint64(&ps.ops, 1)
	atomic.AddUint64(&ps.usecs, usec)
	if usec > atomic.LoadUint64(&ps.max) {
		atomic.StoreUint64(&ps.max, usec)
	}
}

// Count returns the total ops of the PerfStat.
func (ps *PerfStat) Count() uint64 {
	return atomic.LoadUint64(&ps.ops)
}

// Elapsed returns the total elapsed time of the PerfStat
func (ps *PerfStat) Elapsed() uint64 {
	return atomic.LoadUint64(&ps.usecs)
}

func (ps *PerfStat) String() string {
	var avg uint64

	ops, usecs, max := atomic.LoadUint64(&ps.ops), atomic.LoadUint64(&ps.usecs), atomic.LoadUint64(&ps.max)
	if ops > 0 {
		avg = usecs / ops
	}

	if avg > 1000 {
		return fmt.Sprintf("Perf stat for %s: ops %v, total time (ms): %v\n"+"    avg ms/op: %v, max time (ms): %v\n",
			ps.description, ops, usecs/1000, avg/1000, max/1000)
	}
	return fmt.Sprintf("Perf stat for %s: ops %v, total time (us): %v\n"+"    avg us/op: %v, max time (us): %v\n",
		ps.description, ops, usecs, avg, max)
}
