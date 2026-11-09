// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package inventory

import (
	"context"
	"fmt"
	"strings"
	"sync/atomic"
	"testing"
	"time"

	"github.com/NVIDIA/infra-controller/dev/k8s/scale-test/internal/cluster"
)

// countReader counts reads and answers at once.
type countReader struct{ calls atomic.Int32 }

func (r *countReader) Read(context.Context, string) (Response, error) {
	r.calls.Add(1)
	return Response{CarbideAPIURL: "https://n"}, nil
}

func TestReadStop(t *testing.T) {
	reader := &countReader{}
	ctx, cancel := context.WithCancel(bg)
	cancel()
	// readPod checks the stop before the read, so a ready pod reaches no reader.
	_, problem := readPod(ctx, reader, readyPod("p", "10.0.0.1"), 1266)
	is(t, []any{reader.calls.Load(), strings.Contains(problem, "not read")}, []any{int32(0), true}, "readPod stop")
	inv := Read(ctx, fakeLister{pods: manyPods(50)}, "ns", reader, "sel", 1266) // feeds no job once stopped
	is(t, []any{reader.calls.Load(), len(inv.Pods), len(inv.Problems)}, []any{int32(0), 0, 50}, "no dispatch")
}

// blockingReader counts concurrent reads and holds each at a gate.
type blockingReader struct {
	active  atomic.Int32
	peak    atomic.Int32
	calls   atomic.Int32
	gate    chan struct{}
	arrived chan struct{}
}

func (r *blockingReader) Read(context.Context, string) (Response, error) {
	r.calls.Add(1)
	n := r.active.Add(1)
	for {
		if p := r.peak.Load(); n <= p || r.peak.CompareAndSwap(p, n) {
			break
		}
	}
	r.arrived <- struct{}{}
	<-r.gate
	r.active.Add(-1)
	return Response{CarbideAPIURL: "https://n"}, nil
}

func manyPods(n int) []cluster.PodInfo {
	pods := make([]cluster.PodInfo, n)
	for i := range pods {
		pods[i] = readyPod(fmt.Sprintf("p%d", i), fmt.Sprintf("10.0.%d.%d", i/256, i%256))
	}
	return pods
}

func TestReadConcurrency(t *testing.T) {
	t.Run("the fanout runs at most maxReadWorkers reads at once", func(t *testing.T) {
		reader := &blockingReader{gate: make(chan struct{}), arrived: make(chan struct{}, 100)}
		done := make(chan SiteInventory, 1)
		go func() { done <- Read(bg, fakeLister{pods: manyPods(100)}, "ns", reader, "sel", 1266) }()
		for range maxReadWorkers {
			<-reader.arrived
		}
		select { // no further read starts while the pool is full
		case <-reader.arrived:
			t.Fatal("a read beyond the worker limit started")
		case <-time.After(50 * time.Millisecond):
		}
		is(t, reader.active.Load(), int32(maxReadWorkers), "active reads")
		close(reader.gate)
		inv := <-done
		is(t, []any{reader.peak.Load(), len(inv.Pods)}, []any{int32(maxReadWorkers), 100}, "peak capped, all read")
	})

	t.Run("a cancel stops further dispatch", func(t *testing.T) {
		reader := &blockingReader{gate: make(chan struct{}), arrived: make(chan struct{}, 100)}
		ctx, cancel := context.WithCancel(bg)
		done := make(chan SiteInventory, 1)
		go func() { done <- Read(ctx, fakeLister{pods: manyPods(100)}, "ns", reader, "sel", 1266) }()
		for range maxReadWorkers {
			<-reader.arrived
		}
		cancel()
		close(reader.gate)
		inv := <-done
		is(t, []any{reader.calls.Load(), len(inv.Pods)}, []any{int32(maxReadWorkers), maxReadWorkers}, "no dispatch after cancel")
	})
}
