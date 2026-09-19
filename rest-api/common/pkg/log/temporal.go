// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

// Package log builds the loggers this service hands to the Temporal SDK and the
// loggers workflow and activity code should use, so every Temporal log line
// lands in the same zerolog stream, with the same fields and writers, as the
// rest of the service.
package log

import (
	"context"

	"github.com/rs/zerolog"
	zlog "github.com/rs/zerolog/log"
	"go.temporal.io/sdk/activity"
	temporallog "go.temporal.io/sdk/log"
	"go.temporal.io/sdk/workflow"
	zlogadapter "logur.dev/adapter/zerolog"
	"logur.dev/logur"
)

// Field names shared by the workflow and activity loggers. `Workflow` and
// `Activity` keep the names the hand-rolled loggers already emitted so existing
// log queries keep working, while the values now come from the Temporal SDK
// rather than a literal that can drift from the registered name.
const (
	fieldWorkflow   = "Workflow"
	fieldActivity   = "Activity"
	fieldWorkflowID = "WorkflowID"
	fieldRunID      = "RunID"
	fieldActivityID = "ActivityID"
	fieldAttempt    = "Attempt"
)

// TemporalClientLogger returns the logger to pass as `client.Options.Logger`.
// The worker copies it out of the client, so it backs the SDK's own lines plus
// everything reached through `workflow.GetLogger` and `activity.GetLogger`.
//
// zerolog.Logger is a value, so the returned logger is a snapshot of the global
// logger. Call this only after the process has finished configuring that logger,
// otherwise Temporal logs miss writers added later, such as the Sentry writer.
func TemporalClientLogger() temporallog.Logger {
	return logur.LoggerToKV(zlogadapter.New(zlog.Logger))
}

// TemporalWorkflowLogger returns the global logger tagged with the identity of
// the running workflow. During history replay it returns a disabled logger, so
// a replayed workflow does not re-emit the lines its first execution already
// logged.
//
// Only logging and metrics may depend on the replay flag. A workflow must never
// branch on it for anything that changes the sequence of commands it produces.
func TemporalWorkflowLogger(ctx workflow.Context) zerolog.Logger {
	if workflow.IsReplaying(ctx) {
		return zerolog.Nop()
	}

	info := workflow.GetInfo(ctx)

	return zlog.With().
		Str(fieldWorkflow, info.WorkflowType.Name).
		Str(fieldWorkflowID, info.WorkflowExecution.ID).
		Str(fieldRunID, info.WorkflowExecution.RunID).
		Logger()
}

// TemporalActivityLogger returns the global logger tagged with the identity of
// the running activity and of the workflow run that scheduled it. Outside an
// activity context, which is how unit tests invoke activity methods, it returns
// the global logger untagged instead of panicking in `activity.GetInfo`.
func TemporalActivityLogger(ctx context.Context) zerolog.Logger {
	if !activity.IsActivity(ctx) {
		return zlog.Logger
	}

	info := activity.GetInfo(ctx)

	return zlog.With().
		Str(fieldActivity, info.ActivityType.Name).
		Str(fieldActivityID, info.ActivityID).
		Str(fieldWorkflowID, info.WorkflowExecution.ID).
		Str(fieldRunID, info.WorkflowExecution.RunID).
		Int32(fieldAttempt, info.Attempt).
		Logger()
}
