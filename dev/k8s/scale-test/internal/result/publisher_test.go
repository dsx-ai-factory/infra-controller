// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package result

import (
	"context"
	"maps"
	"strconv"
	"strings"
	"testing"

	"github.com/NVIDIA/infra-controller/dev/k8s/scale-test/internal/cluster"
)

// cmFake is an in-memory ConfigMaps with resource versions, UIDs, annotations and data, plus scripted errors per op.
// afterWrite runs when a create or replace returns a queued error (a lost reply, a concurrent reowner or a vanished ConfigMap).
type cmFake struct {
	items                         map[string]cluster.ConfigMapObject
	rv                            int
	getErr, createErr, replaceErr []error
	afterWrite                    func()
}

func newCMFake() *cmFake { return &cmFake{items: map[string]cluster.ConfigMapObject{}} }

func pop(q *[]error) error {
	if len(*q) == 0 {
		return nil
	}
	e := (*q)[0]
	*q = (*q)[1:]
	return e
}

func clone(o cluster.ConfigMapObject) cluster.ConfigMapObject {
	o.Annotations = maps.Clone(o.Annotations)
	o.Data = maps.Clone(o.Data)
	return o
}

func (f *cmFake) put(name string, annotations, data map[string]string, uid string) {
	f.rv++
	f.items[name] = cluster.ConfigMapObject{Name: name, ResourceVersion: strconv.Itoa(f.rv), UID: uid, Annotations: annotations, Data: data}
}

func (f *cmFake) onError(e error) (cluster.ConfigMapObject, error) {
	if f.afterWrite != nil {
		f.afterWrite()
		f.afterWrite = nil
	}
	return cluster.ConfigMapObject{}, e
}

func (f *cmFake) GetConfigMap(_ context.Context, name string) (*cluster.ConfigMapObject, error) {
	if e := pop(&f.getErr); e != nil {
		return nil, e
	}
	o, ok := f.items[name]
	if !ok {
		return nil, nil
	}
	c := clone(o)
	return &c, nil
}

func (f *cmFake) CreateConfigMap(_ context.Context, o cluster.ConfigMapObject) (cluster.ConfigMapObject, error) {
	if e := pop(&f.createErr); e != nil {
		return f.onError(e)
	}
	if _, ok := f.items[o.Name]; ok {
		return cluster.ConfigMapObject{}, cluster.Error{Kind: cluster.AlreadyExists}
	}
	f.put(o.Name, o.Annotations, o.Data, "uid-"+o.Name)
	return clone(f.items[o.Name]), nil
}

func (f *cmFake) ReplaceConfigMap(_ context.Context, o cluster.ConfigMapObject) (cluster.ConfigMapObject, error) {
	if e := pop(&f.replaceErr); e != nil {
		return f.onError(e)
	}
	cur, ok := f.items[o.Name]
	switch {
	case o.ResourceVersion == "" || o.ResourceVersion == "0" || o.UID == "":
		return cluster.ConfigMapObject{}, cluster.Error{Kind: cluster.Conflict}
	case !ok || cur.ResourceVersion != o.ResourceVersion || cur.UID != o.UID:
		return cluster.ConfigMapObject{}, cluster.Error{Kind: cluster.Conflict}
	}
	f.put(o.Name, o.Annotations, o.Data, o.UID)
	return clone(f.items[o.Name]), nil
}

const owner = "r1-abcde/tok-secret-123"

func conflictErr() error { return cluster.Error{Kind: cluster.Conflict} }

func TestClaim(t *testing.T) {
	rows := []struct {
		name    string
		setup   func(*cmFake)
		wantUID string
		wantErr string // substring, "" means success
	}{
		{"missing is created and claimed", func(f *cmFake) {}, "uid-r1-result", ""},
		{"same owner is claimed", func(f *cmFake) { f.put("r1-result", map[string]string{OwnerAnnotation: owner}, nil, "u9") }, "u9", ""},
		{"another owner is refused", func(f *cmFake) { f.put("r1-result", map[string]string{OwnerAnnotation: "other/tok"}, nil, "u9") }, "", "owned by another attempt"},
		{"no owner with data is refused", func(f *cmFake) { f.put("r1-result", nil, map[string]string{ResultKey: "{}"}, "u9") }, "", "holds a result with no owner"},
		{"no owner empty is claimed by replace", func(f *cmFake) { f.put("r1-result", nil, nil, "u9") }, "u9", ""},
		{"lost reply commits our owner, read back as claimed", func(f *cmFake) {
			f.put("r1-result", nil, nil, "u9")
			f.replaceErr = []error{cluster.Error{Kind: cluster.Transport, Text: "the connection failed"}}
			f.afterWrite = func() { f.put("r1-result", map[string]string{OwnerAnnotation: owner}, nil, "u9") }
		}, "u9", ""},
		{"a concurrent owner claimed first", func(f *cmFake) {
			f.put("r1-result", nil, nil, "u9")
			f.replaceErr = []error{conflictErr()}
			f.afterWrite = func() { f.put("r1-result", map[string]string{OwnerAnnotation: "other/tok"}, nil, "u9") }
		}, "", "claimed first by another attempt"},
		{"not confirmed when the read back is also gone", func(f *cmFake) {
			f.put("r1-result", nil, nil, "u9")
			f.replaceErr = []error{cluster.Error{Kind: cluster.Transport, Text: "the connection failed"}}
			f.afterWrite = func() { delete(f.items, "r1-result") }
		}, "", "claim not confirmed"},
	}
	for _, r := range rows {
		f := newCMFake()
		r.setup(f)
		uid, err := Claim(context.Background(), f, "r1-result", owner)
		gotErr := ""
		if err != nil {
			gotErr = err.Error()
		}
		switch {
		case r.wantErr == "" && (err != nil || uid != r.wantUID):
			t.Errorf("%s: got (%q, %v), want (%q, nil)", r.name, uid, err, r.wantUID)
		case r.wantErr != "" && !strings.Contains(gotErr, r.wantErr):
			t.Errorf("%s: error %q does not contain %q", r.name, gotErr, r.wantErr)
		}
		if err != nil && strings.Contains(err.Error(), "secret") {
			t.Errorf("%s: the owner token leaked into the error: %q", r.name, err)
		}
	}
}

