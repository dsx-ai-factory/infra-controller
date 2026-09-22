// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package executor

import (
	"context"
	"fmt"
	"strings"

	"github.com/NVIDIA/infra-controller/rest-api/flow/internal/eventrule"
	"github.com/google/uuid"
)

// AlertRequest contains one idempotent downstream alert submission.
type AlertRequest struct {
	IdempotencyKey string
	Severity       eventrule.Severity
	Message        string
}

// AlertSender submits an alert under a stable idempotency key and returns the
// stable downstream alert identity.
type AlertSender interface {
	SendAlert(context.Context, AlertRequest) (string, error)
}

// AlertExecutor submits send_alert actions through an idempotent alert sender.
type AlertExecutor struct {
	sender AlertSender
}

// Execute submits one alert under a stable idempotency key.
func (e *AlertExecutor) Execute(ctx context.Context, request ExecutionRequest) error {
	if e == nil || e.sender == nil {
		return terminalError(fmt.Errorf("alert sender is required"))
	}

	plan, err := alertPlan(request.Plan)
	if err != nil {
		return terminalError(err)
	}

	req := AlertRequest{
		IdempotencyKey: alertIdempotencyKey(request.ExecutionID),
		Severity:       plan.Severity,
		Message:        plan.Message,
	}

	alertID, err := e.sender.SendAlert(ctx, req)
	if err != nil {
		return retryableError("submit alert", err)
	}

	if strings.TrimSpace(alertID) == "" {
		return terminalError(fmt.Errorf("alert sender returned an empty alert id"))
	}

	return nil
}

func alertPlan(plan eventrule.ExecutionPlan) (*eventrule.SendAlertPlan, error) {
	typed, ok := plan.(*eventrule.SendAlertPlan)
	if !ok {
		return nil, fmt.Errorf("alert executor received plan %T", plan)
	}

	if typed == nil {
		return nil, fmt.Errorf("send-alert execution plan is required")
	}

	return typed, nil
}

func alertIdempotencyKey(executionID uuid.UUID) string {
	return "event-rule-execution:" + executionID.String() + ":alert"
}

var _ Executor = (*AlertExecutor)(nil)
