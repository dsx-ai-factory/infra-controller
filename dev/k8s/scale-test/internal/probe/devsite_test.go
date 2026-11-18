// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

//go:build devsite

package probe_test

import (
	"context"
	"os"
	"slices"
	"testing"
	"time"

	"github.com/NVIDIA/infra-controller/dev/k8s/scale-test/internal/probe"
	"github.com/NVIDIA/infra-controller/dev/k8s/scale-test/internal/probeapi"
)

// TestAgainstARealProbe runs every /v1 call the runner uses against
// SCALE_TEST_PROBE_URL and strict-decodes each reply, so any wire drift
// fails. The target fails, rather than skips, when the URL is unset.
func TestAgainstARealProbe(t *testing.T) {
	url := os.Getenv("SCALE_TEST_PROBE_URL")
	if url == "" {
		t.Fatal("SCALE_TEST_PROBE_URL must be set to run the dev-site probe test")
	}
	p, err := probe.NewHTTP(url, 10*time.Second)
	if err != nil {
		t.Fatal(err)
	}
	ctx := context.Background()
	for _, ut := range []probeapi.UnitType{probeapi.UnitTypeHost, probeapi.UnitTypeDpu, probeapi.UnitTypeSwitch, probeapi.UnitTypePowerShelf} {
		if _, err := p.Count(ctx, probeapi.Filter{Type: ut}); err != nil {
			t.Fatalf("Count %s: %v", ut, err)
		}
	}
	hosts := probeapi.Filter{Type: probeapi.UnitTypeHost}

	// Count and AllUnits of the host family must agree over a snapshot that
	// did not roll over during the read. A rollover retries, bounded.
	matched := false
	for try := 0; try < 5 && !matched; try++ {
		before, err := p.Count(ctx, hosts)
		if err != nil {
			t.Fatalf("Count host: %v", err)
		}
		if before.Count == 0 {
			t.Fatal("the dev site reports no hosts; the fixture must be nonempty")
		}
		all, err := probe.AllUnits(ctx, p, time.Now().Add(60*time.Second), hosts, nil)
		if err != nil {
			t.Fatalf("AllUnits host: %v", err)
		}
		after, err := p.Count(ctx, hosts)
		if err != nil {
			t.Fatalf("Count host: %v", err)
		}
		if before.SnapshotID != after.SnapshotID {
			continue // a sweep rolled over mid-read; the counts are not comparable
		}
		if uint64(len(all)) != before.Count {
			t.Fatalf("host count %d but AllUnits read %d units of one snapshot", before.Count, len(all))
		}
		matched = true
	}
	if !matched {
		t.Fatal("the host snapshot rolled over on every attempt")
	}

	// A one-unit page, then its cursor; the ids feed an exact UnitsQuery.
	first, err := p.Units(ctx, hosts, nil, probe.Page{Limit: new(uint32(1))})
	if err != nil {
		t.Fatalf("Units: %v", err)
	}
	if len(first.Units) != 1 {
		t.Fatalf("the first host page has %d units, want the one Limit asked for", len(first.Units))
	}
	if first.NextCursor != nil {
		if _, err := p.Units(ctx, hosts, nil, probe.Page{Limit: new(uint32(1)), Cursor: first.NextCursor}); err != nil {
			t.Fatalf("Units cursor: %v", err)
		}
	}
	want := ids(first.Units)
	got, err := p.UnitsQuery(ctx, probeapi.UnitsQuery{Filter: hosts, IDs: want})
	if err != nil {
		t.Fatalf("UnitsQuery: %v", err)
	}
	gotIDs := ids(got.Units)
	slices.Sort(want)
	slices.Sort(gotIDs)
	if !slices.Equal(gotIDs, want) || len(got.MissingIDs) != 0 {
		t.Fatalf("UnitsQuery returned %d units and %d missing for %d ids", len(got.Units), len(got.MissingIDs), len(want))
	}
	if _, err := p.Config(ctx, probe.ConfigFilter{}); err != nil {
		t.Fatalf("Config: %v", err)
	}
}
