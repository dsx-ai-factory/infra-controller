// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package metrics

import (
	"context"
	"time"

	"github.com/prometheus/client_golang/prometheus"

	cdb "github.com/NVIDIA/infra-controller/rest-api/db/pkg/db"
	cdbm "github.com/NVIDIA/infra-controller/rest-api/db/pkg/db/model"
)

// PendingExpectedMachineBatchMetrics reads reservations on each scrape, so an
// idle inventory worker or a restarted replica still reports current values.
type PendingExpectedMachineBatchMetrics struct {
	dao           cdbm.PendingExpectedMachineBatchDAO
	batchCount    *prometheus.Desc
	oldestCreated *prometheus.Desc
}

// NewPendingExpectedMachineBatchMetrics registers per-Site reservation gauges.
func NewPendingExpectedMachineBatchMetrics(reg prometheus.Registerer, session *cdb.Session, namespace string) *PendingExpectedMachineBatchMetrics {
	metrics := &PendingExpectedMachineBatchMetrics{
		dao: cdbm.NewPendingExpectedMachineBatchDAO(session),
		batchCount: prometheus.NewDesc(
			prometheus.BuildFQName(namespace, "", "site_pending_expected_machine_batches"),
			"Number of unresolved Expected Machine batch reservations for a Site", []string{"site", "site_id"}, nil),
		oldestCreated: prometheus.NewDesc(
			prometheus.BuildFQName(namespace, "", "site_oldest_pending_expected_machine_batch_created_timestamp_seconds"),
			"Unix creation timestamp of the oldest unresolved Expected Machine batch reservation for a Site, or 0 if none exist", []string{"site", "site_id"}, nil),
	}
	reg.MustRegister(metrics)
	return metrics
}

// Describe implements prometheus.Collector.
func (m *PendingExpectedMachineBatchMetrics) Describe(ch chan<- *prometheus.Desc) {
	ch <- m.batchCount
	ch <- m.oldestCreated
}

// Collect implements prometheus.Collector. A database failure fails the scrape
// rather than reporting that no reservations remain.
func (m *PendingExpectedMachineBatchMetrics) Collect(ch chan<- prometheus.Metric) {
	ctx, cancel := context.WithTimeout(context.Background(), 5*time.Second)
	defer cancel()

	stats, err := m.dao.GetStatistics(ctx, nil)
	if err != nil {
		ch <- prometheus.NewInvalidMetric(m.batchCount, err)
		return
	}
	for _, site := range stats {
		var oldestCreated float64
		if site.OldestCreated != nil {
			oldestCreated = float64(site.OldestCreated.Unix())
		}
		ch <- prometheus.MustNewConstMetric(m.batchCount, prometheus.GaugeValue, float64(site.BatchCount), site.SiteName, site.SiteID.String())
		ch <- prometheus.MustNewConstMetric(m.oldestCreated, prometheus.GaugeValue, oldestCreated, site.SiteName, site.SiteID.String())
	}
}
