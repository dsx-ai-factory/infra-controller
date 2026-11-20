// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package tui

import (
	"testing"

	"github.com/stretchr/testify/assert"
)

func TestCache_InvalidateFiltered(t *testing.T) {
	for _, test := range []struct {
		name          string
		resourceTypes []string
		wantCleared   bool
	}{
		{
			name: "scope-filtered resources", wantCleared: true,
			resourceTypes: []string{
				"vpc", "subnet", "instance",
				"allocation", "machine", "ip-block", "operating-system",
				"ssh-key-group", "network-security-group",
				"vpc-prefix", "rack", "expected-machine", "sku",
				"dpu-extension-service", "infiniband-partition", "nvlink-logical-partition",
				"dpu-machine", "machine-label-key", "expected-machine-label-key", "spectrumx-partition",
				"ipxe-template", "rule", "event-policy", "task-run", "vpc-peering", "tray-component",
			},
		},
		{name: "unscoped resources", resourceTypes: []string{"site", "audit", "ssh-key", "tenant-account"}},
	} {
		t.Run(test.name, func(t *testing.T) {
			cache := NewCache()
			for _, resourceType := range test.resourceTypes {
				cache.Set(resourceType, []NamedItem{{Name: resourceType, ID: resourceType}})
			}
			cache.InvalidateFiltered()
			for _, resourceType := range test.resourceTypes {
				assert.Equal(t, test.wantCleared, cache.Get(resourceType) == nil, resourceType)
			}
		})
	}
}

func TestInvalidateAll_ClearsEverything(t *testing.T) {
	all := []string{"site", "vpc", "subnet", "audit", "machine", "ssh-key"}

	c := NewCache()
	for _, rt := range all {
		c.Set(rt, []NamedItem{{Name: rt, ID: rt}})
	}

	c.InvalidateAll()

	for _, rt := range all {
		if got := c.Get(rt); got != nil {
			t.Errorf("InvalidateAll did not clear %q", rt)
		}
	}
}

func TestInvalidate_ClearsSingleType(t *testing.T) {
	c := NewCache()
	c.Set("vpc", []NamedItem{{Name: "v1", ID: "1"}})
	c.Set("site", []NamedItem{{Name: "s1", ID: "2"}})

	c.Invalidate("vpc")

	if got := c.Get("vpc"); got != nil {
		t.Error("Invalidate did not clear the targeted type")
	}
	if got := c.Get("site"); got == nil {
		t.Error("Invalidate incorrectly cleared an unrelated type")
	}
}

func TestCache_GetSetLookup(t *testing.T) {
	c := NewCache()
	items := []NamedItem{
		{Name: "Alpha", ID: "aaa"},
		{Name: "Bravo", ID: "bbb"},
	}
	c.Set("vpc", items)

	if got := c.Get("vpc"); len(got) != 2 {
		t.Fatalf("Get returned %d items, want 2", len(got))
	}
	if got := c.LookupByName("vpc", "alpha"); got == nil || got.ID != "aaa" {
		t.Error("LookupByName case-insensitive match failed")
	}
	if got := c.LookupByID("vpc", "bbb"); got == nil || got.Name != "Bravo" {
		t.Error("LookupByID failed")
	}
	if got := c.LookupByName("vpc", "nonexistent"); got != nil {
		t.Error("LookupByName should return nil for missing name")
	}
	if got := c.Get("missing"); got != nil {
		t.Error("Get should return nil for unfetched type")
	}
}