func TestWriteResultCAS(t *testing.T) {
	const name = "r1-result"
	claimed := func() *cmFake {
		f := newCMFake()
		f.put(name, map[string]string{OwnerAnnotation: owner}, nil, "u1")
		return f
	}
	t.Run("writes under the claim", func(t *testing.T) {
		f := claimed()
		if err := WriteResult(context.Background(), f, name, owner, "u1", `{"ok":1}`); err != nil {
			t.Fatal(err)
		}
		if f.items[name].Data[ResultKey] != `{"ok":1}` {
			t.Error("the result was not stored")
		}
	})
	t.Run("retries once on conflict then succeeds", func(t *testing.T) {
		f := claimed()
		f.replaceErr = []error{conflictErr()}
		if err := WriteResult(context.Background(), f, name, owner, "u1", "x"); err != nil {
			t.Fatalf("a conflict must be retried: %v", err)
		}
	})
	t.Run("gives up after three conflicts", func(t *testing.T) {
		f := claimed()
		f.replaceErr = []error{conflictErr(), conflictErr(), conflictErr()}
		err := WriteResult(context.Background(), f, name, owner, "u1", "x")
		if err == nil || !strings.Contains(err.Error(), "changed during each of 3 writes") {
			t.Errorf("want a give-up after 3 tries, got %v", err)
		}
	})
	t.Run("refuses a reowned ConfigMap", func(t *testing.T) {
		f := newCMFake()
		f.put(name, map[string]string{OwnerAnnotation: "other/tok"}, nil, "u2")
		err := WriteResult(context.Background(), f, name, owner, "u1", "x")
		if err == nil || !strings.Contains(err.Error(), "replaced or claimed by another attempt") {
			t.Errorf("a reowned ConfigMap must be refused, got %v", err)
		}
	})
	t.Run("refuses a ConfigMap that is gone", func(t *testing.T) {
		f := newCMFake()
		err := WriteResult(context.Background(), f, name, owner, "u1", "x")
		if err == nil || !strings.Contains(err.Error(), "it is gone") {
			t.Errorf("a missing ConfigMap must be refused, not recreated, got %v", err)
		}
	})
	t.Run("a non-conflict error is not retried and carries no server or owner text", func(t *testing.T) {
		f := claimed()
		f.replaceErr = []error{cluster.Error{Kind: cluster.API, Code: 500, Text: "InternalError"}}
		err := WriteResult(context.Background(), f, name, owner, "u1", "x")
		if err == nil || strings.Contains(err.Error(), "secret") || strings.Contains(err.Error(), owner) {
			t.Errorf("want a clean API error with no owner or server text, got %v", err)
		}
	})
	// A lost reply is confirmed only when the read-back is our own object (the loop's UID and owner fence, not equal
	// bytes alone), so a reowner or a new UID carrying the same payload is never adopted as a confirmed write.
	const js = `{"v":1}`
	lost := cluster.Error{Kind: cluster.API, Code: 500, Text: "lost reply"}
	t.Run("a read-back with the same bytes and owner but a new UID is not confirmed", func(t *testing.T) {
		f := claimed()
		f.replaceErr = []error{lost}
		f.afterWrite = func() { f.put(name, map[string]string{OwnerAnnotation: owner}, map[string]string{ResultKey: js}, "u9") }
		if err := WriteResult(context.Background(), f, name, owner, "u1", js); err == nil {
			t.Error("a read-back under a new UID must stay a failure")
		}
	})
	t.Run("a read-back with the same bytes and UID but a different owner is not confirmed", func(t *testing.T) {
		f := claimed()
		f.replaceErr = []error{lost}
		f.afterWrite = func() {
			f.put(name, map[string]string{OwnerAnnotation: "other/tok"}, map[string]string{ResultKey: js}, "u1")
		}
		if err := WriteResult(context.Background(), f, name, owner, "u1", js); err == nil {
			t.Error("a read-back reowned by another attempt must stay a failure")
		}
	})
}
