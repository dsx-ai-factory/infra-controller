// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

//go:build devcluster

package cluster_test

import (
	"context"
	"errors"
	"fmt"
	"os"
	"testing"
	"time"

	"k8s.io/client-go/rest"

	"github.com/NVIDIA/infra-controller/dev/k8s/scale-test/internal/cluster"
)

// TestAgainstARealCluster exercises the Lease calls against the real API server
// using the pod ServiceAccount (in-cluster config) in SCALE_TEST_NAMESPACE. It
// proves the create, read and compare-and-swap semantics the site lock relies
// on, on a fresh unique Lease each run, and removes only that Lease. The target
// fails, rather than skips, when the namespace is unset or no ServiceAccount is
// mounted. It runs as a helm test Job from the runner chart.
func TestAgainstARealCluster(t *testing.T) {
	ns := os.Getenv("SCALE_TEST_NAMESPACE")
	if ns == "" {
		t.Fatal("SCALE_TEST_NAMESPACE must be set to run the dev-cluster test")
	}
	cfg, err := rest.InClusterConfig()
	if err != nil {
		t.Fatalf("in-cluster config requires a mounted ServiceAccount: %v", err)
	}
	k, err := cluster.NewFor(cfg, ns)
	if err != nil {
		t.Fatal(err)
	}
	ctx := context.Background()
	name := fmt.Sprintf("scale-test-b1-%d", time.Now().UnixNano())

	// A fresh name: nothing is stored under it yet.
	if held, err := k.GetLease(ctx, name); err != nil || held != nil {
		t.Fatalf("GetLease before create: %+v, %v", held, err)
	}

	acquire := time.Now().UTC().Truncate(time.Microsecond)
	spec := cluster.Lease{Name: name, HolderIdentity: "b1-harness", LeaseDurationSeconds: 60, AcquireTime: acquire}
	made, err := k.CreateLease(ctx, spec)
	if err != nil {
		t.Fatalf("CreateLease: %v", err)
	}
	if made.UID == "" || made.ResourceVersion == "" {
		t.Fatalf("created Lease missing UID or resourceVersion: %+v", made)
	}
	// Remove only this run's object, and only while its UID is unchanged. The
	// final check deletes it itself, so cleanup only deletes while it is live.
	live := true
	t.Cleanup(func() {
		if live {
			if err := cluster.DeleteLease(k, context.Background(), name, made.UID); err != nil {
				t.Errorf("cleanup delete: %v", err)
			}
		}
		if held, err := k.GetLease(context.Background(), name); err != nil || held != nil {
			t.Errorf("Lease still present after cleanup: %+v, %v", held, err)
		}
	})

	// The create reads back with the same identity and normalized time.
	got, err := k.GetLease(ctx, name)
	if err != nil || got == nil {
		t.Fatalf("GetLease after create: %+v, %v", got, err)
	}
	if got.UID != made.UID || got.HolderIdentity != "b1-harness" || !got.AcquireTime.Equal(acquire) {
		t.Fatalf("read-back mismatch: %+v", got)
	}

	// A second create refuses: the object already exists.
	if _, err := k.CreateLease(ctx, spec); !is(err, cluster.AlreadyExists) {
		t.Errorf("duplicate create: got %v, want already-exists", err)
	}

	// A replace with the stored version and UID replaces, not creates, keeps
	// the UID and advances the resource version with the intended time.
	renew := time.Now().UTC().Truncate(time.Microsecond)
	swap := spec
	swap.ResourceVersion, swap.UID, swap.RenewTime = made.ResourceVersion, made.UID, renew
	done, err := k.ReplaceLease(ctx, swap)
	if err != nil || done.Created || done.Lease == nil {
		t.Fatalf("ReplaceLease: %+v, %v", done, err)
	}
	if done.Lease.UID != made.UID || done.Lease.ResourceVersion == made.ResourceVersion || !done.Lease.RenewTime.Equal(renew) {
		t.Fatalf("replace stored wrong state: %+v", done.Lease)
	}

	// A stale version and a wrong UID each conflict, and neither changes state.
	stale := swap
	stale.ResourceVersion = made.ResourceVersion // now out of date
	if _, err := k.ReplaceLease(ctx, stale); !is(err, cluster.Conflict) {
		t.Errorf("stale replace: got %v, want conflict", err)
	}
	wrongUID := swap
	wrongUID.ResourceVersion, wrongUID.UID = done.Lease.ResourceVersion, "00000000-0000-0000-0000-000000000000"
	if _, err := k.ReplaceLease(ctx, wrongUID); !is(err, cluster.Conflict) {
		t.Errorf("wrong-UID replace: got %v, want conflict", err)
	}
	if after, err := k.GetLease(ctx, name); err != nil || after == nil ||
		after.ResourceVersion != done.Lease.ResourceVersion || after.UID != made.UID {
		t.Fatalf("state changed after refused writes: %+v, %v", after, err)
	}

	// Bounded cancellation: an already-cancelled and an already-expired context
	// each return their own error fast and change nothing.
	cc, cancel := context.WithCancel(ctx)
	cancel()
	t0 := time.Now()
	if _, err := k.GetLease(cc, name); !errors.Is(err, context.Canceled) {
		t.Errorf("get with cancelled ctx: %v", err)
	}
	if _, err := k.ReplaceLease(cc, swap); !errors.Is(err, context.Canceled) {
		t.Errorf("replace with cancelled ctx: %v", err)
	}
	dc, cancelD := context.WithDeadline(ctx, time.Now().Add(-time.Second))
	defer cancelD()
	if _, err := k.GetLease(dc, name); !errors.Is(err, context.DeadlineExceeded) {
		t.Errorf("get with expired ctx: %v", err)
	}
	if d := time.Since(t0); d > 2*time.Second {
		t.Errorf("cancelled calls took %v", d)
	}
	if g, err := k.GetLease(ctx, name); err != nil || g == nil || g.ResourceVersion != done.Lease.ResourceVersion {
		t.Fatalf("state changed by cancelled calls: %+v, %v", g, err)
	}
	t.Logf("bounded cancellation: cancelled and expired contexts refused fast, state intact")

	// A delete with the wrong UID is refused and the Lease stays.
	if err := cluster.DeleteLease(k, ctx, name, "00000000-0000-0000-0000-000000000000"); err == nil {
		t.Errorf("wrong-UID delete succeeded")
	}
	if g, err := k.GetLease(ctx, name); err != nil || g == nil || g.UID != made.UID {
		t.Fatalf("Lease gone after refused delete: %+v, %v", g, err)
	}
	t.Logf("wrong-UID delete refused: Lease intact")

	// Delete with the right UID, then replace with the old version and UID: the
	// replace conflicts and does not recreate the Lease.
	if err := cluster.DeleteLease(k, ctx, name, made.UID); err != nil {
		t.Fatalf("delete: %v", err)
	}
	live = false
	if g, err := k.GetLease(ctx, name); err != nil || g != nil {
		t.Fatalf("Lease present after delete: %+v, %v", g, err)
	}
	r, err := k.ReplaceLease(ctx, swap)
	var e cluster.Error
	if !errors.As(err, &e) || e.Kind != cluster.Conflict || r.Created || r.Lease != nil {
		t.Errorf("old-UID replace after delete: %+v, %v (want conflict, no create)", r, err)
	}
	if g, err := k.GetLease(ctx, name); err != nil || g != nil {
		t.Errorf("Lease recreated by old-UID replace: %+v, %v", g, err)
	}
	t.Logf("old-UID replace after delete conflicts without recreating")
}

func is(err error, kind cluster.Kind) bool {
	var e cluster.Error
	return errors.As(err, &e) && e.Kind == kind
}
