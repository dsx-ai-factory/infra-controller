// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package model

import (
	"encoding/json"
	"fmt"
	"net/url"
	"strconv"
	"time"

	"github.com/NVIDIA/infra-controller/rest-api/api/pkg/api/pagination"
	flowv1 "github.com/NVIDIA/infra-controller/rest-api/proto/flow/gen/v1"
)

var ProtoToAPITaskStatusName = map[flowv1.TaskStatus]string{
	flowv1.TaskStatus_TASK_STATUS_UNKNOWN:    "Unknown",
	flowv1.TaskStatus_TASK_STATUS_PENDING:    "Pending",
	flowv1.TaskStatus_TASK_STATUS_RUNNING:    "Running",
	flowv1.TaskStatus_TASK_STATUS_COMPLETED:  "Succeeded",
	flowv1.TaskStatus_TASK_STATUS_FAILED:     "Failed",
	flowv1.TaskStatus_TASK_STATUS_TERMINATED: "Terminated",
	flowv1.TaskStatus_TASK_STATUS_WAITING:    "Pending",
}

// APITaskStats counts non-terminal tasks currently associated with an
// inventory resource. Pending combines Flow's Waiting and Pending states,
// while Active represents Running. Every count is serialized, including zero.
type APITaskStats struct {
	PendingTaskCount uint32 `json:"pendingTaskCount"`
	ActiveTaskCount  uint32 `json:"activeTaskCount"`
}

// FromProto replaces the task statistics with the values reported by Flow. Nil
// stats clear all counts so the REST response shape remains stable.
func (s *APITaskStats) FromProto(stats *flowv1.TaskStats) {
	*s = APITaskStats{}
	if stats == nil {
		return
	}
	s.PendingTaskCount = stats.GetWaitingTaskCount() + stats.GetPendingTaskCount()
	s.ActiveTaskCount = stats.GetRunningTaskCount()
}

// APITask is the API response model for a Flow-scheduled task
// (OpenAPI schema Task). It covers both rack- and tray-scoped tasks
// because Flow drives them through the same Task entity.
type APITask struct {
	ID          string `json:"id"`
	Status      string `json:"status"`
	Description string `json:"description"`
	Message     string `json:"message"`
	// RuleID is the Operation Rule Flow resolved for this task; nil until
	// Flow records a resolution. Sourced from Task.applied_rule_id.
	RuleID   *string          `json:"ruleId"`
	Started  *time.Time       `json:"started"`
	Finished *time.Time       `json:"finished"`
	Created  time.Time        `json:"created"`
	Updated  time.Time        `json:"updated"`
	Report   *APITaskReportV1 `json:"report,omitempty"`
}

// APITaskReportV1Status enumerates per-stage and per-step execution
// states surfaced in a v1 task report. Values are lowercase to match
// Flow's wire format.
type APITaskReportV1Status string

const (
	APITaskReportV1StatusPending   APITaskReportV1Status = "pending"
	APITaskReportV1StatusRunning   APITaskReportV1Status = "running"
	APITaskReportV1StatusCompleted APITaskReportV1Status = "completed"
	APITaskReportV1StatusFailed    APITaskReportV1Status = "failed"
	APITaskReportV1StatusSkipped   APITaskReportV1Status = "skipped"
)

// APITaskReportV1 is the typed v1 task execution report exposed on
// APITask.Report. The Version field always equals 1 within this
// type; a future schema iteration ships as APITaskReportV2 alongside a
// parallel response field, leaving v1 consumers untouched.
type APITaskReportV1 struct {
	Version int                    `json:"version"`
	Stages  []APITaskReportV1Stage `json:"stages"`
	// Error is the top-level failure summary: the message from the
	// first stage that fails in this report. Not overwritten by later
	// failures so a single canonical task-level error survives.
	Error string `json:"error,omitempty"`
}

// APITaskReportV1Stage captures the execution state of one rule stage.
// Number is the canonical key for joining a stage record back to its
// rule entry.
type APITaskReportV1Stage struct {
	Number     int                   `json:"number"`
	Status     APITaskReportV1Status `json:"status"`
	Steps      []APITaskReportV1Step `json:"steps"`
	StartedAt  string                `json:"startedAt,omitempty"`
	FinishedAt string                `json:"finishedAt,omitempty"`
	Error      string                `json:"error,omitempty"`
}

