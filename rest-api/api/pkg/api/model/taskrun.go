// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package model

import (
	"errors"
	"fmt"
	"net/url"
	"strconv"
	"time"

	validation "github.com/go-ozzo/ozzo-validation/v4"
	"google.golang.org/protobuf/types/known/durationpb"

	"github.com/NVIDIA/infra-controller/rest-api/api/pkg/api/pagination"
	flowv1 "github.com/NVIDIA/infra-controller/rest-api/proto/flow/gen/v1"
)

// A Run is the REST representation of Flow's operation run: a phased,
// policy-gated execution of one operation across many racks. Each run
// materializes a set of per-rack targets (APITaskRunTarget), and each target
// drives at most one Task. Callers drill into execution detail via the Task
// endpoints using APITaskRunTarget.TaskID.

// ~~~~~ Status enums ~~~~~ //

// TaskRunStatus enumerates the lifecycle states of a task run.
type TaskRunStatus string

const (
	TaskRunStatusUnknown               TaskRunStatus = "Unknown"
	TaskRunStatusPending               TaskRunStatus = "Pending"
	TaskRunStatusRunning               TaskRunStatus = "Running"
	TaskRunStatusPaused                TaskRunStatus = "Paused"
	TaskRunStatusCompleted             TaskRunStatus = "Completed"
	TaskRunStatusCancelled             TaskRunStatus = "Cancelled"
	TaskRunStatusFailed                TaskRunStatus = "Failed"
	TaskRunStatusCompletedWithFailures TaskRunStatus = "CompletedWithFailures"
)

var taskRunStatusChoiceMap = map[TaskRunStatus]flowv1.OperationRunStatus{
	TaskRunStatusUnknown:               flowv1.OperationRunStatus_OPERATION_RUN_STATUS_UNKNOWN,
	TaskRunStatusPending:               flowv1.OperationRunStatus_OPERATION_RUN_STATUS_PENDING,
	TaskRunStatusRunning:               flowv1.OperationRunStatus_OPERATION_RUN_STATUS_RUNNING,
	TaskRunStatusPaused:                flowv1.OperationRunStatus_OPERATION_RUN_STATUS_PAUSED,
	TaskRunStatusCompleted:             flowv1.OperationRunStatus_OPERATION_RUN_STATUS_COMPLETED,
	TaskRunStatusCancelled:             flowv1.OperationRunStatus_OPERATION_RUN_STATUS_CANCELLED,
	TaskRunStatusFailed:                flowv1.OperationRunStatus_OPERATION_RUN_STATUS_FAILED,
	TaskRunStatusCompletedWithFailures: flowv1.OperationRunStatus_OPERATION_RUN_STATUS_COMPLETED_WITH_FAILURES,
}

// validTaskRunStatuses lists the run statuses accepted as a list filter.
// Unknown is intentionally excluded (it is not a filterable state).
var validTaskRunStatuses = []TaskRunStatus{
	TaskRunStatusPending,
	TaskRunStatusRunning,
	TaskRunStatusPaused,
	TaskRunStatusCompleted,
	TaskRunStatusCancelled,
	TaskRunStatusFailed,
	TaskRunStatusCompletedWithFailures,
}

var validTaskRunStatusesAny = func() []any {
	out := make([]any, len(validTaskRunStatuses))
	for i, s := range validTaskRunStatuses {
		out[i] = s
	}
	return out
}()

// ToProto converts the REST run status to its Flow enum value.
func (trs TaskRunStatus) ToProto() flowv1.OperationRunStatus {
	if v, ok := taskRunStatusChoiceMap[trs]; ok {
		return v
	}
	return flowv1.OperationRunStatus_OPERATION_RUN_STATUS_UNKNOWN
}

// FromProto populates the REST run status from a Flow enum value.
func (trs *TaskRunStatus) FromProto(p flowv1.OperationRunStatus) {
	if trs == nil {
		return
	}
	for rest, proto := range taskRunStatusChoiceMap {
		if proto == p {
			*trs = rest
			return
		}
	}
	*trs = TaskRunStatusUnknown
}

// TaskRunStatusReason explains why a run is paused or terminal. It is
// response-only, so it converts from Flow but never back.
type TaskRunStatusReason string

const (
	TaskRunStatusReasonUnknown              TaskRunStatusReason = "Unknown"
	TaskRunStatusReasonNone                 TaskRunStatusReason = "None"
	TaskRunStatusReasonOperatorPaused       TaskRunStatusReason = "OperatorPaused"
	TaskRunStatusReasonPhaseGate            TaskRunStatusReason = "PhaseGate"
	TaskRunStatusReasonSafetyGate           TaskRunStatusReason = "SafetyGate"
	TaskRunStatusReasonConflictRetryTimeout TaskRunStatusReason = "ConflictRetryTimeout"
)

var taskRunStatusReasonChoiceMap = map[TaskRunStatusReason]flowv1.OperationRunStatusReason{
	TaskRunStatusReasonUnknown:              flowv1.OperationRunStatusReason_OPERATION_RUN_STATUS_REASON_UNKNOWN,
	TaskRunStatusReasonNone:                 flowv1.OperationRunStatusReason_OPERATION_RUN_STATUS_REASON_NONE,
	TaskRunStatusReasonOperatorPaused:       flowv1.OperationRunStatusReason_OPERATION_RUN_STATUS_REASON_OPERATOR_PAUSED,
	TaskRunStatusReasonPhaseGate:            flowv1.OperationRunStatusReason_OPERATION_RUN_STATUS_REASON_PHASE_GATE,
	TaskRunStatusReasonSafetyGate:           flowv1.OperationRunStatusReason_OPERATION_RUN_STATUS_REASON_SAFETY_GATE,
	TaskRunStatusReasonConflictRetryTimeout: flowv1.OperationRunStatusReason_OPERATION_RUN_STATUS_REASON_CONFLICT_RETRY_TIMEOUT,
}

