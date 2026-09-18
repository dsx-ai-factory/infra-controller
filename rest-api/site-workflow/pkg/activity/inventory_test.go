// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package activity

import (
	"testing"

	corev1 "github.com/NVIDIA/infra-controller/rest-api/proto/core/gen/v1"
	"github.com/stretchr/testify/assert"
	"github.com/stretchr/testify/mock"
)

// pagedInventory is the paging accessor every inventory message generates, which lets one
// assertion cover a publish call whatever resource type it carries.
type pagedInventory interface {
	GetInventoryPage() *corev1.InventoryPage
}

// assertItemIDsOnFinalPageOnly checks the contract Cloud's deletion sweep rests on: the page
// reporting itself last carries every reported ID, and no other page carries any. Cloud reads
// the list only on that page, so sending it earlier repeats the whole Site once per page.
func assertItemIDsOnFinalPageOnly(t *testing.T, calls []mock.Call, wantTotalItems int) {
	t.Helper()
	for index, call := range calls {
		inventory, ok := call.Arguments[4].(pagedInventory)
		if !assert.True(t, ok, "publish call %d carries no inventory", index+1) {
			continue
		}
		// A failed collection reports no paging at all, so it has no list to place.
		page := inventory.GetInventoryPage()
		if page == nil {
			continue
		}
		// Cloud's own predicate, which counts a run with no pages of its own as complete.
		if page.GetTotalPages() == 0 || page.GetCurrentPage() == page.GetTotalPages() {
			assert.Len(t, page.GetItemIds(), wantTotalItems, "final page %d", index+1)
			continue
		}
		assert.Empty(t, page.GetItemIds(), "page %d of %d", page.GetCurrentPage(), page.GetTotalPages())
	}
}
