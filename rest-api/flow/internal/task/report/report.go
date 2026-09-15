// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

// Package report defines the JSON document persisted in task.report and
// the Tracker that mutates it as a rule-based workflow advances.
//
// The document mirrors the structure of the operation rule that drives
// the workflow. NewInitial expands a RuleDefinition through
// operationrules.NewStageIterator: every emitted operationrules.Stage
// becomes one StageRecord, and every SequenceStep within that stage
// becomes one StepRecord at the same index. StageRecord.Number equals the
// rule stage number and is the canonical key for joining a record back to
// its rule entry.
package report

import (
	"encoding/json"
	"fmt"
	"time"

	"github.com/NVIDIA/infra-controller/rest-api/flow/internal/task/operationrules"
	"github.com/NVIDIA/infra-controller/rest-api/flow/pkg/common/devicetypes"
)

// Version is the report document schema version.
const Version = 1

// maxErrLen caps the length of error strings written into the report so
// a single failure cannot inflate the JSONB payload.
const maxErrLen = 512

// Status enumerates the per-stage and per-step state values. It is the
// only set of strings that appears in Status fields on the wire.
type Status string

const (
	// StatusPending - the workflow has not yet reached this stage/step.
	StatusPending Status = "pending"
	// StatusRunning - execution is in progress.
	StatusRunning Status = "running"
	// StatusCompleted - execution finished successfully.
	StatusCompleted Status = "completed"
	// StatusFailed - execution finished with an error.
	StatusFailed Status = "failed"
	// StatusSkipped - the rule lists this step's ComponentType but the
	// task targets no components of that type, so the workflow will not
	// invoke it.
	StatusSkipped Status = "skipped"
)

// Report is the JSON document stored in task.report.
type Report struct {
	Version int           `json:"version"`
	Stages  []StageRecord `json:"stages"`
	// Error is set to the failure summary of the first stage that fails
	// in this report. It is the canonical task-level error text and is
	// not overwritten by subsequent stage failures.
	Error string `json:"error,omitempty"`
}

// StageRecord captures the execution state of one rule stage (i.e. one
// operationrules.Stage produced by NewStageIterator). Number matches the
// rule stage number; Steps preserves the rule's SequenceStep order
// within the stage.
type StageRecord struct {
	Number     int          `json:"number"`
	Status     Status       `json:"status"`
	Steps      []StepRecord `json:"steps"`
	StartedAt  string       `json:"started_at,omitempty"`
	FinishedAt string       `json:"finished_at,omitempty"`
	// Error is set when Status == StatusFailed. Empty otherwise.
	Error string `json:"error,omitempty"`
}

// StepRecord captures the execution state of one rule SequenceStep. It
// pairs 1:1 with operationrules.SequenceStep within the containing stage
// and shares its index.
//
// Per-component counters describe components of ComponentType targeted
// by this step in the current task. Pending and processed counts are
// derivable on the client (Pending = TotalComponents -
// CompletedComponents - FailedComponents; processed = CompletedComponents
// + FailedComponents) and are not stored.
type StepRecord struct {
	// ComponentType mirrors operationrules.SequenceStep.ComponentType.
	ComponentType string `json:"component_type"`
	Status        Status `json:"status"`

	// TotalComponents is the count of components of ComponentType this
	// step targets. Populated once at NewInitial from
	// task.attributes.components_by_type. Carried in the report because
	// the API task representation does not surface that map; without
	// this field a client cannot size the work the step performs.
	TotalComponents int `json:"total_components,omitempty"`

	// CompletedComponents and FailedComponents are reserved on the wire
	// for a future best-effort activity contract that reports
	// per-component outcomes. The current fail-fast contract surfaces
	// only stage-level success or failure, so neither field is written
	// today and both are omitted from the JSON payload.
	CompletedComponents int `json:"completed_components,omitempty"`
	FailedComponents    int `json:"failed_components,omitempty"`

	// StartedAt is set when the step transitions out of StatusPending;
	// FinishedAt is set when it transitions to a terminal state
	// (StatusCompleted or StatusFailed). StatusSkipped steps carry
	// neither timestamp.
	StartedAt  string `json:"started_at,omitempty"`
	FinishedAt string `json:"finished_at,omitempty"`

	// Error carries the failure summary when Status == StatusFailed.
	Error string `json:"error,omitempty"`
}

// NewInitial builds a v1 report whose Stages mirror the rule definition
// stage-by-stage and step-by-step.
//
// Steps whose ComponentType has zero entries in totalByType (or is
// absent) are pre-marked StatusSkipped because the workflow will not
// invoke them. A stage whose every step is skipped is also pre-marked
// StatusSkipped; all other steps and stages start as StatusPending.
//
// Returns a Version-stamped report with empty Stages when ruleDef is nil
// or has no steps.
func NewInitial(
	ruleDef *operationrules.RuleDefinition,
	totalByType map[devicetypes.ComponentType]int,
) *Report {
	rep := &Report{Version: Version}
	if ruleDef == nil {
		return rep
	}

	iter := operationrules.NewStageIterator(ruleDef)
	rep.Stages = make([]StageRecord, 0, iter.Total())
	for stage := iter.Next(); stage != nil; stage = iter.Next() {
		stageRec := StageRecord{
			Number: stage.Number,
			Status: StatusPending,
			Steps:  make([]StepRecord, 0, len(stage.Steps)),
		}
		for _, step := range stage.Steps {
			total := totalByType[step.ComponentType]
			status := StatusPending
			if total == 0 {
				status = StatusSkipped
			}
			stageRec.Steps = append(stageRec.Steps, StepRecord{
				ComponentType:   devicetypes.ComponentTypeToString(step.ComponentType),
				Status:          status,
				TotalComponents: total,
			})
		}
		allSkipped := len(stageRec.Steps) > 0
		for _, step := range stageRec.Steps {
			if step.Status != StatusSkipped {
				allSkipped = false
				break
			}
		}
		if allSkipped {
			stageRec.Status = StatusSkipped
		}
		rep.Stages = append(rep.Stages, stageRec)
	}

	return rep
}

