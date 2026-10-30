// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package eventrule

import (
	"slices"
	"testing"

	taskcommon "github.com/NVIDIA/infra-controller/rest-api/flow/internal/task/common"
	"github.com/google/uuid"
	"github.com/stretchr/testify/require"
)

func TestHandling_Result(t *testing.T) {
	executionID := uuid.New()
	single := func(status ExecutionStatus, reason ExecutionReason, taskStatuses ...taskcommon.TaskStatus) Handling {
		h := Handling{
			AppliedPolicy: Policy{Actions: []Action{{Name: "shutdown", Spec: &SubmitTask{}}}},
			Executions:    []HandlingExecution{{ID: executionID, ActionName: "shutdown", Status: status, Reason: reason}},
		}
		for _, status := range taskStatuses {
			h.Tasks = append(h.Tasks, HandlingTask{ID: uuid.New(), ExecutionID: executionID, Status: status})
		}
		return h
	}
	withSecond := func(h Handling, status ExecutionStatus, reason ExecutionReason) Handling {
		h.AppliedPolicy.Actions = append(h.AppliedPolicy.Actions, Action{Name: "alert", Spec: &SendAlert{}})
		h.Executions = append(h.Executions, HandlingExecution{ID: uuid.New(), ActionName: "alert", Status: status, Reason: reason})
		return h
	}
	missing := single(ExecutionStatusCompleted, "")
	missing.Executions = nil
	duplicate := single(ExecutionStatusCompleted, "")
	duplicate.Executions = append(duplicate.Executions, duplicate.Executions[0])
	unrelated := single(ExecutionStatusCompleted, "", taskcommon.TaskStatusCompleted)
	unrelated.Tasks[0].ExecutionID = uuid.New()
	alertOnly := withSecond(Handling{}, ExecutionStatusCompleted, "")
	noop := Handling{AppliedPolicy: Policy{Actions: []Action{{Name: "record", Spec: &Noop{}}}}, Executions: []HandlingExecution{{ActionName: "record", Status: ExecutionStatusCompleted}}}
	retry := "A handling action failed; another attempt is scheduled."
	incomplete := "Handling progress is unavailable because execution or task information is incomplete."
	starting := "Handling actions are being started."
	running := "Handling tasks are in progress."
	completed := "Handling actions and their tasks completed successfully."
	actionFailed := "One or more handling actions could not be completed."
	taskFailed := "One or more handling tasks failed or were terminated."
	skipped := "One or more handling actions had no eligible targets."
	noAction := "No action was required by the applied policy."
	tests := []struct {
		name     string
		handling Handling
		want     HandlingResult
	}{
		{"no applicable actions", Handling{}, HandlingResult{Status: HandlingStatusNoAction, Message: noAction}},
		{"completed no-op", noop, HandlingResult{Status: HandlingStatusNoAction, Message: noAction}},
		{"completed alert requires no Tasks", alertOnly, HandlingResult{Status: HandlingStatusCompleted, Message: completed}},
		{"all submitted Tasks completed", single(ExecutionStatusCompleted, "", taskcommon.TaskStatusCompleted), HandlingResult{Status: HandlingStatusCompleted, Message: completed}},
		{"waiting pending and running Tasks", single(ExecutionStatusCompleted, "", taskcommon.TaskStatusWaiting, taskcommon.TaskStatusPending, taskcommon.TaskStatusRunning), HandlingResult{Status: HandlingStatusRunning, Message: running}},
		{"unknown Task state", single(ExecutionStatusCompleted, "", taskcommon.TaskStatusCompleted, taskcommon.TaskStatusUnknown), HandlingResult{Status: HandlingStatusUnknown, Reason: HandlingReasonIncompleteState, Message: incomplete}},
		{"submission completed without Tasks", single(ExecutionStatusCompleted, ""), HandlingResult{Status: HandlingStatusUnknown, Reason: HandlingReasonIncompleteState, Message: incomplete}},
		{"another execution's Tasks cannot satisfy submission", unrelated, HandlingResult{Status: HandlingStatusUnknown, Reason: HandlingReasonIncompleteState, Message: incomplete}},
		{"missing matching execution", missing, HandlingResult{Status: HandlingStatusUnknown, Reason: HandlingReasonIncompleteState, Message: incomplete}},
		{"duplicate matching execution", duplicate, HandlingResult{Status: HandlingStatusUnknown, Reason: HandlingReasonIncompleteState, Message: incomplete}},
		{"unknown execution status", single("future", ""), HandlingResult{Status: HandlingStatusUnknown, Reason: HandlingReasonIncompleteState, Message: incomplete}},
		{"pending dispatch after partial submission", single(ExecutionStatusPending, "", taskcommon.TaskStatusCompleted), HandlingResult{Status: HandlingStatusStarting, Message: starting}},
		{"running dispatch before Tasks", single(ExecutionStatusRunning, ""), HandlingResult{Status: HandlingStatusStarting, Message: starting}},
		{"retry after failed submission", single(ExecutionStatusDeferred, ExecutionReasonAttemptFailed), HandlingResult{Status: HandlingStatusRetrying, Reason: HandlingReasonAttemptFailed, Message: retry}},
		{"interrupted retry with partial Tasks", single(ExecutionStatusDeferred, ExecutionReasonAttemptInterrupted, taskcommon.TaskStatusCompleted), HandlingResult{Status: HandlingStatusRetrying, Reason: HandlingReasonAttemptInterrupted, Message: "A handling attempt was interrupted; another attempt is scheduled."}},
		{"retry with unspecified reason", single(ExecutionStatusDeferred, ""), HandlingResult{Status: HandlingStatusRetrying, Reason: HandlingReasonRetryScheduled, Message: "Another handling attempt is scheduled."}},
		{"action failure after completed Task", single(ExecutionStatusFailed, "", taskcommon.TaskStatusCompleted), HandlingResult{Status: HandlingStatusFailed, Reason: HandlingReasonActionFailed, Message: actionFailed}},
		{"failed Task overrides partially completed work", single(ExecutionStatusCompleted, "", taskcommon.TaskStatusCompleted, taskcommon.TaskStatusRunning, taskcommon.TaskStatusFailed), HandlingResult{Status: HandlingStatusFailed, Reason: HandlingReasonTaskFailed, Message: taskFailed}},
		{"terminated Task overrides retry", single(ExecutionStatusDeferred, ExecutionReasonAttemptFailed, taskcommon.TaskStatusTerminated), HandlingResult{Status: HandlingStatusFailed, Reason: HandlingReasonTaskFailed, Message: taskFailed}},
		{"no eligible targets", single(ExecutionStatusSkipped, ExecutionReasonNoTargets), HandlingResult{Status: HandlingStatusSkipped, Reason: HandlingReasonNoTargets, Message: skipped}},
		{"skip without recognized reason", single(ExecutionStatusSkipped, ""), HandlingResult{Status: HandlingStatusSkipped, Message: "One or more handling actions were skipped."}},
		{"failure overrides another action's retry", withSecond(single(ExecutionStatusDeferred, ExecutionReasonAttemptFailed), ExecutionStatusFailed, ""), HandlingResult{Status: HandlingStatusFailed, Reason: HandlingReasonActionFailed, Message: actionFailed}},
		{"retry overrides another action starting", withSecond(single(ExecutionStatusDeferred, ExecutionReasonAttemptFailed), ExecutionStatusPending, ""), HandlingResult{Status: HandlingStatusRetrying, Reason: HandlingReasonAttemptFailed, Message: retry}},
		{"starting overrides unknown action state", withSecond(single("future", ""), ExecutionStatusPending, ""), HandlingResult{Status: HandlingStatusStarting, Message: starting}},
		{"unknown action prevents reporting running", withSecond(single(ExecutionStatusCompleted, "", taskcommon.TaskStatusRunning), "future", ""), HandlingResult{Status: HandlingStatusUnknown, Reason: HandlingReasonIncompleteState, Message: incomplete}},
		{"active Tasks override skipped action", withSecond(single(ExecutionStatusCompleted, "", taskcommon.TaskStatusRunning), ExecutionStatusSkipped, ExecutionReasonNoTargets), HandlingResult{Status: HandlingStatusRunning, Message: running}},
		{"partial skip after other work completed", withSecond(single(ExecutionStatusCompleted, "", taskcommon.TaskStatusCompleted), ExecutionStatusSkipped, ExecutionReasonNoTargets), HandlingResult{Status: HandlingStatusSkipped, Reason: HandlingReasonNoTargets, Message: skipped}},
		{"retry diagnostic independent of action ordering", withSecond(single(ExecutionStatusDeferred, ExecutionReasonAttemptInterrupted), ExecutionStatusDeferred, ExecutionReasonAttemptFailed), HandlingResult{Status: HandlingStatusRetrying, Reason: HandlingReasonAttemptFailed, Message: retry}},
	}
	for _, test := range tests {
		t.Run(test.name, func(t *testing.T) {
			require.Equal(t, test.want, test.handling.Result())
			if len(test.handling.AppliedPolicy.Actions) > 1 {
				slices.Reverse(test.handling.AppliedPolicy.Actions)
				slices.Reverse(test.handling.Executions)
				require.Equal(t, test.want, test.handling.Result(), "action ordering must not affect the aggregate")
			}
		})
	}
}