// APITaskReportV1Step captures the execution state of one rule
// SequenceStep. Pairs 1:1 with the rule's ordered steps within the
// containing stage and shares its index.
type APITaskReportV1Step struct {
	// ComponentType identifies which component class this step targets,
	// e.g. "Compute", "NVLSwitch", "PowerShelf".
	ComponentType string                `json:"componentType"`
	Status        APITaskReportV1Status `json:"status"`
	// TotalComponents is the count of components of ComponentType this
	// step targets. Carried in the report because the API task
	// representation does not surface the per-type component map.
	TotalComponents int `json:"totalComponents,omitempty"`
	// CompletedComponents and FailedComponents are reserved for a
	// future best-effort activity contract that reports per-component
	// outcomes. The current fail-fast contract surfaces only
	// stage-level success or failure; both fields are omitted today.
	CompletedComponents int    `json:"completedComponents,omitempty"`
	FailedComponents    int    `json:"failedComponents,omitempty"`
	StartedAt           string `json:"startedAt,omitempty"`
	FinishedAt          string `json:"finishedAt,omitempty"`
	// Error carries the failure summary when Status == failed.
	// Truncated to 512 bytes on the Flow side.
	Error string `json:"error,omitempty"`
}

// flowTaskReportV1 mirrors the wire shape Flow writes into Task.report
// (snake_case JSON keys). Used solely as an unmarshal target so the
// REST-facing APITaskReportV1 can keep camelCase keys per the rest of
// the API surface. Kept private; clients consume APITaskReportV1.
type flowTaskReportV1 struct {
	Version int                     `json:"version"`
	Stages  []flowTaskReportV1Stage `json:"stages"`
	Error   string                  `json:"error,omitempty"`
}

type flowTaskReportV1Stage struct {
	Number     int                    `json:"number"`
	Status     APITaskReportV1Status  `json:"status"`
	Steps      []flowTaskReportV1Step `json:"steps"`
	StartedAt  string                 `json:"started_at,omitempty"`
	FinishedAt string                 `json:"finished_at,omitempty"`
	Error      string                 `json:"error,omitempty"`
}

type flowTaskReportV1Step struct {
	ComponentType       string                `json:"component_type"`
	Status              APITaskReportV1Status `json:"status"`
	TotalComponents     int                   `json:"total_components,omitempty"`
	CompletedComponents int                   `json:"completed_components,omitempty"`
	FailedComponents    int                   `json:"failed_components,omitempty"`
	StartedAt           string                `json:"started_at,omitempty"`
	FinishedAt          string                `json:"finished_at,omitempty"`
	Error               string                `json:"error,omitempty"`
}

// UnmarshalJSON decodes Flow's snake_case wire format for Task.report
// into the REST-facing APITaskReportV1. The marshal path is asymmetric:
// json.Marshal walks the struct tags and emits camelCase per the REST
// surface, while UnmarshalJSON intentionally only accepts Flow's
// snake_case input. Round-tripping a marshalled APITaskReportV1 through
// json.Unmarshal will therefore drop most fields; callers that need a
// round-trippable form should decode into a map. Returns an error for
// any payload that fails to parse or carries a version other than 1, so
// FromProto can drop the field rather than surface an off-contract shape.
func (r *APITaskReportV1) UnmarshalJSON(data []byte) error {
	var src flowTaskReportV1
	if err := json.Unmarshal(data, &src); err != nil {
		return err
	}
	if src.Version != 1 {
		return fmt.Errorf("unsupported task report version %d", src.Version)
	}
	r.Version = src.Version
	r.Error = src.Error
	r.Stages = make([]APITaskReportV1Stage, 0, len(src.Stages))
	for _, s := range src.Stages {
		dstStage := APITaskReportV1Stage{
			Number:     s.Number,
			Status:     s.Status,
			StartedAt:  s.StartedAt,
			FinishedAt: s.FinishedAt,
			Error:      s.Error,
			Steps:      make([]APITaskReportV1Step, 0, len(s.Steps)),
		}
		for _, p := range s.Steps {
			dstStage.Steps = append(dstStage.Steps, APITaskReportV1Step{
				ComponentType:       p.ComponentType,
				Status:              p.Status,
				TotalComponents:     p.TotalComponents,
				CompletedComponents: p.CompletedComponents,
				FailedComponents:    p.FailedComponents,
				StartedAt:           p.StartedAt,
				FinishedAt:          p.FinishedAt,
				Error:               p.Error,
			})
		}
		r.Stages = append(r.Stages, dstStage)
	}
	return nil
}

