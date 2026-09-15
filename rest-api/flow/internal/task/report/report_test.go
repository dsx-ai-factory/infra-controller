/*
 * SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
 * SPDX-License-Identifier: Apache-2.0
 */

package report

import (
	"errors"
	"strings"
	"testing"
	"time"

	"github.com/stretchr/testify/assert"
	"github.com/stretchr/testify/require"

	"github.com/NVIDIA/infra-controller/rest-api/flow/internal/task/operationrules"
	"github.com/NVIDIA/infra-controller/rest-api/flow/pkg/common/devicetypes"
)

// twoStageRule mirrors a force-power-on-style plan: stage 1 powers up
// PowerShelves, stage 2 brings up Compute. It exists to anchor tests
// against a stable rule shape without pulling in the full resolver.
func twoStageRule() *operationrules.RuleDefinition {
	return &operationrules.RuleDefinition{
		Version: "v1",
		Steps: []operationrules.SequenceStep{
			{ComponentType: devicetypes.ComponentTypePowerShelf, Stage: 1},
			{ComponentType: devicetypes.ComponentTypeCompute, Stage: 2},
		},
	}
}

func TestNewInitial_mirrorsRuleAndMarksSkipped(t *testing.T) {
	t.Parallel()

	// totalByType lists Compute only; PowerShelf step has no targets and
	// must be marked skipped.
	totals := map[devicetypes.ComponentType]int{
		devicetypes.ComponentTypeCompute: 4,
	}

	rep := NewInitial(twoStageRule(), totals)

	require.Equal(t, Version, rep.Version)
	require.Len(t, rep.Stages, 2)

	stage1 := rep.Stages[0]
	assert.Equal(t, 1, stage1.Number)
	assert.Equal(t, StatusSkipped, stage1.Status)
	require.Len(t, stage1.Steps, 1)
	assert.Equal(t, "PowerShelf", stage1.Steps[0].ComponentType)
	assert.Equal(t, StatusSkipped, stage1.Steps[0].Status,
		"step with zero targets must start skipped")
	assert.Zero(t, stage1.Steps[0].TotalComponents)

	stage2 := rep.Stages[1]
	assert.Equal(t, 2, stage2.Number)
	require.Len(t, stage2.Steps, 1)
	assert.Equal(t, "Compute", stage2.Steps[0].ComponentType)
	assert.Equal(t, StatusPending, stage2.Steps[0].Status)
	assert.Equal(t, 4, stage2.Steps[0].TotalComponents)
}

func TestNewInitial_nilRule(t *testing.T) {
	t.Parallel()

	rep := NewInitial(nil, nil)

	require.Equal(t, Version, rep.Version)
	assert.Empty(t, rep.Stages)
}

func TestTracker_BeginStage_advancesPendingOnly(t *testing.T) {
	t.Parallel()

	totals := map[devicetypes.ComponentType]int{
		devicetypes.ComponentTypeCompute: 2,
	}
	ruleDef := &operationrules.RuleDefinition{Steps: []operationrules.SequenceStep{
		{ComponentType: devicetypes.ComponentTypePowerShelf, Stage: 1},
		{ComponentType: devicetypes.ComponentTypeCompute, Stage: 1},
	}}
	tr := &Tracker{Report: NewInitial(ruleDef, totals)}
	now := time.Date(2026, 5, 28, 12, 0, 0, 0, time.UTC)

	tr.BeginStage(1, now)
	stage1 := tr.Report.Stages[0]
	assert.Equal(t, StatusRunning, stage1.Status)
	assert.Equal(t, "2026-05-28T12:00:00Z", stage1.StartedAt)
	// Stage 1's only step targets PowerShelf which has zero components,
	// so it must remain skipped after BeginStage.
	assert.Equal(t, StatusSkipped, stage1.Steps[0].Status)
	assert.Empty(t, stage1.Steps[0].StartedAt)
	assert.Equal(t, StatusRunning, stage1.Steps[1].Status)

	// BeginStage on a stage that is already running is a no-op so a
	// retried Temporal activity does not rewrite timestamps.
	tr.BeginStage(1, now.Add(time.Hour))
	assert.Equal(t, "2026-05-28T12:00:00Z", tr.Report.Stages[0].StartedAt)
}

func TestTracker_CompleteStage_finalizesRunningSteps(t *testing.T) {
	t.Parallel()

	totals := map[devicetypes.ComponentType]int{
		devicetypes.ComponentTypeCompute: 3,
	}
	tr := &Tracker{Report: NewInitial(twoStageRule(), totals)}
	startedAt := time.Date(2026, 5, 28, 12, 0, 0, 0, time.UTC)
	finishedAt := startedAt.Add(30 * time.Second)

	tr.BeginStage(2, startedAt)
	tr.CompleteStage(2, finishedAt)

	stage2 := tr.Report.Stages[1]
	assert.Equal(t, StatusCompleted, stage2.Status)
	assert.Equal(t, "2026-05-28T12:00:30Z", stage2.FinishedAt)

	step := stage2.Steps[0]
	assert.Equal(t, StatusCompleted, step.Status)
	assert.Equal(t, "2026-05-28T12:00:00Z", step.StartedAt)
	assert.Equal(t, "2026-05-28T12:00:30Z", step.FinishedAt)
	assert.Empty(t, step.Error)
}

