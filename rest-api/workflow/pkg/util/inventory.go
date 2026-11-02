// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package util

import (
	corev1 "github.com/NVIDIA/infra-controller/rest-api/proto/core/gen/v1"
)

// ShouldReconcileDeletions reports whether a page carries the complete set of IDs the Site
// holds, which is the only basis on which Cloud may delete what it no longer sees.
//
// A page that reports itself last but carries no list came from a Site Agent that sends the
// list elsewhere, and says nothing about what the Site is missing. Acting on it would read
// every item the page does not carry as deleted. A Site reporting no items is the one case
// where an empty list is the complete answer, and it has to keep reconciling so that a Site
// emptied on purpose still clears its Cloud records.
func ShouldReconcileDeletions(page *corev1.InventoryPage) bool {
	// No paging at all: the message is the whole inventory.
	if page == nil || page.GetTotalPages() == 0 {
		return true
	}
	if page.GetCurrentPage() != page.GetTotalPages() {
		return false
	}
	return page.GetTotalItems() == 0 || len(page.GetItemIds()) > 0
}