// FromProto populates the REST status reason from a Flow enum value.
func (trsr *TaskRunStatusReason) FromProto(p flowv1.OperationRunStatusReason) {
	if trsr == nil {
		return
	}
	for rest, proto := range taskRunStatusReasonChoiceMap {
		if proto == p {
			*trsr = rest
			return
		}
	}
	*trsr = TaskRunStatusReasonUnknown
}

// TaskRunTargetStatus enumerates per-rack target execution states.
type TaskRunTargetStatus string

const (
	TaskRunTargetStatusUnknown    TaskRunTargetStatus = "Unknown"
	TaskRunTargetStatusPending    TaskRunTargetStatus = "Pending"
	TaskRunTargetStatusBlocked    TaskRunTargetStatus = "Blocked"
	TaskRunTargetStatusSubmitted  TaskRunTargetStatus = "Submitted"
	TaskRunTargetStatusCompleted  TaskRunTargetStatus = "Completed"
	TaskRunTargetStatusFailed     TaskRunTargetStatus = "Failed"
	TaskRunTargetStatusTerminated TaskRunTargetStatus = "Terminated"
	TaskRunTargetStatusSkipped    TaskRunTargetStatus = "Skipped"
	TaskRunTargetStatusClaimed    TaskRunTargetStatus = "Claimed"
)

var taskRunTargetStatusChoiceMap = map[TaskRunTargetStatus]flowv1.OperationRunTargetStatus{
	TaskRunTargetStatusUnknown:    flowv1.OperationRunTargetStatus_OPERATION_RUN_TARGET_STATUS_UNKNOWN,
	TaskRunTargetStatusPending:    flowv1.OperationRunTargetStatus_OPERATION_RUN_TARGET_STATUS_PENDING,
	TaskRunTargetStatusBlocked:    flowv1.OperationRunTargetStatus_OPERATION_RUN_TARGET_STATUS_BLOCKED,
	TaskRunTargetStatusSubmitted:  flowv1.OperationRunTargetStatus_OPERATION_RUN_TARGET_STATUS_SUBMITTED,
	TaskRunTargetStatusCompleted:  flowv1.OperationRunTargetStatus_OPERATION_RUN_TARGET_STATUS_COMPLETED,
	TaskRunTargetStatusFailed:     flowv1.OperationRunTargetStatus_OPERATION_RUN_TARGET_STATUS_FAILED,
	TaskRunTargetStatusTerminated: flowv1.OperationRunTargetStatus_OPERATION_RUN_TARGET_STATUS_TERMINATED,
	TaskRunTargetStatusSkipped:    flowv1.OperationRunTargetStatus_OPERATION_RUN_TARGET_STATUS_SKIPPED,
	TaskRunTargetStatusClaimed:    flowv1.OperationRunTargetStatus_OPERATION_RUN_TARGET_STATUS_CLAIMED,
}

var validTaskRunTargetStatuses = []TaskRunTargetStatus{
	TaskRunTargetStatusPending,
	TaskRunTargetStatusBlocked,
	TaskRunTargetStatusSubmitted,
	TaskRunTargetStatusCompleted,
	TaskRunTargetStatusFailed,
	TaskRunTargetStatusTerminated,
	TaskRunTargetStatusSkipped,
	TaskRunTargetStatusClaimed,
}

var validTaskRunTargetStatusesAny = func() []any {
	out := make([]any, len(validTaskRunTargetStatuses))
	for i, s := range validTaskRunTargetStatuses {
		out[i] = s
	}
	return out
}()

// ToProto converts the REST target status to its Flow enum value.
func (trts TaskRunTargetStatus) ToProto() flowv1.OperationRunTargetStatus {
	if v, ok := taskRunTargetStatusChoiceMap[trts]; ok {
		return v
	}
	return flowv1.OperationRunTargetStatus_OPERATION_RUN_TARGET_STATUS_UNKNOWN
}

// FromProto populates the REST target status from a Flow enum value.
func (trts *TaskRunTargetStatus) FromProto(p flowv1.OperationRunTargetStatus) {
	if trts == nil {
		return
	}
	for rest, proto := range taskRunTargetStatusChoiceMap {
		if proto == p {
			*trts = rest
			return
		}
	}
	*trts = TaskRunTargetStatusUnknown
}

// ~~~~~ Response model ~~~~~ //

// APITaskRun is the API response model for a Flow operation run. List
// responses populate the summary fields only; get responses additionally
// populate Stats when the caller requests derived stats.
type APITaskRun struct {
	ID            string           `json:"id"`
	Name          string           `json:"name"`
	Description   string           `json:"description"`
	OperationType APIOperationType `json:"operationType"`
	OperationCode string           `json:"operationCode"`
	Status        TaskRunStatus    `json:"status"`
	// StatusReason explains why a run is paused or terminal (e.g. PhaseGate,
	// SafetyGate). "None" when there is no qualifying reason.
	StatusReason  TaskRunStatusReason `json:"statusReason"`
	StatusMessage string              `json:"statusMessage"`
	TotalPhases   int32               `json:"totalPhases"`
	Created       time.Time           `json:"created"`
	Updated       time.Time           `json:"updated"`
	Started       *time.Time          `json:"started"`
	Finished      *time.Time          `json:"finished"`
	// Stats is present only when the caller requests derived stats on a get.
	Stats *APITaskRunStats `json:"stats,omitempty"`
}

