// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package healthz

import (
	"net/http"
	"net/http/httptest"
	"testing"
	"time"

	"github.com/stretchr/testify/assert"
	"github.com/stretchr/testify/require"
)

type fakeClock struct{ now time.Time }

func (c *fakeClock) Now() time.Time          { return c.now }
func (c *fakeClock) Advance(d time.Duration) { c.now = c.now.Add(d) }
func newTestLiveness(bound time.Duration) (*Liveness, *fakeClock) {
	clock := &fakeClock{now: time.Date(2026, 1, 1, 0, 0, 0, 0, time.UTC)}
	l := &Liveness{now: clock.Now, staleAfter: bound}
	l.started = clock.Now()
	return l, clock
}

func probe(t *testing.T, l *Liveness, method string) *httptest.ResponseRecorder {
	t.Helper()
	rec := httptest.NewRecorder()
	l.Handler().ServeHTTP(rec, httptest.NewRequest(method, livenessPath, nil))
	return rec
}

func TestLiveness_FirstPassGetsTheBoundFromStart(t *testing.T) {
	l, clock := newTestLiveness(10 * time.Minute)

	require.NoError(t, l.check(), "a young process without a completed pass is alive")
	clock.Advance(9 * time.Minute)
	require.NoError(t, l.check())

	clock.Advance(2 * time.Minute)
	err := l.check()
	require.Error(t, err)
	assert.Contains(t, err.Error(), "no reconcile pass completed since start")
}

func TestLiveness_ProgressResetsTheBound(t *testing.T) {
	l, clock := newTestLiveness(10 * time.Minute)
	clock.Advance(9 * time.Minute)
	l.MarkProgress()

	clock.Advance(9 * time.Minute)
	require.NoError(t, l.check(), "18 minutes after start but 9 after the last pass")

	clock.Advance(2 * time.Minute)
	err := l.check()
	require.Error(t, err)
	assert.Contains(t, err.Error(), "last reconcile pass completed 11m0s ago")

	l.MarkProgress()
	require.NoError(t, l.check())
}

func TestLiveness_ZeroBoundDisablesTheProgressCheck(t *testing.T) {
	l, clock := newTestLiveness(0)
	clock.Advance(1000 * time.Hour)
	require.NoError(t, l.check())
	assert.Equal(t, http.StatusOK, probe(t, l, http.MethodGet).Code)
}

func TestLiveness_HandlerReportsStateAndMethods(t *testing.T) {
	l, clock := newTestLiveness(time.Minute)

	rec := probe(t, l, http.MethodGet)
	assert.Equal(t, http.StatusOK, rec.Code)
	assert.Equal(t, "ok\n", rec.Body.String())
	assert.Equal(t, "no-store", rec.Header().Get("Cache-Control"))

	rec = probe(t, l, http.MethodHead)
	assert.Equal(t, http.StatusOK, rec.Code)
	assert.Empty(t, rec.Body.String())

	rec = probe(t, l, http.MethodPost)
	assert.Equal(t, http.StatusMethodNotAllowed, rec.Code)

	clock.Advance(2 * time.Minute)
	rec = probe(t, l, http.MethodGet)
	assert.Equal(t, http.StatusServiceUnavailable, rec.Code)
	assert.Contains(t, rec.Body.String(), "no reconcile pass completed since start")

	rec = httptest.NewRecorder()
	l.Handler().ServeHTTP(rec, httptest.NewRequest(http.MethodGet, "/other", nil))
	assert.Equal(t, http.StatusNotFound, rec.Code)
}
