// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package metrics

import (
	"context"
	"sync"
	"testing"
	"time"

	"github.com/google/uuid"
	"github.com/prometheus/client_golang/prometheus"
	"github.com/prometheus/client_golang/prometheus/collectors"
	"github.com/stretchr/testify/assert"

	cdbm "github.com/NVIDIA/infra-controller/rest-api/db/pkg/db/model"
	"github.com/NVIDIA/infra-controller/rest-api/workflow/pkg/util"
)

func TestManageInventoryMetrics_RecordLatency(t *testing.T) {
	dbSession := util.TestInitDB(t)
	defer dbSession.Close()

	util.TestSetupSchema(t, dbSession)

	ipOrg := "test-provider-org"
	ipRoles := []string{"FORGE_PROVIDER_ADMIN"}

	ipu := util.TestBuildUser(t, dbSession, uuid.NewString(), []string{ipOrg}, ipRoles)
	ip := util.TestBuildInfrastructureProvider(t, dbSession, "test-provider", ipOrg, ipu)

	site := util.TestBuildSite(t, dbSession, ip, "test-site-1", cdbm.SiteStatusRegistered, nil, ipu)
	assert.NotNil(t, site)

	reg := prometheus.NewRegistry()
	reg.MustRegister(collectors.NewGoCollector())

	inventoryMetricsManager := NewManageInventoryMetrics(reg, dbSession, "nico_rest_workflow")

	t.Run("records an observation and caches the Site name", func(t *testing.T) {
		err := inventoryMetricsManager.RecordLatency(context.Background(), site.ID, "test-workflow", false, time.Second)
		assert.NoError(t, err)

		util.TestAssertMetricExistsTimes(t, reg, "nico_rest_workflow_inventory_latency_seconds", 1, map[string]string{
			"activity": "test-workflow",
			"site":     site.Name,
			"site_id":  site.ID.String(),
			"status":   InventoryStatusSuccess,
		}, 0)

		assert.Equal(t, 1, inventoryMetricsManager.siteNames.Len())
	})

	// The worker dispatches this activity concurrently against one registered
	// instance, and a cold cache is what makes that racy: every caller misses,
	// reads the Site, then writes the same map. Run under -race.
	t.Run("resolves the Site name under concurrent callers", func(t *testing.T) {
		const callers = 16

		concurrentReg := prometheus.NewRegistry()
		concurrentManager := NewManageInventoryMetrics(concurrentReg, dbSession, "nico_rest_workflow")

		var wg sync.WaitGroup
		for range callers {
			wg.Add(1)
			go func() {
				defer wg.Done()
				cerr := concurrentManager.RecordLatency(context.Background(), site.ID, "test-workflow", false, time.Second)
				assert.NoError(t, cerr)
			}()
		}
		wg.Wait()

		metrics, err := concurrentReg.Gather()
		assert.NoError(t, err)
		assert.Len(t, metrics, 1)
		assert.Equal(t, "nico_rest_workflow_inventory_latency_seconds", metrics[0].GetName())
		assert.Len(t, metrics[0].Metric, 1)
		assert.Equal(t, uint64(callers), metrics[0].Metric[0].Histogram.GetSampleCount())
	})
}