// FromProtoSummary populates the summary fields shared by list and get
// responses from an OperationRunSummary.
func (atr *APITaskRun) FromProtoSummary(s *flowv1.OperationRunSummary) {
	if s == nil {
		return
	}
	if s.GetId() != nil {
		atr.ID = s.GetId().GetId()
	}
	atr.Name = s.GetName()
	atr.Description = s.GetDescription()
	if k := s.GetOperationKind(); k != nil {
		atr.OperationType = enumOr(protoToAPIOperationType, k.GetType(), "")
		atr.OperationCode = k.GetCode()
	}
	if st := s.GetState(); st != nil {
		atr.Status.FromProto(st.GetStatus())
		atr.StatusReason.FromProto(st.GetReason())
	}
	atr.StatusMessage = s.GetStatusMessage()
	atr.TotalPhases = s.GetTotalPhases()
	if ts := s.GetCreatedAt(); ts != nil {
		atr.Created = ts.AsTime().UTC()
	}
	if ts := s.GetUpdatedAt(); ts != nil {
		atr.Updated = ts.AsTime().UTC()
	}
	if ts := s.GetStartedAt(); ts != nil {
		v := ts.AsTime().UTC()
		atr.Started = &v
	}
	if ts := s.GetFinishedAt(); ts != nil {
		v := ts.AsTime().UTC()
		atr.Finished = &v
	}
}

// FromProto populates an APITaskRun from a full OperationRun, including
// derived stats when Flow computed them.
func (atr *APITaskRun) FromProto(run *flowv1.OperationRun) {
	if run == nil {
		return
	}
	atr.FromProtoSummary(run.GetSummary())
	if stats := run.GetStats(); stats != nil {
		atr.Stats = &APITaskRunStats{}
		atr.Stats.FromProto(stats)
	}
}

// APITaskRunStats summarizes target outcomes for the active phase and for all
// phases processed so far.
type APITaskRunStats struct {
	CurrentPhase    APITaskRunPhaseStats `json:"currentPhase"`
	CumulativePhase APITaskRunPhaseStats `json:"cumulativePhase"`
}

// FromProto populates the run stats from Flow's OperationRunStats.
func (atrs *APITaskRunStats) FromProto(s *flowv1.OperationRunStats) {
	if s == nil {
		return
	}
	atrs.CurrentPhase.FromProto(s.GetCurrentPhaseStats())
	atrs.CumulativePhase.FromProto(s.GetCumulativePhaseStats())
}

// APITaskRunPhaseStats summarizes target outcomes for one phase scope.
type APITaskRunPhaseStats struct {
	PhaseIndex      int32                   `json:"phaseIndex"`
	SelectedTargets int32                   `json:"selectedTargets"`
	OutcomeCounts   APITaskRunOutcomeCounts `json:"outcomeCounts"`
}

// FromProto populates the phase stats from Flow's OperationRunPhaseStats.
func (atrps *APITaskRunPhaseStats) FromProto(p *flowv1.OperationRunPhaseStats) {
	if p == nil {
		return
	}
	atrps.PhaseIndex = p.GetPhaseIndex()
	atrps.SelectedTargets = p.GetSelectedTargets()
	atrps.OutcomeCounts.FromProto(p.GetOutcomeCounts())
}

// APITaskRunOutcomeCounts counts terminal target outcomes within a phase scope.
type APITaskRunOutcomeCounts struct {
	Completed  int32 `json:"completed"`
	Failed     int32 `json:"failed"`
	Terminated int32 `json:"terminated"`
	Skipped    int32 `json:"skipped"`
}

// FromProto populates the outcome counts from Flow's target outcome counts.
func (atroc *APITaskRunOutcomeCounts) FromProto(c *flowv1.OperationRunTargetOutcomeCounts) {
	if c == nil {
		return
	}
	atroc.Completed = c.GetCompleted()
	atroc.Failed = c.GetFailed()
	atroc.Terminated = c.GetTerminated()
	atroc.Skipped = c.GetSkipped()
}

// ~~~~~ Target response model ~~~~~ //

// APITaskRunTarget is the API response model for one materialized rack
// execution target of a run. TaskID references the Task the run
// submitted for this rack (nil until submission); clients drill into execution
// detail via GET /task/{taskId}.
type APITaskRunTarget struct {
	ID            string              `json:"id"`
	RunID         string              `json:"runId"`
	RackID        string              `json:"rackId"`
	SequenceIndex int32               `json:"sequenceIndex"`
	PhaseIndex    int32               `json:"phaseIndex"`
	TaskID        *string             `json:"taskId"`
	Status        TaskRunTargetStatus `json:"status"`
	Message       string              `json:"message"`
	Created       time.Time           `json:"created"`
	Updated       time.Time           `json:"updated"`
}

// FromProto populates an APITaskRunTarget from an OperationRunTarget.
func (atrt *APITaskRunTarget) FromProto(p *flowv1.OperationRunTarget) {
	if p == nil {
		return
	}
	if p.GetId() != nil {
		atrt.ID = p.GetId().GetId()
	}
	if p.GetOperationRunId() != nil {
		atrt.RunID = p.GetOperationRunId().GetId()
	}
	atrt.RackID = p.GetRackExternalId()
	atrt.SequenceIndex = p.GetSequenceIndex()
	atrt.PhaseIndex = p.GetPhaseIndex()
	if id := p.GetTaskId(); id != nil && id.GetId() != "" {
		v := id.GetId()
		atrt.TaskID = &v
	}
	atrt.Status.FromProto(p.GetStatus())
	atrt.Message = p.GetMessage()
	if ts := p.GetCreatedAt(); ts != nil {
		atrt.Created = ts.AsTime().UTC()
	}
	if ts := p.GetUpdatedAt(); ts != nil {
		atrt.Updated = ts.AsTime().UTC()
	}
}

