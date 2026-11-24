// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package clustertest

import (
	"context"
	"reflect"
	"testing"
	"testing/synctest"

	"github.com/NVIDIA/infra-controller/dev/k8s/scale-test/internal/cluster"
)

var (
	bg       = context.Background()
	conflict = cluster.Error{Kind: cluster.Conflict}
)

func is(t *testing.T, got, want any, what string) {
	t.Helper()
	if !reflect.DeepEqual(got, want) {
		t.Errorf("%s: got %v, want %v", what, got, want)
	}
}

func lease() cluster.Lease {
	return cluster.Lease{Name: "lock", HolderIdentity: "test/r1/x", LeaseDurationSeconds: 60}
}

func TestFake(t *testing.T) {
	for _, tc := range []struct {
		name string
		run  func(t *testing.T, f *Fake)
	}{
		{"parked write commits without its caller", func(t *testing.T, f *Fake) {
			created, _ := f.CreateLease(bg, lease())
			// The caller gives up. The server still holds its write.
			f.ParkNext(ReplaceLease)
			ctx, cancel := context.WithCancel(bg)
			go func() { _, _ = f.ReplaceLease(ctx, created) }()
			synctest.Wait()
			cancel()
			is(t, *f.Lease("lock"), created, "not yet")
			is(t, f.CommitParked(), nil, "commit")
			committed := *f.Lease("lock")
			is(t, committed.ResourceVersion != created.ResourceVersion, true, "committed")
			// The version is compared at commit: another writer got in first.
			f.ParkNext(ReplaceLease)
			answer := make(chan error, 1)
			go func() { _, err := f.ReplaceLease(bg, committed); answer <- err }()
			synctest.Wait()
			f.SetLease(committed)
			is(t, f.CommitParked(), error(conflict), "late commit")
			is(t, <-answer, error(conflict), "a caller that still waits gets the answer")
			is(t, f.CommitParked(), ErrNotParked, "nothing parked")
		}},
	} {
		t.Run(tc.name, func(t *testing.T) { synctest.Test(t, func(t *testing.T) { tc.run(t, New("nico-mat")) }) })
	}
}
