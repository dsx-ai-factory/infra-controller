// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package site

import (
	"context"

	"github.com/google/uuid"
	"github.com/rs/zerolog"
	tsdkClient "go.temporal.io/sdk/client"

	cdbm "github.com/NVIDIA/infra-controller/rest-api/db/pkg/db/model"
	"github.com/NVIDIA/infra-controller/rest-api/workflow/pkg/queue"
)

// StartTenantCreate triggers Tenant creation on the Site; inventory reconciliation retries failures.
func (cp *ClientPool) StartTenantCreate(ctx context.Context, logger zerolog.Logger, tenant *cdbm.Tenant, siteID uuid.UUID) tsdkClient.WorkflowRun {
	var we tsdkClient.WorkflowRun
	stc, err := cp.GetClientByID(siteID)
	if err == nil {
		we, err = stc.ExecuteWorkflow(ctx, tsdkClient.StartWorkflowOptions{
			ID: "site-tenant-create-" + tenant.Org, TaskQueue: queue.SiteTaskQueue,
		}, "CreateTenant", tenant.ToCreateRequestProto())
	}
	if err != nil {
		logger.Error().Err(err).Str("Site ID", siteID.String()).Str("Tenant ID", tenant.ID.String()).Msg("failed to trigger Tenant creation; inventory reconciliation will retry")
	}
	return we
}