// ~~~~~ Get (siteId + includeStats via query) ~~~~~ //

// APITaskRunGetRequest captures query parameters for GET /task/run/{id}.
type APITaskRunGetRequest struct {
	SiteID       string `query:"siteId"`
	IncludeStats bool   `query:"includeStats"`
}

func (atrgr *APITaskRunGetRequest) Validate() error {
	return validation.ValidateStruct(atrgr,
		validation.Field(&atrgr.SiteID, validation.Required.Error("siteId query parameter is required")),
	)
}

// ~~~~~ List ~~~~~ //

// APITaskRunGetAllRequest binds query parameters for GET /task/run. Pagination
// is bound separately via pagination.PageRequest.
type APITaskRunGetAllRequest struct {
	SiteID        string           `query:"siteId"`
	Status        TaskRunStatus    `query:"status"`
	OperationType APIOperationType `query:"operationType"`
}

func (atrgar *APITaskRunGetAllRequest) Validate() error {
	return validation.ValidateStruct(atrgar,
		validation.Field(&atrgar.SiteID, validation.Required.Error("siteId query parameter is required")),
		validation.Field(&atrgar.OperationType,
			validation.When(atrgar.OperationType != "",
				validation.In(validOperationTypesAny...).Error(
					fmt.Sprintf("operationType must be one of %v", validOperationTypes)))),
		validation.Field(&atrgar.Status,
			validation.When(atrgar.Status != "",
				validation.In(validTaskRunStatusesAny...).Error(
					fmt.Sprintf("status must be one of %v", validTaskRunStatuses)))),
	)
}

// ToProto converts the list filters into the Flow ListOperationRunsRequest.
func (atrgar *APITaskRunGetAllRequest) ToProto(page pagination.PageRequest) (*flowv1.ListOperationRunsRequest, error) {
	req := &flowv1.ListOperationRunsRequest{}
	filter := &flowv1.OperationRunFilter{}
	hasFilter := false

	if atrgar.Status != "" {
		status := atrgar.Status.ToProto()
		filter.States = []*flowv1.OperationRunStateFilter{{Status: &status}}
		hasFilter = true
	}
	if atrgar.OperationType != "" {
		opType, err := atrgar.OperationType.ToProto()
		if err != nil {
			return nil, err
		}
		filter.OperationKinds = []*flowv1.OperationKind{{Type: opType}}
		hasFilter = true
	}
	if hasFilter {
		req.Filter = filter
	}

	if page.Offset != nil && page.Limit != nil {
		req.Pagination = &flowv1.Pagination{
			Offset: int32(*page.Offset),
			Limit:  int32(*page.Limit),
		}
	}
	return req, nil
}

// QueryValues returns the request fields that feed the workflow ID hash,
// including pagination so different pages map to distinct workflow IDs.
func (atrgar *APITaskRunGetAllRequest) QueryValues(page pagination.PageRequest) url.Values {
	v := url.Values{}
	v.Set("siteId", atrgar.SiteID)
	if atrgar.Status != "" {
		v.Set("status", string(atrgar.Status))
	}
	if atrgar.OperationType != "" {
		v.Set("operationType", string(atrgar.OperationType))
	}
	if page.PageNumber != nil && *page.PageNumber != 0 {
		v.Set("pageNumber", strconv.Itoa(*page.PageNumber))
	}
	if page.PageSize != nil && *page.PageSize != 0 {
		v.Set("pageSize", strconv.Itoa(*page.PageSize))
	}
	return v
}

// ~~~~~ List targets ~~~~~ //

// APITaskRunTargetGetAllRequest binds query parameters for
// GET /task/run/{id}/target.
type APITaskRunTargetGetAllRequest struct {
	SiteID     string              `query:"siteId"`
	Status     TaskRunTargetStatus `query:"status"`
	PhaseScope string              `query:"phaseScope"`
}

var validTaskRunPhaseScopes = []string{"currentPhase", "completedPhases", "currentAndCompletedPhases"}

var validTaskRunPhaseScopesAny = func() []any {
	out := make([]any, len(validTaskRunPhaseScopes))
	for i, s := range validTaskRunPhaseScopes {
		out[i] = s
	}
	return out
}()

var apiToProtoRunPhaseScope = map[string]flowv1.OperationRunTargetPhaseScope{
	"":                          flowv1.OperationRunTargetPhaseScope_OPERATION_RUN_TARGET_PHASE_SCOPE_CURRENT_PHASE,
	"currentPhase":              flowv1.OperationRunTargetPhaseScope_OPERATION_RUN_TARGET_PHASE_SCOPE_CURRENT_PHASE,
	"completedPhases":           flowv1.OperationRunTargetPhaseScope_OPERATION_RUN_TARGET_PHASE_SCOPE_COMPLETED_PHASES,
	"currentAndCompletedPhases": flowv1.OperationRunTargetPhaseScope_OPERATION_RUN_TARGET_PHASE_SCOPE_CURRENT_AND_COMPLETED_PHASES,
}