// APITaskOption configures optional fields populated on an APITask.
// Used by NewAPITask so list endpoints can omit large optional payloads
// (Report in particular) by default while single-task endpoints opt in.
type APITaskOption func(*apiTaskOptions)

type apiTaskOptions struct {
	withReport bool
}

// WithTaskReport populates APITask.Report by decoding Task.report as
// APITaskReportV1. Without it Report stays nil and is omitted from the
// JSON response; a malformed or non-v1 payload also yields nil so the
// response never carries an off-contract shape.
func WithTaskReport() APITaskOption {
	return func(o *apiTaskOptions) { o.withReport = true }
}

func (t *APITask) FromProto(task *flowv1.Task, opts ...APITaskOption) {
	if task == nil {
		return
	}
	o := apiTaskOptions{}
	for _, opt := range opts {
		opt(&o)
	}
	if task.GetId() != nil {
		t.ID = task.GetId().GetId()
	}
	t.Status = enumOr(ProtoToAPITaskStatusName, task.GetStatus(), "Unknown")
	t.Description = task.GetDescription()
	t.Message = task.GetMessage()
	if ts := task.GetStartedAt(); ts != nil {
		v := ts.AsTime().UTC()
		t.Started = &v
	}
	if ts := task.GetFinishedAt(); ts != nil {
		v := ts.AsTime().UTC()
		t.Finished = &v
	}
	t.Created = task.GetCreatedAt().AsTime().UTC()
	t.Updated = task.GetUpdatedAt().AsTime().UTC()
	if id := task.GetAppliedRuleId(); id != nil && id.GetId() != "" {
		v := id.GetId()
		t.RuleID = &v
	}
	if o.withReport {
		if raw := task.GetReport(); raw != "" {
			var r APITaskReportV1
			if err := json.Unmarshal([]byte(raw), &r); err == nil {
				t.Report = &r
			}
		}
	}
}

func NewAPITask(task *flowv1.Task, opts ...APITaskOption) *APITask {
	t := &APITask{}
	t.FromProto(task, opts...)
	return t
}

// APIGetTaskRequest captures query parameters for getting a task by ID.
type APIGetTaskRequest struct {
	SiteID string `query:"siteId"`
}

func (r *APIGetTaskRequest) Validate() error {
	if r.SiteID == "" {
		return fmt.Errorf("siteId query parameter is required")
	}
	return nil
}

// APICancelTaskRequest is the request body for cancelling a task by ID.
type APICancelTaskRequest struct {
	SiteID string `json:"siteId"`
}

// Validate validates the cancel task request
func (r *APICancelTaskRequest) Validate() error {
	if r.SiteID == "" {
		return fmt.Errorf("siteId is required")
	}
	return nil
}

// APIGetTasksRequest binds query parameters for Site-, Rack-, and Tray-scoped
// task list endpoints. Pagination is bound separately via pagination.PageRequest.
type APIGetTasksRequest struct {
	SiteID        string `query:"siteId"`
	ActiveOnly    bool   `query:"activeOnly"`
	IncludeReport bool   `query:"includeReport"`
}

func (r *APIGetTasksRequest) Validate() error {
	if r.SiteID == "" {
		return fmt.Errorf("siteId query parameter is required")
	}
	return nil
}

// TaskOptions returns the APITaskOption slice the list-task handlers
// pass to NewAPITask, translating the opt-in toggles on this request
// (IncludeReport today, more later) into the corresponding option
// constructors. Centralized here so new opt-in fields wire up in one
// place rather than in each list handler.
func (r *APIGetTasksRequest) TaskOptions() []APITaskOption {
	var opts []APITaskOption
	if r.IncludeReport {
		opts = append(opts, WithTaskReport())
	}
	return opts
}

// QueryValues returns query parameters that participate in deterministic
// workflow ID hashing, including pagination fields so concurrent requests
// for different pages do not reuse the same workflow execution.
func (r *APIGetTasksRequest) QueryValues(page pagination.PageRequest) url.Values {
	v := url.Values{}
	v.Set("siteId", r.SiteID)
	if r.ActiveOnly {
		v.Set("activeOnly", strconv.FormatBool(r.ActiveOnly))
	}
	if r.IncludeReport {
		v.Set("includeReport", strconv.FormatBool(r.IncludeReport))
	}
	if page.PageNumber != nil && *page.PageNumber != 0 {
		v.Set("pageNumber", strconv.Itoa(*page.PageNumber))
	}
	if page.PageSize != nil && *page.PageSize != 0 {
		v.Set("pageSize", strconv.Itoa(*page.PageSize))
	}
	return v
}
