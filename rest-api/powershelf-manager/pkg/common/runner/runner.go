// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package runner

import (
	"sync/atomic"
	"time"

	"github.com/NVIDIA/infra-controller/rest-api/powershelf-manager/pkg/common/perfstat"
	"github.com/NVIDIA/infra-controller/rest-api/powershelf-manager/pkg/common/util"

	log "github.com/sirupsen/logrus"
)

const (
	waiting runnerStatus = iota
	running
	stopping
	stopped
)

type runnerStatus uint32

func (s runnerStatus) String() string {
	switch s {
	case waiting:
		return "waiting"
	case running:
		return "running"
	case stopping:
		return "stopping"
	case stopped:
		return "stopped"
	default:
		return "in unknown state"
	}
}

type waiterFunc func(interface{}) interface{}
type runnerFunc func(interface{}, interface{})
type initFunc func() interface{}

// A Runner specifies a long running goroutine which waits for certain condition to trigger a task.
type Runner struct {
	waiter   waiterFunc
	runner   runnerFunc
	ctx      interface{}
	status   uint32
	tid      uint64
	tag      string
	lastTime time.Time // Time when last transition to running or waiting happened
	perf     *perfstat.PerfStat
}

func (r *Runner) restart() {
	if atomic.LoadUint32(&r.status) != uint32(stopped) {
		go r.run()
	}
}

func (r *Runner) run() {
	defer r.restart()

	r.tid = util.GetGoroutineID()

	log.Info("Runner ", r.tag, " started")

	cur := r.perf.NewInstance("")
	for {
		status := atomic.SwapUint32(&r.status, uint32(waiting))
		if status == uint32(stopping) {
			break
		}

		r.lastTime = time.Now()
		task := r.waiter(r.ctx)

		status = atomic.SwapUint32(&r.status, uint32(running))
		if status == uint32(stopping) {
			break
		}
		cur.Start()
		r.lastTime = time.Now()
		r.runner(r.ctx, task)
		cur.End()
	}
	atomic.StoreUint32(&r.status, uint32(stopped))

}

// New initializes and starts a new runner.
func New(tag string, initializer initFunc, waiter waiterFunc, runner runnerFunc) *Runner {
	nr := &Runner{
		waiter: waiter,
		runner: runner,
		tag:    tag,
		perf:   perfstat.New(tag),
	}

	if initializer != nil {
		nr.ctx = initializer()
	}

	go nr.run()

	return nr
}

// Stop stops a runner
func (r *Runner) Stop() {
	status := atomic.SwapUint32(&r.status, uint32(stopping))
	if status == uint32(running) {
		for atomic.LoadUint32(&r.status) != uint32(stopped) {
			time.Sleep(time.Millisecond * 10)
		}
	}
}

// State returns the state of a runner in enum
func (r *Runner) State() int {
	return int(atomic.LoadUint32(&r.status))
}

// Status returns a runner's status
func (r *Runner) Status() string {
	return runnerStatus(atomic.LoadUint32(&r.status)).String()
}

// SetStopping sets the state of the runner to stopping,
// allowing it to be stopped, and goroutine be terminated
// once the current run has ended
func (r *Runner) SetStopping() {
	atomic.SwapUint32(&r.status, uint32(stopping))
}
