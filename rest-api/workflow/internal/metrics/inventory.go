// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package metrics

import (
	"context"
	"time"

	"github.com/google/uuid"
	"github.com/prometheus/client_golang/prometheus"

	cdb "github.com/NVIDIA/infra-controller/rest-api/db/pkg/db"
)

const (
	// InventoryStatusSuccess workflow has completed successfully
	InventoryStatusSuccess = "Success"
	// InventoryStatusFailed workflow activity execution has failed
	InventoryStatusFailed = "Failed"

	// Inventory operation types for metrics labels
	InventoryOperationTypeCreate = "create"
	InventoryOperationTypeDelete = "delete"
)

// InventoryObjectLifecycleEvent represents a lifecycle event for an inventory object.
// Either Created or Deleted should be set, but not both:
// - For CREATE events: Created should be non-nil, Deleted should be nil
// - For DELETE events: Deleted should be non-nil, Created should be nil
type InventoryObjectLifecycleEvent struct {
	ObjectID uuid.UUID
	Created  *time.Time // Non-nil for CREATE events, nil for DELETE events
	Deleted  *time.Time // Non-nil for DELETE events, nil for CREATE events
}

// ManageInventoryMetrics is a wrapper for managing inventory metrics activities
type ManageInventoryMetrics struct {
	dbSession *cdb.Session
	latency   *prometheus.HistogramVec
	siteNames *SiteNameCache
}

// RecordLatency is a Temporal activity that records the latency of inventory processing activities
func (mim *ManageInventoryMetrics) RecordLatency(ctx context.Context, siteID uuid.UUID, activity string, isFailed bool, duration time.Duration) error {
	// This method is called by inventory workflows
	// NOTE: Temporal will cache the arguments to this call, even if this activity is scheduled a bit later, we'll still get the correct latency
	status := InventoryStatusSuccess
	if isFailed {
		status = InventoryStatusFailed
	}

	siteName, err := mim.siteNames.Get(ctx, mim.dbSession, siteID)
	if err != nil {
		return err
	}

	mim.latency.WithLabelValues(siteName, siteID.String(), activity, status).Observe(duration.Seconds())

	return nil
}

// InitInventoryMetrics initializes inventory activity metrics
func NewManageInventoryMetrics(reg prometheus.Registerer, dbSession *cdb.Session, namespace string) *ManageInventoryMetrics {
	inventoryMetrics := &ManageInventoryMetrics{
		dbSession: dbSession,
		latency: prometheus.NewHistogramVec(
			prometheus.HistogramOpts{
				Namespace: namespace,
				Name:      "inventory_latency_seconds",
				Help:      "Latency of each inventory call, measured across the whole workflow including activity retries",
				// Top bucket covers a fully retried run under the shared inventory
				// budget, which is 2 x 60s plus 5s of backoff. Only SSH Key Group sets
				// its own longer timeout, so its slowest runs fall in +Inf.
				Buckets: []float64{0.0005, 0.001, 0.005, 0.010, 0.025, 0.050, 0.100, 0.250, 0.500, 1.0, 2.5, 5.0, 10.0, 30.0, 60.0, 125.0},
			},
			[]string{"site", "site_id", "activity", "status"}),

		siteNames: NewSiteNameCache(),
	}
	reg.MustRegister(inventoryMetrics.latency)

	return inventoryMetrics
}
