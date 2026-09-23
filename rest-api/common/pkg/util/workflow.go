// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package util

import "time"

const (
	// DefaultInventoryReceiptInterval is the assumed interval between 2 subsequent inventory
	// receipts for a Site that has not reported its own collection interval. Prefer
	// Site.IsTimeWithinStaleInventoryThreshold, which follows the reported interval where there
	// is one.
	DefaultInventoryReceiptInterval = 3 * time.Minute
	// StaleInventoryBuffer keeps the staleness check from sitting exactly on the collection
	// interval, where clock skew between the Site and REST layer decides the outcome.
	StaleInventoryBuffer = 10 * time.Second
	// MaxInventoryReceiptInterval is the slowest inventory collection the system supports. REST
	// layer waits out the reported interval before acting on an object, so a slower Site would
	// hold off deletions and status updates long enough to destabilize it. The Site Agent rejects
	// a schedule past this at config load rather than run in that state.
	MaxInventoryReceiptInterval = 5 * time.Minute
	// The next three constants form the timeout ladder for a bespoke Site workflow
	// that a REST handler starts and waits on, and they must stay strictly increasing
	// toward the caller: activity, then workflow, then caller. A Site activity hands
	// its context straight to the on-site gRPC call, so Core inherits the activity
	// budget as its own deadline. An activity allowed to outlive the caller's wait can
	// therefore commit after the handler has given up and rolled its transaction back,
	// leaving the object live on Site with no record in the cloud and no way to reach
	// it through the API.
	//
	// grpcproxy owns the matching ladder for the generic proxy path, which is where
	// these handlers are headed. These values exist to hold the line until each one
	// has moved, so mirror any change to them there rather than letting the two drift.

	// ActivityStartToCloseTimeout bounds the on-site request before the workflow and
	// the REST caller time out.
	ActivityStartToCloseTimeout = 40 * time.Second
	// WorkflowExecutionTimeout leaves the REST caller time to observe and translate a
	// terminal workflow result. Temporal enforces it and closes the execution, unlike
	// WorkflowContextTimeout, which only stops the caller waiting.
	WorkflowExecutionTimeout = 45 * time.Second
	// WorkflowContextTimeout is how long a REST handler waits for the workflow it
	// started before abandoning it.
	WorkflowContextTimeout = time.Second * 50
	// WorkflowContextNewAfterTimeout is the timeout for a new workflow context
	WorkflowContextNewAfterTimeout = time.Second * 5
)