func TestTracker_FailStage_propagatesToRunningStepsAndReportError(t *testing.T) {
	t.Parallel()

	totals := map[devicetypes.ComponentType]int{
		devicetypes.ComponentTypeCompute:    2,
		devicetypes.ComponentTypePowerShelf: 1,
	}
	tr := &Tracker{Report: NewInitial(twoStageRule(), totals)}
	now := time.Date(2026, 5, 28, 12, 0, 0, 0, time.UTC)

	tr.BeginStage(1, now)
	failErr := errors.New("BMC unreachable")
	tr.FailStage(1, failErr, now.Add(15*time.Second))

	stage1 := tr.Report.Stages[0]
	assert.Equal(t, StatusFailed, stage1.Status)
	assert.Equal(t, "BMC unreachable", stage1.Error)
	assert.Equal(t, "2026-05-28T12:00:15Z", stage1.FinishedAt)

	// Under fail-fast the running PowerShelf step must inherit the
	// failure so the wire shape never reports it as still in flight.
	step := stage1.Steps[0]
	assert.Equal(t, StatusFailed, step.Status)
	assert.Equal(t, "BMC unreachable", step.Error)
	assert.Equal(t, "2026-05-28T12:00:15Z", step.FinishedAt)

	assert.Equal(t, "BMC unreachable", tr.Report.Error,
		"first stage failure becomes the canonical task-level error")

	// A second failure must not overwrite the canonical task-level
	// error: the first cause is what callers diagnose against.
	tr.BeginStage(2, now.Add(time.Minute))
	tr.FailStage(2, errors.New("compute boot timed out"), now.Add(2*time.Minute))
	assert.Equal(t, "BMC unreachable", tr.Report.Error)
}

func TestTracker_FailStage_truncatesLongError(t *testing.T) {
	t.Parallel()

	totals := map[devicetypes.ComponentType]int{
		devicetypes.ComponentTypeCompute: 1,
	}
	tr := &Tracker{Report: NewInitial(twoStageRule(), totals)}
	now := time.Date(2026, 5, 28, 12, 0, 0, 0, time.UTC)

	huge := strings.Repeat("x", maxErrLen+50)
	tr.BeginStage(2, now)
	tr.FailStage(2, errors.New(huge), now.Add(time.Second))

	assert.Len(t, tr.Report.Stages[1].Error, maxErrLen)
	assert.Len(t, tr.Report.Error, maxErrLen)
}

func TestTracker_skippedStepsAreNotMutated(t *testing.T) {
	t.Parallel()

	// PowerShelf has no targets so its step starts skipped; the test
	// asserts every Tracker transition leaves it skipped.
	totals := map[devicetypes.ComponentType]int{
		devicetypes.ComponentTypeCompute: 1,
	}
	tr := &Tracker{Report: NewInitial(twoStageRule(), totals)}
	now := time.Date(2026, 5, 28, 12, 0, 0, 0, time.UTC)

	tr.BeginStage(1, now)
	tr.CompleteStage(1, now.Add(time.Second))

	stage1 := tr.Report.Stages[0]
	assert.Equal(t, StatusSkipped, stage1.Status)
	assert.Equal(t, StatusSkipped, stage1.Steps[0].Status)
	assert.Empty(t, stage1.Steps[0].StartedAt)
	assert.Empty(t, stage1.Steps[0].FinishedAt)
}

func TestReport_RoundTrip(t *testing.T) {
	t.Parallel()

	totals := map[devicetypes.ComponentType]int{
		devicetypes.ComponentTypeCompute:    2,
		devicetypes.ComponentTypePowerShelf: 0,
	}
	original := NewInitial(twoStageRule(), totals)
	tr := &Tracker{Report: original}
	tr.BeginStage(1, time.Unix(100, 0))
	tr.CompleteStage(1, time.Unix(150, 0))
	tr.BeginStage(2, time.Unix(160, 0))
	tr.FailStage(2, errors.New("driver crashed"), time.Unix(200, 0))

	raw, err := original.MarshalRaw()
	require.NoError(t, err)

	got, err := Unmarshal(raw)
	require.NoError(t, err)
	require.NotNil(t, got)

	assert.Equal(t, original.Version, got.Version)
	assert.Equal(t, original.Error, got.Error)
	assert.Equal(t, len(original.Stages), len(got.Stages))
	for i := range original.Stages {
		assert.Equal(t, original.Stages[i].Status, got.Stages[i].Status)
		assert.Equal(t, len(original.Stages[i].Steps), len(got.Stages[i].Steps))
	}
}
