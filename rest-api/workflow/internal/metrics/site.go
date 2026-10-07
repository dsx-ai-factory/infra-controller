// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package metrics

import (
	"time"

	"github.com/google/uuid"
	"github.com/prometheus/client_golang/prometheus"
)

// SiteHealthReport is one monitored Site's health signals for a single monitor
// cycle. Both timestamps are nil when the Site has never reported that signal.
type SiteHealthReport struct {
	SiteID            uuid.UUID
	SiteName          string
	InventoryReceived *time.Time
	AgentCertExpiry   *time.Time
}

// SiteHealthMetrics publishes per-Site health signals gathered by the Site
// health monitor cron.
//
// The cron is a single Temporal chain, so one Cloud worker replica runs each
// cycle and only that replica updates its own gauges. With
// cloudWorker.replicaCount above 1, Prometheus scrapes replicas whose values are
// as old as the last cycle they won, so an alert on these gauges has to collapse
// the replicas first, as in max by (site_id) (...). A replica scaled up
// mid-flight reports nothing until it wins a cycle.
type SiteHealthMetrics struct {
	lastInventoryReceipt *prometheus.GaugeVec
	agentCertExpiry      *prometheus.GaugeVec
}

// SetSiteHealth republishes every gauge for every monitored Site, as Unix
// timestamps that operators compare against time(). A signal the Site has never
// reported is published as 0 rather than left absent, so a Site that never
// connects is as visible as one that stops, and 0 reads as overdue under the
// same comparison that covers a real timestamp.
//
// Both vectors are rebuilt from the caller's set on each cycle. Without that, a
// deleted or de-registered Site would keep its last value and age into an alert
// nothing can clear.
func (shm *SiteHealthMetrics) SetSiteHealth(reports []SiteHealthReport) {
	// Nil when the worker was built with metrics disabled, and in activity tests.
	if shm == nil {
		return
	}

	shm.lastInventoryReceipt.Reset()
	shm.agentCertExpiry.Reset()

	for _, report := range reports {
		site, siteID := report.SiteName, report.SiteID.String()

		var inventoryReceived float64
		if report.InventoryReceived != nil {
			inventoryReceived = float64(report.InventoryReceived.Unix())
		}
		shm.lastInventoryReceipt.WithLabelValues(site, siteID).Set(inventoryReceived)

		var agentCertExpiry float64
		if report.AgentCertExpiry != nil {
			agentCertExpiry = float64(report.AgentCertExpiry.Unix())
		}
		shm.agentCertExpiry.WithLabelValues(site, siteID).Set(agentCertExpiry)
	}
}

// NewSiteHealthMetrics initializes per-Site health metrics
func NewSiteHealthMetrics(reg prometheus.Registerer, namespace string) *SiteHealthMetrics {
	siteHealthMetrics := &SiteHealthMetrics{
		lastInventoryReceipt: prometheus.NewGaugeVec(
			prometheus.GaugeOpts{
				Namespace: namespace,
				Name:      "site_last_inventory_receipt_timestamp_seconds",
				Help:      "Unix timestamp of the last Machine inventory received from a monitored Site, or 0 if none has been received. Published by one worker replica per cycle, so aggregate with max by (site_id)",
			},
			[]string{"site", "site_id"}),

		agentCertExpiry: prometheus.NewGaugeVec(
			prometheus.GaugeOpts{
				Namespace: namespace,
				Name:      "site_agent_cert_expiry_timestamp_seconds",
				Help:      "Unix timestamp at which a monitored Site's Site Agent Temporal certificate expires, or 0 if the Site has never reported one. Published by one worker replica per cycle, so aggregate with max by (site_id)",
			},
			[]string{"site", "site_id"}),
	}
	reg.MustRegister(siteHealthMetrics.lastInventoryReceipt, siteHealthMetrics.agentCertExpiry)

	return siteHealthMetrics
}