func (atrtgar *APITaskRunTargetGetAllRequest) Validate() error {
	return validation.ValidateStruct(atrtgar,
		validation.Field(&atrtgar.SiteID, validation.Required.Error("siteId query parameter is required")),
		validation.Field(&atrtgar.Status,
			validation.When(atrtgar.Status != "",
				validation.In(validTaskRunTargetStatusesAny...).Error(
					fmt.Sprintf("status must be one of %v", validTaskRunTargetStatuses)))),
		validation.Field(&atrtgar.PhaseScope,
			validation.When(atrtgar.PhaseScope != "",
				validation.In(validTaskRunPhaseScopesAny...).Error(
					fmt.Sprintf("phaseScope must be one of %v", validTaskRunPhaseScopes)))),
	)
}

// ToProto converts the target-list filters into the Flow
// ListOperationRunTargetsRequest. status UNKNOWN means no status filter.
func (atrtgar *APITaskRunTargetGetAllRequest) ToProto(runID string, page pagination.PageRequest) *flowv1.ListOperationRunTargetsRequest {
	req := &flowv1.ListOperationRunTargetsRequest{
		OperationRunId: &flowv1.UUID{Id: runID},
		Status:         flowv1.OperationRunTargetStatus_OPERATION_RUN_TARGET_STATUS_UNKNOWN,
		PhaseScope:     apiToProtoRunPhaseScope[atrtgar.PhaseScope],
	}
	if atrtgar.Status != "" {
		req.Status = atrtgar.Status.ToProto()
	}
	if page.Offset != nil && page.Limit != nil {
		req.Pagination = &flowv1.Pagination{
			Offset: int32(*page.Offset),
			Limit:  int32(*page.Limit),
		}
	}
	return req
}

// QueryValues returns the fields that feed the workflow ID hash for the
// target-list endpoint, including pagination.
func (atrtgar *APITaskRunTargetGetAllRequest) QueryValues(page pagination.PageRequest) url.Values {
	v := url.Values{}
	v.Set("siteId", atrtgar.SiteID)
	if atrtgar.Status != "" {
		v.Set("status", string(atrtgar.Status))
	}
	if atrtgar.PhaseScope != "" {
		v.Set("phaseScope", atrtgar.PhaseScope)
	}
	if page.PageNumber != nil && *page.PageNumber != 0 {
		v.Set("pageNumber", strconv.Itoa(*page.PageNumber))
	}
	if page.PageSize != nil && *page.PageSize != 0 {
		v.Set("pageSize", strconv.Itoa(*page.PageSize))
	}
	return v
}

// ~~~~~ Lifecycle request bodies (siteId in body) ~~~~~ //

// APITaskRunSiteRequest is the shared JSON body for the pause/resume/advance
// lifecycle actions, which need only the target Site.
type APITaskRunSiteRequest struct {
	SiteID string `json:"siteId"`
}

func (atrsr *APITaskRunSiteRequest) Validate() error {
	return validation.ValidateStruct(atrsr,
		validation.Field(&atrsr.SiteID, validation.Required.Error("siteId is required")),
	)
}

// APITaskRunAdvanceRequest is the JSON body for POST /task/run/{id}/advance.
// ExpectedPhaseIndex is an optional guard: when set, the phase that would be
// opened must match, otherwise Flow rejects the advance.
type APITaskRunAdvanceRequest struct {
	SiteID             string `json:"siteId"`
	ExpectedPhaseIndex *int32 `json:"expectedPhaseIndex"`
}

func (atrar *APITaskRunAdvanceRequest) Validate() error {
	return validation.ValidateStruct(atrar,
		validation.Field(&atrar.SiteID, validation.Required.Error("siteId is required")),
	)
}

// APITaskRunCancelRequest is the JSON body for POST /task/run/{id}/cancel.
type APITaskRunCancelRequest struct {
	SiteID string `json:"siteId"`
	Reason string `json:"reason"`
}

func (atrcr *APITaskRunCancelRequest) Validate() error {
	return validation.ValidateStruct(atrcr,
		validation.Field(&atrcr.SiteID, validation.Required.Error("siteId is required")),
	)
}

// ~~~~~ Create ~~~~~ //

// APITaskRunCreateRequest is the JSON body for POST /task/run. A run executes
// exactly one operation across a candidate set of racks, narrowed by an
// optional selector and divided into phases by an optional phase policy.
// operationType is inferred from the operation and is not accepted here.
type APITaskRunCreateRequest struct {
	SiteID      string `json:"siteId"`
	Name        string `json:"name"`
	Description string `json:"description"`
	// Selector narrows the candidate racks. Omit to target the full candidate
	// scope (100%).
	Selector  *APITaskRunSelector `json:"selector"`
	Options   APITaskRunOptions   `json:"options"`
	Operation APITaskRunOperation `json:"operation"`
}

// Validate enforces request shape only; Flow performs semantic validation
// (selector ranges, phase math, operation code membership) server-side.
func (atrcr *APITaskRunCreateRequest) Validate() error {
	return validation.ValidateStruct(atrcr,
		validation.Field(&atrcr.SiteID, validation.Required.Error("siteId is required")),
		validation.Field(&atrcr.Name, validation.Required.Error("name is required")),
		validation.Field(&atrcr.Options),
		validation.Field(&atrcr.Operation),
	)
}

// ToProto converts the create request into the Flow CreateOperationRunRequest.
// Callers must Validate first: the nested mappers dereference fields whose
// presence only Validate enforces.
func (atrcr *APITaskRunCreateRequest) ToProto() *flowv1.CreateOperationRunRequest {
	cfg := &flowv1.OperationRunConfiguration{
		Options:   atrcr.Options.ToProto(),
		Operation: atrcr.Operation.ToProto(),
	}

	if atrcr.Selector != nil && atrcr.Selector.Percentage != nil {
		cfg.Selector = &flowv1.OperationRunSelector{
			Selector: &flowv1.OperationRunSelector_Percentage{
				Percentage: &flowv1.PercentageSelector{
					Percentage: atrcr.Selector.Percentage.Percent,
					Seed:       atrcr.Selector.Percentage.Seed,
				},
			},
		}
	}

	return &flowv1.CreateOperationRunRequest{
		Name:          atrcr.Name,
		Description:   atrcr.Description,
		Configuration: cfg,
	}
}

