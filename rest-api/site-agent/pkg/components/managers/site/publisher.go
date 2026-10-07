// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package site

import (
	"github.com/google/uuid"
	"google.golang.org/protobuf/proto"
	"google.golang.org/protobuf/types/known/durationpb"

	corev1 "github.com/NVIDIA/infra-controller/rest-api/proto/core/gen/v1"
	wfmgr "github.com/NVIDIA/infra-controller/rest-api/site-agent/pkg/components/managers/workflow"
	"github.com/NVIDIA/infra-controller/rest-api/site-agent/pkg/metadata"
	swa "github.com/NVIDIA/infra-controller/rest-api/site-workflow/pkg/activity"
	swu "github.com/NVIDIA/infra-controller/rest-api/site-workflow/pkg/util"
	sww "github.com/NVIDIA/infra-controller/rest-api/site-workflow/pkg/workflow"
)

// RegisterPublisher registers Site Config inventory workflow and activity with Temporal.
func (api *API) RegisterPublisher() error {
	ManagerAccess.Data.EB.Log.Info().Msg("Site: Registering Site Config inventory workflow and activity")

	ManagerAccess.Data.EB.Managers.Workflow.Temporal.Worker.RegisterWorkflow(sww.DiscoverSiteConfigInventory)
	ManagerAccess.Data.EB.Log.Info().Msg("Site: Successfully registered DiscoverSiteConfigInventory workflow")

	siteID, err := uuid.Parse(ManagerAccess.Conf.EB.Temporal.ClusterID)
	if err != nil {
		ManagerAccess.Data.EB.Log.Error().Err(err).Msg("Site: invalid Temporal ClusterID")
		return err
	}

	siteAgentBuildInfo := &corev1.SiteAgentBuildInfo{
		Version:     metadata.Version,
		FlowEnabled: proto.Bool(ManagerAccess.Conf.EB.FlowGrpc.Enabled),
	}

	// Cloud decides when reported data is stale from this interval, so an unparseable schedule
	// leaves it unset rather than reporting a wrong number. The cron registration below fails on
	// the same schedule anyway.
	schedule := wfmgr.EffectiveCronSchedule()
	inventoryInterval, err := swu.InventoryIntervalFromSchedule(schedule)
	if err != nil {
		ManagerAccess.Data.EB.Log.Error().Err(err).Str("Schedule", schedule).
			Msg("Site: could not derive the inventory interval from the configured schedule")
	} else {
		siteAgentBuildInfo.InventoryInterval = durationpb.New(inventoryInterval)
	}

	inventoryManager := swa.NewManageSiteConfigInventory(swa.ManageInventoryConfig{
		SiteID:                siteID,
		CoreGrpcAtomicClient:  ManagerAccess.Data.EB.Managers.CoreGrpc.Client,
		TemporalPublishClient: ManagerAccess.Data.EB.Managers.Workflow.Temporal.Publisher,
		TemporalPublishQueue:  ManagerAccess.Conf.EB.Temporal.TemporalPublishQueue,
	}, siteAgentBuildInfo)

	ManagerAccess.Data.EB.Managers.Workflow.Temporal.Worker.RegisterActivity(inventoryManager.DiscoverSiteConfigInventory)
	ManagerAccess.Data.EB.Log.Info().Msg("Site: Successfully registered DiscoverSiteConfigInventory activity")

	return api.RegisterCron()
}
