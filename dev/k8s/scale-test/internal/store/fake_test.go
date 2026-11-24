// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package store

import (
	"encoding/json"
	"maps"
	"net/http"
	"net/http/httptest"
	"regexp"
	"slices"
	"strconv"
	"strings"
	"sync"
	"time"
)

// fakeStore is a metrics store over recorded samples, served as the instant
// query API. A sample answers only once the clock passed its visible time and
// the ingestion delay, and a query whose time is in the future fails. The clock
// is time.Now, which testing/synctest fakes.
type fakeStore struct {
	mu               sync.Mutex
	samples          []fakeSample
	queries          []fakeQuery
	delay, ingestion time.Duration
	// Hook, when set before the first query, runs at every query before it
	// is answered.
	Hook func(fakeQuery)
}

// SetDelay holds every reply delay long first. A sample becomes queryable
// only ingestion after its time, as behind remote-write batching.
func (s *fakeStore) SetDelay(delay, ingestion time.Duration) {
	s.mu.Lock()
	defer s.mu.Unlock()
	s.delay, s.ingestion = delay, ingestion
}

type fakeSample struct {
	labels          map[string]string
	atMs, visibleMs int64
	value           float64
}

// fakeQuery is a query the store got: its PromQL and its time in Unix
// milliseconds.
type fakeQuery struct {
	PromQL string
	AtMs   int64
}

// Add stores value of the series name with the label pairs kv at atMs.
func (s *fakeStore) Add(name string, atMs int64, value float64, kv ...string) {
	s.AddLate(name, atMs, atMs, value, kv...)
}

// AddLate is Add of a sample the store can answer with only from visibleMs
// on.
func (s *fakeStore) AddLate(name string, atMs, visibleMs int64, value float64, kv ...string) {
	labels := map[string]string{"__name__": name}
	for i := 0; i+1 < len(kv); i += 2 {
		labels[kv[i]] = kv[i+1]
	}
	s.mu.Lock()
	defer s.mu.Unlock()
	s.samples = append(s.samples, fakeSample{labels, atMs, visibleMs, value})
}

// Queries is every query so far.
func (s *fakeStore) Queries() []fakeQuery {
	s.mu.Lock()
	defer s.mu.Unlock()
	return slices.Clone(s.queries)
}

var fakeMatcher = regexp.MustCompile(`(\w+)(=~?)("(?:[^"\\]|\\.)*")`)

func (s *fakeStore) ServeHTTP(w http.ResponseWriter, r *http.Request) {
	s.mu.Lock()
	delay, ingestion, hook := s.delay, s.ingestion, s.Hook
	s.mu.Unlock()
	select {
	case <-time.After(delay):
	case <-r.Context().Done():
		return
	}
	promql, at := r.URL.Query().Get("query"), r.URL.Query().Get("time")
	secs, err := strconv.ParseFloat(at, 64)
	open, shut := strings.Index(promql, "{"), strings.LastIndex(promql, "}")
	if err != nil || open < 0 || shut < open {
		http.Error(w, "bad query", http.StatusBadRequest)
		return
	}
	q := fakeQuery{promql, int64(secs*1000 + 0.5)}
	now := time.Now().UnixMilli()
	s.mu.Lock()
	s.queries = append(s.queries, q)
	s.mu.Unlock()
	if hook != nil {
		hook(q)
	}
	if q.AtMs > now {
		http.Error(w, "query time is after now", http.StatusBadRequest)
		return
	}
	span := promql[shut+2 : len(promql)-1]
	spanMs, _ := strconv.ParseInt(strings.TrimSuffix(span, "ms"), 10, 64)
	if !strings.HasSuffix(span, "ms") {
		spanMs, _ = strconv.ParseInt(strings.TrimSuffix(span, "s"), 10, 64)
		spanMs *= 1000
	}
	match := func(labels map[string]string) bool {
		if name := promql[:open]; name != "" && labels["__name__"] != name {
			return false
		}
		for _, m := range fakeMatcher.FindAllStringSubmatch(promql[open:shut], -1) {
			var want string
			_ = json.Unmarshal([]byte(m[3]), &want)
			if m[2] == "=" && labels[m[1]] != want || m[2] == "=~" && !regexp.MustCompile("^(?:"+want+")$").MatchString(labels[m[1]]) {
				return false
			}
		}
		return true
	}
	type series struct {
		Metric map[string]string `json:"metric"`
		Values [][2]any          `json:"values"`
	}
	var result []*series
	s.mu.Lock()
	for _, x := range s.samples {
		if x.atMs <= q.AtMs-spanMs || x.atMs > q.AtMs || max(x.visibleMs, x.atMs+ingestion.Milliseconds()) > now || !match(x.labels) {
			continue
		}
		i := slices.IndexFunc(result, func(r *series) bool { return maps.Equal(r.Metric, x.labels) })
		if i < 0 {
			i, result = len(result), append(result, &series{Metric: x.labels})
		}
		result[i].Values = append(result[i].Values, [2]any{float64(x.atMs) / 1000, strconv.FormatFloat(x.value, 'g', -1, 64)})
	}
	s.mu.Unlock()
	for _, r := range result {
		slices.SortFunc(r.Values, func(a, b [2]any) int { return int(a[0].(float64)*1000 - b[0].(float64)*1000) })
	}
	_ = json.NewEncoder(w).Encode(map[string]any{"status": "success", "data": map[string]any{"resultType": "matrix", "result": append([]*series{}, result...)}})
}

// fakeTransport serves h in process as an HTTP client's round tripper: no
// socket, so testing/synctest can run a client and its store in one bubble.
type fakeTransport struct{ http.Handler }

func (t fakeTransport) RoundTrip(r *http.Request) (*http.Response, error) {
	rec := httptest.NewRecorder()
	done := make(chan struct{})
	go func() {
		defer close(done)
		t.ServeHTTP(rec, r)
	}()
	select {
	case <-done:
		return rec.Result(), nil
	case <-r.Context().Done():
		return nil, r.Context().Err()
	}
}
