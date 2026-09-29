// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package workflow

import (
	"context"
	"errors"

	"go.temporal.io/sdk/client"
)

var errWorkerNotStarted = errors.New("no Temporal worker has started yet")

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

// CheckReadiness returns nil once the Temporal worker is running and every Temporal
// client it was started with reaches the Temporal frontend.
func (wflow *API) CheckReadiness(ctx context.Context) error {
	status := ManagerAccess.Data.EB.Managers.Workflow.State.Worker()
	if status == nil {
		return errWorkerNotStarted
	}
	err := status.Err()
	if err != nil {
		return err
	}
	for _, temporalClient := range status.Clients {
		_, err = temporalClient.CheckHealth(ctx, &client.CheckHealthRequest{})
		if err != nil {
			return err
		}
	}
	return nil
}
