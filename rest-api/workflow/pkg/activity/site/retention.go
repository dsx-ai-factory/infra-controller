// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package site

import (
	"context"
	"errors"
	"fmt"
	"strings"
	"time"

	"github.com/rs/zerolog/log"
	tBatchv1 "go.temporal.io/api/batch/v1"
	temporalEnums "go.temporal.io/api/enums/v1"
	"go.temporal.io/api/serviceerror"
	tWorkflowv1 "go.temporal.io/api/workflowservice/v1"
	"go.temporal.io/sdk/activity"
	"go.temporal.io/sdk/client"

	cwfn "github.com/NVIDIA/infra-controller/rest-api/workflow/pkg/namespace"
)

// inventoryWorkflowTypes lists the inventory workflows the Site worker registers. Keep in sync with
// cmd/workflow/main.go, since a type missing here keeps its successful runs for the full namespace
// retention.
var inventoryWorkflowTypes = []string{
	"UpdateMachineInventory",
	"UpdateVpcInventory",
	"UpdateSubnetInventory",
	"UpdateInstanceInventory",
	"UpdateSiteConfigInventory",
	"UpdateSiteConfigInventoryV2",
	"UpdateSSHKeyGroupInventory",
	"UpdateInfiniBandPartitionInventory",
	"UpdateSpectrumXPartitionInventory",
	"UpdateTenantInventory",
	"UpdateInstanceTypeInventory",
	"UpdateNetworkSecurityGroupInventory",
	"UpdateOsImageInventory",
	"UpdateOperatingSystemInventory",
	"UpdateIpxeTemplateInventory",
	"UpdateVpcPrefixInventory",
	"UpdateSitePrefixInventory",
	"UpdateVpcPeeringInventory",
	"UpdateExpectedMachineInventory",
	"UpdateExpectedPowerShelfInventory",
	"UpdateExpectedRackInventory",
	"UpdateExpectedRackGroupInventory",
	"UpdateExpectedSwitchInventory",
	"UpdateSkuInventory",
	"UpdateDpuExtensionServiceInventory",
	"UpdateNVLinkLogicalPartitionInventory",
}

// ManageInventoryRetention is an activity wrapper for limiting how long successful inventory
// workflows are kept in the Site namespace
type ManageInventoryRetention struct {
	tc                  client.Client
	successfulRetention time.Duration
}

// NewManageInventoryRetention returns a new ManageInventoryRetention activity wrapper. A zero
// successfulRetention leaves successful inventory workflows to the namespace retention.
func NewManageInventoryRetention(tc client.Client, successfulRetention time.Duration) ManageInventoryRetention {
	return ManageInventoryRetention{
		tc:                  tc,
		successfulRetention: successfulRetention,
	}
}

// StartSuccessfulInventoryWorkflowDeletion is a Temporal activity that starts a batch job deleting
// the inventory workflows in the Site namespace that completed more than the successful retention
// ago. Temporal runs the job server side, so the activity returns once the job has started.
func (mir ManageInventoryRetention) StartSuccessfulInventoryWorkflowDeletion(ctx context.Context) error {
	logger := log.With().Str("Activity", "StartSuccessfulInventoryWorkflowDeletion").Logger()

	logger.Info().Msg("starting activity")

	if mir.successfulRetention == 0 {
		logger.Info().Msg("successful inventory retention is disabled, skipping deletion")
		return nil
	}

	// One job per cron run, so a retry after a lost response finds the job it already started
	info := activity.GetInfo(ctx)
	jobID := fmt.Sprintf("%s-%s", info.WorkflowExecution.ID, info.WorkflowExecution.RunID)
	logger = logger.With().Str("Job ID", jobID).Logger()

	// Only Completed matches, so failed, canceled, terminated, and timed out runs stay for the full
	// namespace retention
	cutoff := time.Now().UTC().Add(-mir.successfulRetention)
	query := fmt.Sprintf("ExecutionStatus = 'Completed' AND CloseTime < '%s' AND WorkflowType IN ('%s')",
		cutoff.Format(time.RFC3339), strings.Join(inventoryWorkflowTypes, "', '"))

	_, err := mir.tc.WorkflowService().StartBatchOperation(ctx, &tWorkflowv1.StartBatchOperationRequest{
		Namespace:       cwfn.SiteNamespace,
		JobId:           jobID,
		Reason:          fmt.Sprintf("Delete inventory workflows that completed more than %s ago", mir.successfulRetention),
		VisibilityQuery: query,
		Operation: &tWorkflowv1.StartBatchOperationRequest_DeletionOperation{
			DeletionOperation: &tBatchv1.BatchOperationDeletion{
				Identity: info.WorkflowExecution.ID,
			},
		},
	})
	if err != nil {
		var alreadyStarted *serviceerror.WorkflowExecutionAlreadyStarted
		if errors.As(err, &alreadyStarted) {
			logger.Info().Msg("batch job deleting successful inventory workflows was already started")
			return nil
		}

		// Temporal runs one batch job per namespace at a time by default. Skipping is safe, since the
		// next run's cutoff covers everything this one would have deleted.
		var resourceExhausted *serviceerror.ResourceExhausted
		if errors.As(err, &resourceExhausted) && resourceExhausted.Cause == temporalEnums.RESOURCE_EXHAUSTED_CAUSE_CONCURRENT_LIMIT {
			logger.Info().Err(err).Msg("another batch job is running in the Site namespace, skipping deletion")
			return nil
		}

		logger.Error().Err(err).Msg("failed to start batch job deleting successful inventory workflows")
		return err
	}

	logger.Info().Str("Cutoff", cutoff.Format(time.RFC3339)).Msg("started batch job deleting successful inventory workflows")

	return nil
}
