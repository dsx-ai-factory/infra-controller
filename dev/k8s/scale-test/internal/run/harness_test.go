// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package run

import (
	"context"
	"encoding/json"
	"maps"
	"strconv"
	"sync"
	"testing"

	"github.com/NVIDIA/infra-controller/dev/k8s/scale-test/internal/cluster"
	"github.com/NVIDIA/infra-controller/dev/k8s/scale-test/internal/config"
	"github.com/NVIDIA/infra-controller/dev/k8s/scale-test/internal/lock"
	"github.com/NVIDIA/infra-controller/dev/k8s/scale-test/internal/result"
	"github.com/NVIDIA/infra-controller/dev/k8s/scale-test/internal/writer"
)

func cfg(dir string) *config.Config {
	return &config.Config{
		RunID: "r1", Command: "burst", Namespace: "ns", ExecutionID: "r1-pod",
		DryRun:         ptr(false),
		Site:           config.Site{LockName: "nico-scale-test-site-lock"},
		Timeouts:       config.Timeouts{WholeRunSeconds: 300, ReadinessSeconds: 60, FinalizeSeconds: 120},
		Journal:        config.Journal{Dir: dir, Claim: "r1-journal"},
		ConfigMaps:     config.ConfigMaps{Run: "r1-run", Result: "r1-result"},
		ResultMaxBytes: 900_000,
	}
}

// sequence records ordered events from the fakes: the pending record is written before the release, the final after.
type sequence struct {
	mu     sync.Mutex
	events []string
}

func (s *sequence) add(e string) {
	s.mu.Lock()
	s.events = append(s.events, e)
	s.mu.Unlock()
}

func (s *sequence) all() []string {
	s.mu.Lock()
	defer s.mu.Unlock()
	return append([]string(nil), s.events...)
}

// fakeTest is a scale test whose Run is a closure.
type fakeTest struct {
	run    func(RunCtx, *writer.Writer) (result.Outcome, error)
	writes *bool // nil means it writes
}

func (fakeTest) Name() string                                             { return "burst" }
func (fakeTest) LockKind() lock.HolderKind                                { return lock.HolderTest }
func (t fakeTest) Writes() bool                                           { return t.writes == nil || *t.writes }
func (t fakeTest) Run(c RunCtx, w *writer.Writer) (result.Outcome, error) { return t.run(c, w) }

// fakeHeld is a held lock over a real write gate, recording releases and closes.
type fakeHeld struct {
	gate       *lock.WriteGate
	seq        *sequence
	releaseErr error
	released   int
	closed     int
}

func (h *fakeHeld) Gate() *lock.WriteGate                            { return h.gate }
func (h *fakeHeld) SetState(context.Context, lock.HolderState) error { return nil }
func (h *fakeHeld) Release(_ context.Context, s lock.HolderState) error {
	h.released++
	if h.seq != nil {
		h.seq.add("release:" + string(s))
	}
	return h.releaseErr
}
func (h *fakeHeld) Close() { h.closed++ }

type fakeLock struct {
	held *fakeHeld
	err  error
}

func (l *fakeLock) Acquire(context.Context, lock.HolderRecord) (lock.Held, error) {
	if l.err != nil {
		return nil, l.err
	}
	return l.held, nil
}
func (l *fakeLock) Read(context.Context) (*lock.HolderView, error) { return nil, nil }

// cmFake is an in-memory ConfigMaps. It records each result.json write to seq, with the verdict and the pending reason.
type cmFake struct {
	mu    sync.Mutex
	items map[string]cluster.ConfigMapObject
	rv    int
	seq   *sequence
	// loseReply, when it returns true for a result.json write, commits the write but returns a lost-reply error.
	loseReply func(json string) bool
}

func newCM() *cmFake { return &cmFake{items: map[string]cluster.ConfigMapObject{}} }

func (f *cmFake) seed(name string, data map[string]string) {
	f.mu.Lock()
	defer f.mu.Unlock()
	f.rv++
	f.items[name] = cluster.ConfigMapObject{Name: name, ResourceVersion: strconv.Itoa(f.rv), UID: "uid-" + name, Data: data}
}

func cmClone(o cluster.ConfigMapObject) cluster.ConfigMapObject {
	o.Annotations = maps.Clone(o.Annotations)
	o.Data = maps.Clone(o.Data)
	return o
}

func (f *cmFake) GetConfigMap(_ context.Context, name string) (*cluster.ConfigMapObject, error) {
	f.mu.Lock()
	defer f.mu.Unlock()
	o, ok := f.items[name]
	if !ok {
		return nil, nil
	}
	c := cmClone(o)
	return &c, nil
}

func (f *cmFake) CreateConfigMap(_ context.Context, o cluster.ConfigMapObject) (cluster.ConfigMapObject, error) {
	f.mu.Lock()
	defer f.mu.Unlock()
	if _, ok := f.items[o.Name]; ok {
		return cluster.ConfigMapObject{}, cluster.Error{Kind: cluster.AlreadyExists}
	}
	f.rv++
	o.ResourceVersion, o.UID = strconv.Itoa(f.rv), "uid-"+o.Name
	f.items[o.Name] = cmClone(o)
	return cmClone(o), nil
}

func (f *cmFake) ReplaceConfigMap(_ context.Context, o cluster.ConfigMapObject) (cluster.ConfigMapObject, error) {
	f.mu.Lock()
	defer f.mu.Unlock()
	cur, ok := f.items[o.Name]
	switch {
	case o.ResourceVersion == "" || o.ResourceVersion == "0" || o.UID == "":
		return cluster.ConfigMapObject{}, cluster.Error{Kind: cluster.Conflict}
	case !ok || cur.ResourceVersion != o.ResourceVersion || cur.UID != o.UID:
		return cluster.ConfigMapObject{}, cluster.Error{Kind: cluster.Conflict}
	}
	f.rv++
	o.ResourceVersion = strconv.Itoa(f.rv)
	f.items[o.Name] = cmClone(o)
	js, has := o.Data[result.ResultKey]
	if f.seq != nil && has {
		f.seq.add("write:" + summaryOf(js))
	}
	if has && f.loseReply != nil && f.loseReply(js) {
		return cmClone(o), cluster.Error{Kind: cluster.Transport, Text: "lost reply"}
	}
	return cmClone(o), nil
}

// result.json of the result ConfigMap, parsed.
func (f *cmFake) written(name string) map[string]json.RawMessage {
	f.mu.Lock()
	defer f.mu.Unlock()
	var out map[string]json.RawMessage
	_ = json.Unmarshal([]byte(f.items[name].Data[result.ResultKey]), &out)
	return out
}

// summaryOf names a written summary by its verdict, with "/pending" when the release-pending reason is in it.
func summaryOf(js string) string {
	var s struct {
		Verdict string   `json:"verdict"`
		Reasons []string `json:"reasons"`
	}
	_ = json.Unmarshal([]byte(js), &s)
	for _, r := range s.Reasons {
		if r == releasePending {
			return s.Verdict + "/pending"
		}
	}
	return s.Verdict
}

// held makes a held lock over an open gate.
func held(seq *sequence) *fakeHeld {
	return &fakeHeld{gate: lock.Open(), seq: seq}
}

// deps wires a happy-path Deps: a seeded empty result ConfigMap, a held lock.
func deps(t *testing.T, seq *sequence, h *fakeHeld) Deps {
	t.Helper()
	cm := newCM()
	cm.seq = seq
	cm.seed("r1-result", nil) // the empty ConfigMap the chart made
	return Deps{Config: cfg(t.TempDir()), Lock: &fakeLock{held: h}, CM: cm, Clock: RealClock{}}
}