// MarshalJSON returns the JSON encoding for persistence. A nil receiver
// encodes as JSON null.
func (r *Report) MarshalJSON() ([]byte, error) {
	if r == nil {
		return []byte("null"), nil
	}
	if r.Version == 0 {
		r.Version = Version
	}
	type alias Report
	return json.Marshal((*alias)(r))
}

// MarshalRaw returns json.RawMessage suitable for direct insertion into
// the task.report JSONB column.
func (r *Report) MarshalRaw() (json.RawMessage, error) {
	if r == nil {
		return nil, nil
	}
	b, err := r.MarshalJSON()
	if err != nil {
		return nil, err
	}
	return json.RawMessage(b), nil
}

// Unmarshal decodes a stored report document. Empty input returns a nil
// report without error.
func Unmarshal(data json.RawMessage) (*Report, error) {
	if len(data) == 0 {
		return nil, nil
	}
	var r Report
	err := json.Unmarshal(data, &r)
	if err != nil {
		return nil, fmt.Errorf("unmarshal task report: %w", err)
	}
	return &r, nil
}

// Tracker mutates a Report through stage execution. All methods are
// workflow-safe: pure in-memory updates, no I/O, deterministic given
// identical inputs. Callers serialize the Report after each transition
// and persist the snapshot via the UpdateTaskReport activity.
type Tracker struct {
	Report *Report
}

// BeginStage transitions the stage at stageNum to StatusRunning. Each of
// the stage's StatusPending steps is advanced to StatusRunning at the
// same time; StatusSkipped steps are left untouched. Calls on an unknown
// stageNum or a stage already past StatusPending are a no-op so
// double-fired transitions stay idempotent.
func (t *Tracker) BeginStage(stageNum int, now time.Time) {
	if t == nil || t.Report == nil {
		return
	}
	stage := t.findStage(stageNum)
	if stage == nil || stage.Status != StatusPending {
		return
	}

	started := formatTime(now)
	stage.Status = StatusRunning
	stage.StartedAt = started
	for i := range stage.Steps {
		if stage.Steps[i].Status == StatusPending {
			stage.Steps[i].Status = StatusRunning
			stage.Steps[i].StartedAt = started
		}
	}
}

// CompleteStage transitions the stage at stageNum to StatusCompleted.
// Every step still in StatusRunning is finalized to StatusCompleted with
// the same FinishedAt; under the fail-fast activity contract a completed
// stage implies all its non-skipped steps succeeded. StatusSkipped steps
// and steps already in a terminal state are left untouched.
func (t *Tracker) CompleteStage(stageNum int, now time.Time) {
	if t == nil || t.Report == nil {
		return
	}
	stage := t.findStage(stageNum)
	if stage == nil || stage.Status == StatusSkipped {
		return
	}

	finished := formatTime(now)
	stage.Status = StatusCompleted
	stage.FinishedAt = finished
	for i := range stage.Steps {
		if stage.Steps[i].Status == StatusRunning {
			stage.Steps[i].Status = StatusCompleted
			stage.Steps[i].FinishedAt = finished
		}
	}
}

// FailStage transitions the stage at stageNum to StatusFailed and
// propagates the failure to every step still in StatusRunning. The
// fail-fast activity contract aborts the stage on the first failing
// child and discards any sibling outcomes, so all running steps are
// marked StatusFailed with the same Error: their actual results are
// unobserved and reporting any of them as completed would be a guess.
//
// Report.Error is set to the failure summary the first time a stage
// fails in this report; subsequent FailStage calls leave it unchanged so
// the first failure remains the canonical task-level error.
func (t *Tracker) FailStage(stageNum int, stageErr error, now time.Time) {
	if t == nil || t.Report == nil {
		return
	}
	stage := t.findStage(stageNum)
	if stage == nil {
		return
	}

	msg := truncateErr(stageErr)
	finished := formatTime(now)
	stage.Status = StatusFailed
	stage.FinishedAt = finished
	stage.Error = msg
	for i := range stage.Steps {
		if stage.Steps[i].Status == StatusRunning {
			stage.Steps[i].Status = StatusFailed
			stage.Steps[i].FinishedAt = finished
			stage.Steps[i].Error = msg
		}
	}
	if t.Report.Error == "" {
		t.Report.Error = msg
	}
}

func (t *Tracker) findStage(stageNum int) *StageRecord {
	for i := range t.Report.Stages {
		if t.Report.Stages[i].Number == stageNum {
			return &t.Report.Stages[i]
		}
	}
	return nil
}

func formatTime(now time.Time) string {
	return now.UTC().Format(time.RFC3339)
}

func truncateErr(err error) string {
	if err == nil {
		return ""
	}
	msg := err.Error()
	if len(msg) > maxErrLen {
		msg = msg[:maxErrLen]
	}
	return msg
}
