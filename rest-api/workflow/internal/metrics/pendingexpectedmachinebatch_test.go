// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package metrics

import (
	"context"
	"fmt"
	"strings"
	"testing"
	"time"

	"github.com/google/uuid"
	"github.com/prometheus/client_golang/prometheus"
	"github.com/prometheus/client_golang/prometheus/testutil"
	"github.com/stretchr/testify/require"

	cdbm "github.com/NVIDIA/infra-controller/rest-api/db/pkg/db/model"
	"github.com/NVIDIA/infra-controller/rest-api/workflow/pkg/util"
)

func TestPendingExpectedMachineBatchMetrics_Collect(t *testing.T) {
	ctx := context.Background()
	session := util.TestInitDB(t)
	t.Cleanup(session.Close)
	util.TestSetupSchema(t, session)

	user := util.TestBuildUser(t, session, uuid.NewString(), []string{"test-org"}, []string{"FORGE_PROVIDER_ADMIN"})
	provider := util.TestBuildInfrastructureProvider(t, session, "test-provider", "test-org", user)
	site := util.TestBuildSite(t, session, provider, "reserved-site", cdbm.SiteStatusRegistered, nil, user)
	emptySite := util.TestBuildSite(t, session, provider, "empty-site", cdbm.SiteStatusPending, nil, user)
	dao := cdbm.NewPendingExpectedMachineBatchDAO(session)
	older, newer := time.Unix(1700000000, 0).UTC(), time.Unix(1700000060, 0).UTC()
	for _, batch := range []cdbm.PendingExpectedMachineBatch{
		{SiteID: site.ID, WorkflowID: "older", Created: older, Inputs: []cdbm.ExpectedMachineCreateInput{
			{ExpectedMachineID: uuid.New(), SiteID: site.ID},
			{ExpectedMachineID: uuid.New(), SiteID: site.ID},
		}},
		{SiteID: site.ID, WorkflowID: "newer", Created: newer, Inputs: []cdbm.ExpectedMachineCreateInput{
			{ExpectedMachineID: uuid.New(), SiteID: site.ID},
		}},
	} {
		require.NoError(t, dao.Create(ctx, nil, batch))
	}

	reg := prometheus.NewRegistry()
	NewPendingExpectedMachineBatchMetrics(reg, session, "test_workflow")
	cases := []struct {
		name       string
		prepare    func() error
		count      int
		oldest     int64
		removeSite bool
		wantError  bool
	}{
		{name: "counts batches rather than members without inventory", count: 2, oldest: older.Unix()},
		{name: "oldest advances after one batch resolves", prepare: func() error { return dao.Delete(ctx, nil, site.ID, "older") }, count: 1, oldest: newer.Unix()},
		{name: "both gauges clear after the final batch resolves", prepare: func() error { return dao.Delete(ctx, nil, site.ID, "newer") }},
		{name: "deleted Sites disappear", prepare: func() error { return cdbm.NewSiteDAO(session).Delete(ctx, nil, site.ID) }, removeSite: true},
		{name: "database failure fails the scrape", prepare: func() error { session.Close(); return nil }, wantError: true},
	}
	for _, tc := range cases {
		t.Run(tc.name, func(t *testing.T) {
			if tc.prepare != nil {
				require.NoError(t, tc.prepare())
			}
			if tc.wantError {
				families, err := reg.Gather()
				require.Error(t, err, "an unavailable database must fail the scrape")
				require.Empty(t, families, "a failed query must not publish healthy zero values")
				return
			}
			countSeries, oldestSeries := "", ""
			if !tc.removeSite {
				countSeries = fmt.Sprintf("test_workflow_site_pending_expected_machine_batches{site=%q,site_id=%q} %d\n", site.Name, site.ID.String(), tc.count)
				oldestSeries = fmt.Sprintf("test_workflow_site_oldest_pending_expected_machine_batch_created_timestamp_seconds{site=%q,site_id=%q} %d\n", site.Name, site.ID.String(), tc.oldest)
			}
			want := fmt.Sprintf(`# HELP test_workflow_site_pending_expected_machine_batches Number of unresolved Expected Machine batch reservations for a Site
# TYPE test_workflow_site_pending_expected_machine_batches gauge
test_workflow_site_pending_expected_machine_batches{site=%q,site_id=%q} 0
%s# HELP test_workflow_site_oldest_pending_expected_machine_batch_created_timestamp_seconds Unix creation timestamp of the oldest unresolved Expected Machine batch reservation for a Site, or 0 if none exist
# TYPE test_workflow_site_oldest_pending_expected_machine_batch_created_timestamp_seconds gauge
test_workflow_site_oldest_pending_expected_machine_batch_created_timestamp_seconds{site=%q,site_id=%q} 0
%s`, emptySite.Name, emptySite.ID.String(), countSeries, emptySite.Name, emptySite.ID.String(), oldestSeries)
			require.NoError(t, testutil.GatherAndCompare(reg, strings.NewReader(want)))
		})
	}
}
