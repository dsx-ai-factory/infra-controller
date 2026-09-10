// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

// Package healthz serves the controller's liveness endpoint on the pod
// network so the kubelet can restart a controller that stopped serving or
// whose reconcile loop stopped making progress.
//
// GET /healthz -> 200 "ok\n" while the reconcile loop completed a pass within
// the configured bound (or the process is younger than the bound), 503 with
// the reason otherwise. Unlike the source list, this endpoint carries no data
// and is meant to be reached from outside the pod, by the kubelet.
package healthz

import (
	"fmt"
	"net/http"
	"sync"
	"time"
)

// livenessPath is the liveness path.
const livenessPath = "/healthz"

// Liveness tracks reconcile-loop progress for the liveness endpoint.
//
// A zero or negative staleAfter disables the progress check: the endpoint
// then only tells the kubelet that the process still serves HTTP.
type Liveness struct {
	now        func() time.Time
	staleAfter time.Duration

	mu           sync.Mutex
	started      time.Time
	lastProgress time.Time
}

// New creates a Liveness that reports a stall once no reconcile pass has
// completed for staleAfter. The bound is also granted to the first pass,
// measured from construction.
func New(staleAfter time.Duration) *Liveness {
	l := &Liveness{now: time.Now, staleAfter: staleAfter}
	l.started = l.now()
	return l
}

// MarkProgress records that a reconcile pass completed.
func (l *Liveness) MarkProgress() {
	now := l.now()
	l.mu.Lock()
	defer l.mu.Unlock()
	l.lastProgress = now
}

// check returns nil while the loop is considered alive and an error naming
// the stall otherwise.
func (l *Liveness) check() error {
	if l.staleAfter <= 0 {
		return nil
	}
	now := l.now()
	l.mu.Lock()
	last, started := l.lastProgress, l.started
	l.mu.Unlock()

	if last.IsZero() {
		if age := now.Sub(started); age > l.staleAfter {
			return fmt.Errorf("no reconcile pass completed since start %s ago (bound %s)", age.Truncate(time.Second), l.staleAfter)
		}
		return nil
	}
	if age := now.Sub(last); age > l.staleAfter {
		return fmt.Errorf("last reconcile pass completed %s ago (bound %s)", age.Truncate(time.Second), l.staleAfter)
	}
	return nil
}

// Handler returns an http.Handler serving GET /healthz from the liveness
// state.
func (l *Liveness) Handler() http.Handler {
	mux := http.NewServeMux()
	mux.HandleFunc(livenessPath, func(w http.ResponseWriter, r *http.Request) {
		if r.Method != http.MethodGet && r.Method != http.MethodHead {
			w.Header().Set("Allow", "GET, HEAD")
			http.Error(w, "method not allowed", http.StatusMethodNotAllowed)
			return
		}
		w.Header().Set("Content-Type", "text/plain; charset=utf-8")
		w.Header().Set("Cache-Control", "no-store")
		if err := l.check(); err != nil {
			w.WriteHeader(http.StatusServiceUnavailable)
			if r.Method == http.MethodGet {
				_, _ = w.Write([]byte(err.Error() + "\n"))
			}
			return
		}
		w.WriteHeader(http.StatusOK)
		if r.Method == http.MethodGet {
			_, _ = w.Write([]byte("ok\n"))
		}
	})
	return mux
}