// APITaskRunSelector selects a subset of candidate racks. Percentage is the
// only supported selector today.
type APITaskRunSelector struct {
	Percentage *APITaskRunPercentageSelector `json:"percentage"`
}

// APITaskRunPercentageSelector selects a percentage of the candidate racks.
// Seed is optional; when omitted Flow generates and stores one so the cohort
// is deterministic and auditable.
type APITaskRunPercentageSelector struct {
	Percent int32  `json:"percent"`
	Seed    string `json:"seed"`
}

// APITaskRunOptions configures execution policy for the run.
type APITaskRunOptions struct {
	// MaxConcurrentTargets caps how many targets may have active child tasks at
	// once. Required, must be greater than zero.
	MaxConcurrentTargets int32                     `json:"maxConcurrentTargets"`
	SafetyPolicy         *APITaskRunSafetyPolicy   `json:"safetyPolicy"`
	ConflictPolicy       *APITaskRunConflictPolicy `json:"conflictPolicy"`
	OrderingPolicy       *APITaskRunOrderingPolicy `json:"orderingPolicy"`
	PhasePolicy          *APITaskRunPhasePolicy    `json:"phasePolicy"`
}

// Validate enforces the execution policy shape. The nested policies validate
// themselves; ozzo skips the ones left nil.
func (atro APITaskRunOptions) Validate() error {
	return validation.ValidateStruct(&atro,
		// Required rejects zero (the threshold rules treat it as empty and skip
		// it); Min rejects negatives.
		validation.Field(&atro.MaxConcurrentTargets,
			validation.Required.Error("must be greater than zero"),
			validation.Min(1).Error("must be greater than zero")),
		validation.Field(&atro.SafetyPolicy),
		validation.Field(&atro.ConflictPolicy),
		validation.Field(&atro.PhasePolicy),
	)
}

// ToProto maps the execution policy onto Flow's OperationRunOptions.
func (atro APITaskRunOptions) ToProto() *flowv1.OperationRunOptions {
	out := &flowv1.OperationRunOptions{
		MaxConcurrentTargets: atro.MaxConcurrentTargets,
	}

	if atro.SafetyPolicy != nil {
		gates := make([]*flowv1.OperationRunSafetyGate, 0, len(atro.SafetyPolicy.Gates))
		for _, g := range atro.SafetyPolicy.Gates {
			gates = append(gates, g.ToProto())
		}
		out.SafetyPolicy = &flowv1.OperationRunSafetyPolicy{Gates: gates}
	}

	if atro.ConflictPolicy != nil && atro.ConflictPolicy.Retry != nil {
		out.ConflictPolicy = &flowv1.OperationRunConflictPolicy{
			Strategy: &flowv1.OperationRunConflictPolicy_Retry{
				Retry: atro.ConflictPolicy.Retry.ToProto(),
			},
		}
	}

	if atro.OrderingPolicy != nil && atro.OrderingPolicy.Random != nil {
		out.OrderingPolicy = &flowv1.OperationRunOrderingPolicy{
			Ordering: &flowv1.OperationRunOrderingPolicy_Random{
				Random: &flowv1.OperationRunRandomOrdering{Seed: atro.OrderingPolicy.Random.Seed},
			},
		}
	}

	if atro.PhasePolicy != nil {
		out.PhasePolicy = atro.PhasePolicy.ToProto()
	}

	return out
}

// APITaskRunSafetyPolicy is a set of gates that pause the run when any
// one of them trips (OR composition).
type APITaskRunSafetyPolicy struct {
	Gates []APITaskRunSafetyGate `json:"gates"`
}

func (atrsp APITaskRunSafetyPolicy) Validate() error {
	return validation.ValidateStruct(&atrsp,
		validation.Field(&atrsp.Gates),
	)
}

// safety-gate scope maps between the API string and Flow's enum. The empty
// value maps to the current-phase default.
var validTaskRunGateScopes = []string{"currentPhase", "cumulativeRun"}

var validTaskRunGateScopesAny = func() []any {
	out := make([]any, len(validTaskRunGateScopes))
	for i, s := range validTaskRunGateScopes {
		out[i] = s
	}
	return out
}()

var apiToProtoGateScope = map[string]flowv1.OperationRunSafetyGateScope{
	"":              flowv1.OperationRunSafetyGateScope_OPERATION_RUN_SAFETY_GATE_SCOPE_CURRENT_PHASE,
	"currentPhase":  flowv1.OperationRunSafetyGateScope_OPERATION_RUN_SAFETY_GATE_SCOPE_CURRENT_PHASE,
	"cumulativeRun": flowv1.OperationRunSafetyGateScope_OPERATION_RUN_SAFETY_GATE_SCOPE_CUMULATIVE_RUN,
}

// APITaskRunSafetyGate is exactly one of failureRate or failureCount.
type APITaskRunSafetyGate struct {
	FailureRate  *APITaskRunFailureRateGate  `json:"failureRate"`
	FailureCount *APITaskRunFailureCountGate `json:"failureCount"`
}

