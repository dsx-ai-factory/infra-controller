// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package workflow

import (
	"context"

	"go.temporal.io/sdk/client"

	computils "github.com/NVIDIA/infra-controller/rest-api/site-agent/pkg/components/utils"
)

// CheckLiveness returns why the Site Agent has no Temporal worker: its latest
// connection attempt failed, or the Temporal SDK stopped the worker on an error it
// does not retry. Neither recovers on its own. The SDK never restarts a stopped
// worker, and a failed attempt is only retried when the certificate files change.
// It returns nil before the first attempt, which waits for Core gRPC, and while an
// attempt is in progress.
func (wflow *API) CheckLiveness() error {
	status := ManagerAccess.Data.EB.Managers.Workflow.State.Worker()
	if status == nil {
		return nil
	}
	return status.Err()
}

// CheckConnection checks that the Temporal worker is running and that every Temporal
// client it was started with reaches the Temporal frontend, and records the result in
// the Temporal state. A connection attempt in progress records its own outcome.
func (wflow *API) CheckConnection(ctx context.Context) {
	state := ManagerAccess.Data.EB.Managers.Workflow.State
	status := state.Worker()
	if status == nil {
		return
	}
	err := status.Err()
	if err == nil {
		err = checkHealth(ctx, status.Clients)
	}
	// A reconnect that finished during the check has recorded a newer outcome.
	if state.Worker() != status {
		return
	}
	// A worker that stopped during the check has failed, whatever the frontend says.
	stopErr := status.Err()
	if stopErr != nil {
		err = stopErr
	}

	log := ManagerAccess.Data.EB.Log
	if err != nil {
		state.SetErr(err.Error())
		previous := state.HealthStatus.Swap(uint64(computils.CompUnhealthy))
		if computils.CompStatus(previous) == computils.CompHealthy {
			log.Warn().Err(err).Msg("Workflow: Temporal health check failed")
		}
		return
	}
	previous := state.HealthStatus.Swap(uint64(computils.CompHealthy))
	if computils.CompStatus(previous) != computils.CompHealthy {
		log.Info().Msg("Workflow: Temporal health check passed")
	}
}

func checkHealth(ctx context.Context, temporalClients []client.Client) error {
	for _, temporalClient := range temporalClients {
		_, err := temporalClient.CheckHealth(ctx, &client.CheckHealthRequest{})
		if err != nil {
			return err
		}
	}
	return nil
}
