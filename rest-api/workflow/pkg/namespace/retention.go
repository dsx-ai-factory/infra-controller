// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package namespace

import (
	"context"
	"fmt"
	"time"

	"github.com/rs/zerolog/log"
	tnsv1 "go.temporal.io/api/namespace/v1"
	tWorkflowv1 "go.temporal.io/api/workflowservice/v1"
	"go.temporal.io/sdk/client"
	"google.golang.org/protobuf/types/known/durationpb"
)

// RetentionUpdateTimeout bounds how long worker startup waits on Temporal to read and update the
// retention of its namespace
const RetentionUpdateTimeout = 30 * time.Second

// EnsureRetention sets the workflow execution retention of a Temporal namespace when it differs from
// retention. Temporal fixes a workflow's deletion time when the workflow closes, so the change only
// reaches workflows that close after it.
func EnsureRetention(ctx context.Context, tnc client.NamespaceClient, namespace string, retention time.Duration) error {
	logger := log.With().Str("Namespace", namespace).Str("Retention", retention.String()).Logger()

	resp, err := tnc.Describe(ctx, namespace)
	if err != nil {
		return fmt.Errorf("failed to describe Temporal namespace %s: %w", namespace, err)
	}

	current := resp.GetConfig().GetWorkflowExecutionRetentionTtl().AsDuration()
	if current == retention {
		logger.Info().Msg("Temporal namespace retention is already set")
		return nil
	}

	err = tnc.Update(ctx, &tWorkflowv1.UpdateNamespaceRequest{
		Namespace: namespace,
		Config: &tnsv1.NamespaceConfig{
			WorkflowExecutionRetentionTtl: durationpb.New(retention),
		},
	})
	if err != nil {
		return fmt.Errorf("failed to update retention of Temporal namespace %s: %w", namespace, err)
	}

	logger.Info().Str("Previous Retention", current.String()).Msg("updated Temporal namespace retention")

	return nil
}