// Validate requires exactly one gate kind. ozzo cannot express a choice
// between sibling fields declaratively.
func (atrsg APITaskRunSafetyGate) Validate() error {
	if (atrsg.FailureRate == nil) == (atrsg.FailureCount == nil) {
		return errors.New("must set exactly one of failureRate or failureCount")
	}
	return validation.ValidateStruct(&atrsg,
		validation.Field(&atrsg.FailureRate),
		validation.Field(&atrsg.FailureCount),
	)
}

// ToProto maps the gate onto Flow's oneof. Validate guarantees exactly one
// side is set.
func (atrsg APITaskRunSafetyGate) ToProto() *flowv1.OperationRunSafetyGate {
	if atrsg.FailureRate != nil {
		return &flowv1.OperationRunSafetyGate{
			Gate: &flowv1.OperationRunSafetyGate_FailureRate{
				FailureRate: &flowv1.OperationRunFailureRateGate{
					Scope:                   apiToProtoGateScope[atrsg.FailureRate.Scope],
					FailureThresholdPercent: atrsg.FailureRate.ThresholdPercent,
				},
			},
		}
	}
	return &flowv1.OperationRunSafetyGate{
		Gate: &flowv1.OperationRunSafetyGate_FailureCount{
			FailureCount: &flowv1.OperationRunFailureCountGate{
				Scope:                 apiToProtoGateScope[atrsg.FailureCount.Scope],
				FailureThresholdCount: atrsg.FailureCount.ThresholdCount,
			},
		},
	}
}

// APITaskRunFailureRateGate pauses when failed/planned reaches ThresholdPercent
// for the scope.
type APITaskRunFailureRateGate struct {
	Scope            string `json:"scope"`
	ThresholdPercent int32  `json:"thresholdPercent"`
}

func (atrfrg APITaskRunFailureRateGate) Validate() error {
	return validation.ValidateStruct(&atrfrg,
		validation.Field(&atrfrg.Scope,
			validation.In(validTaskRunGateScopesAny...).Error(
				fmt.Sprintf("must be one of %v", validTaskRunGateScopes))),
	)
}

// APITaskRunFailureCountGate pauses when failed targets reach ThresholdCount
// for the scope.
type APITaskRunFailureCountGate struct {
	Scope          string `json:"scope"`
	ThresholdCount int32  `json:"thresholdCount"`
}

func (atrfcg APITaskRunFailureCountGate) Validate() error {
	return validation.ValidateStruct(&atrfcg,
		validation.Field(&atrfcg.Scope,
			validation.In(validTaskRunGateScopesAny...).Error(
				fmt.Sprintf("must be one of %v", validTaskRunGateScopes))),
	)
}

// APITaskRunConflictPolicy configures how blocked targets are retried. Retry
// is the only supported strategy today.
type APITaskRunConflictPolicy struct {
	Retry *APITaskRunConflictRetry `json:"retry"`
}

func (atrcp APITaskRunConflictPolicy) Validate() error {
	return validation.ValidateStruct(&atrcp,
		validation.Field(&atrcp.Retry),
	)
}

// APITaskRunConflictRetry configures retry backoff for blocked targets.
// Durations are Go duration strings (e.g. "30m", "10s"); empty means "use the
// operation default".
type APITaskRunConflictRetry struct {
	RetryTimeout      string `json:"retryTimeout"`
	InitialRetryDelay string `json:"initialRetryDelay"`
	MaxRetryDelay     string `json:"maxRetryDelay"`
}

// Validate rejects unparseable durations here so ToProto stays a pure mapper.
func (atrcr APITaskRunConflictRetry) Validate() error {
	return validation.ValidateStruct(&atrcr,
		validation.Field(&atrcr.RetryTimeout, validation.By(validateOptionalDuration)),
		validation.Field(&atrcr.InitialRetryDelay, validation.By(validateOptionalDuration)),
		validation.Field(&atrcr.MaxRetryDelay, validation.By(validateOptionalDuration)),
	)
}

// ToProto maps the retry backoff onto Flow's policy. Validate guarantees the
// durations parse.
func (atrcr APITaskRunConflictRetry) ToProto() *flowv1.OperationRunConflictRetryPolicy {
	return &flowv1.OperationRunConflictRetryPolicy{
		RetryTimeout:      optionalDurationToProto(atrcr.RetryTimeout),
		InitialRetryDelay: optionalDurationToProto(atrcr.InitialRetryDelay),
		MaxRetryDelay:     optionalDurationToProto(atrcr.MaxRetryDelay),
	}
}

// APITaskRunOrderingPolicy controls the order in which targets are processed.
// Random is the only supported ordering today.
type APITaskRunOrderingPolicy struct {
	Random *APITaskRunRandomOrdering `json:"random"`
}

// APITaskRunRandomOrdering orders targets randomly. Seed is optional; Flow
// generates and stores one when omitted.
type APITaskRunRandomOrdering struct {
	Seed string `json:"seed"`
}

// APITaskRunPhasePolicy divides the selected targets into phases. Exactly one
// of equal, percentage, or count may be set; omit the whole policy for a single
// phase covering all targets.
type APITaskRunPhasePolicy struct {
	Equal      *APITaskRunEqualPhases      `json:"equal"`
	Percentage *APITaskRunPercentagePhases `json:"percentage"`
	Count      *APITaskRunCountPhases      `json:"count"`
	// AutoAdvance, when true, advances phases automatically as long as safety
	// gates are not tripped. When false (default) each completed phase pauses at
	// a phase gate until advanced explicitly.
	AutoAdvance bool `json:"autoAdvance"`
}

