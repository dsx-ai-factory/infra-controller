// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package coregrpc

import (
	"context"
	"time"

	coregrpctypes "github.com/NVIDIA/infra-controller/rest-api/site-agent/pkg/datatypes/managertypes/coregrpc"
	"go.temporal.io/sdk/activity"
	"go.temporal.io/sdk/interceptor"
)

// NewWorkflowMetricsInterceptor records the duration and outcome of each Site
// Agent activity attempt, including retries, in the existing workflow histogram.
// Recording on the activity worker avoids workflow-replay observations and
// excludes queue time and retry backoff. Activity names come from registration,
// so workflow IDs and request data do not become metric labels.
func NewWorkflowMetricsInterceptor(metrics coregrpctypes.WorkflowMetrics) interceptor.WorkerInterceptor {
	return &workflowMetricsInterceptor{metrics: metrics}
}

type workflowMetricsInterceptor struct {
	interceptor.WorkerInterceptorBase
	metrics coregrpctypes.WorkflowMetrics
}

func (i *workflowMetricsInterceptor) InterceptActivity(_ context.Context, next interceptor.ActivityInboundInterceptor) interceptor.ActivityInboundInterceptor {
	return &activityMetricsInterceptor{
		ActivityInboundInterceptorBase: interceptor.ActivityInboundInterceptorBase{Next: next},
		metrics:                        i.metrics,
	}
}

type activityMetricsInterceptor struct {
	interceptor.ActivityInboundInterceptorBase
	metrics coregrpctypes.WorkflowMetrics
}

func (i *activityMetricsInterceptor) ExecuteActivity(ctx context.Context, in *interceptor.ExecuteActivityInput) (interface{}, error) {
	start := time.Now()
	status := coregrpctypes.WorkflowStatusActivityFailed
	defer func() {
		i.metrics.RecordLatency(activity.GetInfo(ctx).ActivityType.Name, status, time.Since(start))
	}()

	result, err := i.Next.ExecuteActivity(ctx, in)
	if err == nil {
		status = coregrpctypes.WorkflowStatusSuccess
	}
	return result, err
}