// Validate allows at most one phase division. ozzo cannot express a choice
// between sibling fields declaratively.
func (atrpp APITaskRunPhasePolicy) Validate() error {
	set := 0
	for _, isSet := range []bool{atrpp.Equal != nil, atrpp.Percentage != nil, atrpp.Count != nil} {
		if isSet {
			set++
		}
	}
	if set > 1 {
		return errors.New("must set at most one of equal, percentage, count")
	}
	return nil
}

// ToProto maps the phase plan onto Flow's oneof. An unset plan yields a single
// phase covering all targets.
func (atrpp APITaskRunPhasePolicy) ToProto() *flowv1.OperationRunPhasePolicy {
	out := &flowv1.OperationRunPhasePolicy{
		AdvancePolicy: &flowv1.OperationRunPhaseAdvancePolicy{AutoAdvance: atrpp.AutoAdvance},
	}
	switch {
	case atrpp.Equal != nil:
		out.Plan = &flowv1.OperationRunPhasePolicy_Equal{
			Equal: &flowv1.EqualOperationRunPhases{PhaseCount: atrpp.Equal.PhaseCount},
		}
	case atrpp.Percentage != nil:
		phases := make([]*flowv1.OperationRunPercentagePhase, 0, len(atrpp.Percentage.Phases))
		for _, pct := range atrpp.Percentage.Phases {
			phases = append(phases, &flowv1.OperationRunPercentagePhase{Percentage: pct})
		}
		out.Plan = &flowv1.OperationRunPhasePolicy_Percentage{
			Percentage: &flowv1.PercentageOperationRunPhases{Phases: phases},
		}
	case atrpp.Count != nil:
		phases := make([]*flowv1.OperationRunCountPhase, 0, len(atrpp.Count.Phases))
		for _, n := range atrpp.Count.Phases {
			phases = append(phases, &flowv1.OperationRunCountPhase{Count: n})
		}
		out.Plan = &flowv1.OperationRunPhasePolicy_Count{
			Count: &flowv1.CountOperationRunPhases{Phases: phases},
		}
	}
	return out
}

// APITaskRunEqualPhases splits targets into PhaseCount roughly equal phases.
type APITaskRunEqualPhases struct {
	PhaseCount int32 `json:"phaseCount"`
}

// APITaskRunPercentagePhases splits targets by percentage. Values must sum to
// 100.
type APITaskRunPercentagePhases struct {
	Phases []int32 `json:"phases"`
}

// APITaskRunCountPhases splits targets by explicit counts. A generated final
// phase covers any remaining targets.
type APITaskRunCountPhases struct {
	Phases []int32 `json:"phases"`
}

// APITaskRunOperation is the operation the run executes. Firmware is the only
// supported operation today.
type APITaskRunOperation struct {
	Firmware *APITaskRunFirmwareOperation `json:"firmware"`
	// ExcludeRunIDs excludes racks materialized by prior runs from
	// this run's candidate scope.
	ExcludeRunIDs []string `json:"excludeRunIds"`
}

// Validate requires the firmware operation, the only kind Flow supports today.
func (atro APITaskRunOperation) Validate() error {
	return validation.ValidateStruct(&atro,
		validation.Field(&atro.Firmware, validation.Required.Error("firmware is required")),
	)
}

// ToProto maps the operation onto Flow's oneof. Validate guarantees Firmware
// is set.
func (atro APITaskRunOperation) ToProto() *flowv1.OperationRunOperation {
	fw := &flowv1.UpgradeFirmwareRequest{
		TargetVersion:          &atro.Firmware.Version,
		SubTargets:             atro.Firmware.SubTargets,
		OverrideReadinessCheck: atro.Firmware.OverrideReadinessCheck,
	}
	if atro.Firmware.RuleID != nil && *atro.Firmware.RuleID != "" {
		fw.RuleId = &flowv1.UUID{Id: *atro.Firmware.RuleID}
	}

	out := &flowv1.OperationRunOperation{
		Operation: &flowv1.OperationRunOperation_UpgradeFirmware{UpgradeFirmware: fw},
	}

	if len(atro.ExcludeRunIDs) > 0 {
		excludes := make([]*flowv1.UUID, 0, len(atro.ExcludeRunIDs))
		for _, id := range atro.ExcludeRunIDs {
			excludes = append(excludes, &flowv1.UUID{Id: id})
		}
		out.TargetScope = &flowv1.OperationRunTargetScope{ExcludeOperationRunIds: excludes}
	}

	return out
}

// APITaskRunFirmwareOperation configures a firmware rollout.
type APITaskRunFirmwareOperation struct {
	Version                string   `json:"version"`
	RuleID                 *string  `json:"ruleId"`
	OverrideReadinessCheck bool     `json:"overrideReadinessCheck"`
	SubTargets             []string `json:"subTargets"`
}

func (atrfo APITaskRunFirmwareOperation) Validate() error {
	return validation.ValidateStruct(&atrfo,
		validation.Field(&atrfo.Version, validation.Required.Error("version is required")),
	)
}

// validateOptionalDuration accepts an empty string, meaning "use the operation
// default", or any Go duration string.
func validateOptionalDuration(value any) error {
	s, ok := value.(string)
	if !ok || s == "" {
		return nil
	}
	if _, err := time.ParseDuration(s); err != nil {
		return fmt.Errorf("invalid duration %q", s)
	}
	return nil
}

// optionalDurationToProto converts a Go duration string into a protobuf
// Duration. An empty or unparseable string returns nil so Flow falls back to
// the operation default; validateOptionalDuration rejects the latter before a
// request reaches the mappers.
func optionalDurationToProto(s string) *durationpb.Duration {
	d, err := time.ParseDuration(s)
	if err != nil {
		return nil
	}
	return durationpb.New(d)
}
