// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package model

import (
	"fmt"
	"time"

	validation "github.com/go-ozzo/ozzo-validation/v4"
	validationis "github.com/go-ozzo/ozzo-validation/v4/is"

	"github.com/NVIDIA/infra-controller/rest-api/api/pkg/api/pagination"
	flowtypes "github.com/NVIDIA/infra-controller/rest-api/flow/pkg/types"
	flowv1 "github.com/NVIDIA/infra-controller/rest-api/proto/flow/gen/v1"
)

const (
	LeakageActionForcePowerOff = "ForcePowerOff"
	LeakageActionNoAction      = "NoAction"
	LeakageTargetSource        = "LeakingSource"
	LeakageTargetRack          = "LeakingRack"
	LeakageTargetAffected      = "AffectedComponents"
	LeakageConflictQueue       = "Queue"
	LeakageConflictReject      = "Reject"
)

// APILeakageTargetStrategy maps the leakage REST contract to Flow's target strategy.
type APILeakageTargetStrategy string

// Validate rejects strategies outside the supported leakage contract.
func (s APILeakageTargetStrategy) Validate() error {
	return validation.Errors{
		"targetStrategy": validation.Validate(string(s),
			validation.Required.Error("is required for ForcePowerOff"),
			validation.In(LeakageTargetSource, LeakageTargetRack, LeakageTargetAffected).
				Error("must be LeakingSource, LeakingRack, or AffectedComponents for ForcePowerOff"),
		),
	}.Filter()
}

// ToProto maps a validated strategy without substituting a different target.
// Call Validate first; an unsupported value maps to UNSPECIFIED.
func (s APILeakageTargetStrategy) ToProto() flowv1.EventRuleTargetStrategy {
	switch s {
	case LeakageTargetSource:
		return flowv1.EventRuleTargetStrategy_EVENT_RULE_TARGET_STRATEGY_SOURCE
	case LeakageTargetRack:
		return flowv1.EventRuleTargetStrategy_EVENT_RULE_TARGET_STRATEGY_RACK
	case LeakageTargetAffected:
		return flowv1.EventRuleTargetStrategy_EVENT_RULE_TARGET_STRATEGY_AFFECTED_COMPONENTS
	default:
		return flowv1.EventRuleTargetStrategy_EVENT_RULE_TARGET_STRATEGY_UNSPECIFIED
	}
}

// FromProto clears the receiver for strategies outside the REST contract.
func (s *APILeakageTargetStrategy) FromProto(strategy flowv1.EventRuleTargetStrategy) {
	switch strategy {
	case flowv1.EventRuleTargetStrategy_EVENT_RULE_TARGET_STRATEGY_SOURCE:
		*s = LeakageTargetSource
	case flowv1.EventRuleTargetStrategy_EVENT_RULE_TARGET_STRATEGY_RACK:
		*s = LeakageTargetRack
	case flowv1.EventRuleTargetStrategy_EVENT_RULE_TARGET_STRATEGY_AFFECTED_COMPONENTS:
		*s = LeakageTargetAffected
	default:
		*s = ""
	}
}

// APIPutLeakagePolicyRequest is the operator-facing leakage policy body.
type APIPutLeakagePolicyRequest struct {
	SiteID         string  `json:"siteId"`
	Action         string  `json:"action"`
	TargetStrategy *string `json:"targetStrategy"`
	OnConflict     *string `json:"onConflict"`
}

// Validate checks the Site routing UUID and action-specific required fields.
func (r APIPutLeakagePolicyRequest) Validate() error {
	return validation.ValidateStruct(&r,
		validation.Field(&r.SiteID,
			validation.Required.Error("siteId is required"),
			validationis.UUID.Error("siteId must be a valid UUID"),
		),
		validation.Field(&r.Action,
			validation.Required.Error("must be ForcePowerOff or NoAction"),
			validation.In(LeakageActionForcePowerOff, LeakageActionNoAction).Error("must be ForcePowerOff or NoAction"),
		),
		validation.Field(&r.TargetStrategy,
			validation.When(r.Action == LeakageActionForcePowerOff,
				validation.Required.Error("is required for ForcePowerOff"),
				validation.In(LeakageTargetSource, LeakageTargetRack, LeakageTargetAffected).
					Error("must be LeakingSource, LeakingRack, or AffectedComponents for ForcePowerOff"),
			).Else(validation.Nil.Error("must be null for NoAction")),
		),
		validation.Field(&r.OnConflict,
			validation.When(r.Action == LeakageActionForcePowerOff,
				validation.Required.Error("must be Queue or Reject for ForcePowerOff"),
				validation.In(LeakageConflictQueue, LeakageConflictReject).Error("must be Queue or Reject for ForcePowerOff"),
			).Else(validation.Nil.Error("must be null for NoAction")),
		),
	)
}

// EventRuleActions translates the REST policy into Flow's fixed rule shape.
func (r APIPutLeakagePolicyRequest) EventRuleActions() []*flowv1.EventRuleAction {
	var action *flowv1.EventRuleAction

	if r.Action == LeakageActionNoAction {
		action = &flowv1.EventRuleAction{
			Name: "record_without_power_action",
			Spec: &flowv1.EventRuleAction_Noop{
				Noop: &flowv1.EventRuleNoopAction{
					Reason: "Leakage policy is configured for detection without an automatic power response.",
				},
			},
		}
	} else {
		targetStrategy := APILeakageTargetStrategy(*r.TargetStrategy).ToProto()
		conflict := flowv1.EventRuleConflictStrategy_EVENT_RULE_CONFLICT_STRATEGY_QUEUE
		if *r.OnConflict == LeakageConflictReject {
			conflict = flowv1.EventRuleConflictStrategy_EVENT_RULE_CONFLICT_STRATEGY_REJECT
		}

		action = &flowv1.EventRuleAction{
			Name: "force_power_off",
			Spec: &flowv1.EventRuleAction_SubmitTask{
				SubmitTask: &flowv1.EventRuleSubmitTaskAction{
					TargetStrategy:   targetStrategy,
					ConflictStrategy: conflict,
					Description:      "Leakage response",
					Operation: &flowv1.TaskOperation{
						Operation: &flowv1.TaskOperation_PowerControl{
							PowerControl: &flowv1.PowerControlTaskOperation{
								Operation: flowv1.PowerControlOperation_POWER_CONTROL_OPERATION_FORCE_POWER_OFF,
							},
						},
					},
				},
			},
		}
	}

	return []*flowv1.EventRuleAction{action}
}

// APILeakageAppliedPolicy is the action configuration captured for a handling.
// It has no update timestamp because it describes a historical policy snapshot.
type APILeakageAppliedPolicy struct {
	Action         string  `json:"action"`
	TargetStrategy *string `json:"targetStrategy"`
	OnConflict     *string `json:"onConflict"`
}

// APILeakagePolicy is the effective leakage response at a requested scope.
type APILeakagePolicy struct {
	Action         string     `json:"action"`
	TargetStrategy *string    `json:"targetStrategy"`
	OnConflict     *string    `json:"onConflict"`
	Updated        *time.Time `json:"updated"`
}

// APILeakagePolicyScope identifies the default or a Rack override.
type APILeakagePolicyScope string

const (
	LeakagePolicyScopeDefault APILeakagePolicyScope = "Default"
	LeakagePolicyScopeRack    APILeakagePolicyScope = "Rack"
)

// FromProto clears unsupported scopes instead of treating them as defaults.
func (s *APILeakagePolicyScope) FromProto(scope flowv1.EventRuleScopeType) {
	*s = map[flowv1.EventRuleScopeType]APILeakagePolicyScope{
		flowv1.EventRuleScopeType_EVENT_RULE_SCOPE_TYPE_SITE: LeakagePolicyScopeDefault,
		flowv1.EventRuleScopeType_EVENT_RULE_SCOPE_TYPE_RACK: LeakagePolicyScopeRack,
	}[scope]
}

// APILeakagePolicySource distinguishes an immutable fallback from an override.
type APILeakagePolicySource string

const (
	LeakagePolicySourceBuiltIn  APILeakagePolicySource = "BuiltIn"
	LeakagePolicySourceOverride APILeakagePolicySource = "Override"
)

// FromProto derives the policy source from Flow's rule origin marker.
func (s *APILeakagePolicySource) FromProto(rule *flowv1.EventRule) {
	*s = ""
	if rule != nil {
		*s = LeakagePolicySourceOverride
		if rule.GetReadOnly() {
			*s = LeakagePolicySourceBuiltIn
		}
	}
}

// APILeakageScopedPolicy is one effective default or enabled Rack override.
type APILeakageScopedPolicy struct {
	SiteID string                 `json:"siteId"`
	Scope  APILeakagePolicyScope  `json:"scope"`
	RackID *string                `json:"rackId"`
	Source APILeakagePolicySource `json:"source"`
	Policy APILeakagePolicy       `json:"policy"`
}

// FromProto converts a scoped policy. SiteID is supplied by the authorized
// REST routing context and is not replaced by site-local Flow data.
func (p *APILeakageScopedPolicy) FromProto(value *flowv1.ScopedEventRule) error {
	if value == nil {
		return fmt.Errorf("scoped leakage policy is missing")
	}
	var policy APILeakagePolicy
	err := policy.FromProto(value.GetRule())
	if err != nil {
		return err
	}
	var scope APILeakagePolicyScope
	scope.FromProto(value.GetScopeType())
	switch value.GetScopeType() {
	case flowv1.EventRuleScopeType_EVENT_RULE_SCOPE_TYPE_SITE:
		if value.RackId != nil {
			return fmt.Errorf("default leakage policy must not include a Rack identifier")
		}
	case flowv1.EventRuleScopeType_EVENT_RULE_SCOPE_TYPE_RACK:
	default:
		return fmt.Errorf("unsupported leakage policy scope")
	}
	var source APILeakagePolicySource
	source.FromProto(value.GetRule())
	if value.GetRule().GetReadOnly() {
		if scope != LeakagePolicyScopeDefault {
			return fmt.Errorf("rack leakage policy must be an override")
		}
	}
	*p = APILeakageScopedPolicy{SiteID: p.SiteID, Scope: scope, RackID: value.RackId, Source: source, Policy: policy}
	return nil
}

// ToProto maps authorized Site routing and validated pagination into one
// site-local Flow policy collection request.
func (q APILeakagePolicyQuery) ToProto(page pagination.PageRequest) *flowv1.ListScopedEventRulesRequest {
	return &flowv1.ListScopedEventRulesRequest{
		EventType: string(flowtypes.EventTypeHardwareLeakDetected),
		Pagination: &flowv1.Pagination{
			Offset: int32(*page.Offset),
			Limit:  int32(*page.Limit),
		},
	}
}

// FromProto converts a representable Flow event rule into the REST policy.
func (p *APILeakagePolicy) FromProto(rule *flowv1.EventRule) error {
	if rule == nil {
		return fmt.Errorf("effective leakage rule is missing")
	}

	if !rule.GetEnabled() ||
		rule.GetEventType() != string(flowtypes.EventTypeHardwareLeakDetected) ||
		len(rule.GetActions()) != 1 {
		return fmt.Errorf("effective leakage rule is not representable")
	}

	var applied APILeakageAppliedPolicy
	err := applied.FromProto(&flowv1.EventRulePolicy{Actions: rule.GetActions()})
	if err != nil {
		return err
	}

	*p = APILeakagePolicy{Action: applied.Action, TargetStrategy: applied.TargetStrategy, OnConflict: applied.OnConflict}
	if !rule.GetReadOnly() && rule.GetUpdatedAt() != nil {
		v := rule.GetUpdatedAt().AsTime().UTC()
		p.Updated = &v
	}

	return nil
}

// FromProto converts a stored Flow policy snapshot into the leakage contract.
// Policies outside the supported single shutdown or no-op shape return an error.
func (p *APILeakageAppliedPolicy) FromProto(policy *flowv1.EventRulePolicy) error {
	if policy == nil || len(policy.GetActions()) != 1 {
		return fmt.Errorf("applied leakage policy is not representable")
	}

	action := policy.GetActions()[0]
	if action.GetCondition() != nil &&
		(len(action.GetCondition().GetSeverities()) != 0 ||
			len(action.GetCondition().GetComponentTypes()) != 0) {
		return fmt.Errorf("conditional leakage rule is not representable")
	}

	switch {
	case action.GetNoop() != nil:
		*p = APILeakageAppliedPolicy{Action: LeakageActionNoAction}
	case action.GetSubmitTask() != nil:
		submit := action.GetSubmitTask()
		if submit.GetOperation().GetPowerControl().GetOperation() != flowv1.PowerControlOperation_POWER_CONTROL_OPERATION_FORCE_POWER_OFF {
			return fmt.Errorf("leakage task operation is not representable")
		}

		var strategy APILeakageTargetStrategy
		strategy.FromProto(submit.GetTargetStrategy())
		if strategy == "" {
			return fmt.Errorf("leakage targetStrategy is not representable")
		}

		targetStrategy := string(strategy)
		conflict := LeakageConflictQueue
		if submit.GetConflictStrategy() == flowv1.EventRuleConflictStrategy_EVENT_RULE_CONFLICT_STRATEGY_REJECT {
			conflict = LeakageConflictReject
		} else if submit.GetConflictStrategy() != flowv1.EventRuleConflictStrategy_EVENT_RULE_CONFLICT_STRATEGY_QUEUE {
			return fmt.Errorf("leakage conflict behavior is not representable")
		}

		*p = APILeakageAppliedPolicy{Action: LeakageActionForcePowerOff, TargetStrategy: &targetStrategy, OnConflict: &conflict}
	default:
		return fmt.Errorf("leakage action is not representable")
	}
	return nil
}

// APILeakageEvent identifies and describes the event that caused a handling.
type APILeakageEvent struct {
	DetectedAt *time.Time `json:"detectedAt"`
	Summary    string     `json:"summary"`
	SourceID   string     `json:"sourceId"`
	SourceType string     `json:"sourceType"`
}

// APILeakageHandlingStatus describes dispatch and Task progress for a leakage event.
type APILeakageHandlingStatus string

const (
	APILeakageHandlingStatusUnknown      APILeakageHandlingStatus = "Unknown"
	APILeakageHandlingStatusNone         APILeakageHandlingStatus = "None"
	APILeakageHandlingStatusStarting     APILeakageHandlingStatus = "Starting"
	APILeakageHandlingStatusRetrying     APILeakageHandlingStatus = "Retrying"
	APILeakageHandlingStatusSkipped      APILeakageHandlingStatus = "Skipped"
	APILeakageHandlingStatusShuttingDown APILeakageHandlingStatus = "ShuttingDown"
	APILeakageHandlingStatusDown         APILeakageHandlingStatus = "Down"
	APILeakageHandlingStatusFailed       APILeakageHandlingStatus = "Failed"
)

// FromProto maps Flow's authoritative result into leakage-facing status names.
// Unspecified and unrecognized values map to Unknown without recomputation.
func (s *APILeakageHandlingStatus) FromProto(status flowv1.EventHandlingStatus) {
	*s = enumOr(map[flowv1.EventHandlingStatus]APILeakageHandlingStatus{
		flowv1.EventHandlingStatus_EVENT_HANDLING_STATUS_NO_ACTION: APILeakageHandlingStatusNone,
		flowv1.EventHandlingStatus_EVENT_HANDLING_STATUS_STARTING:  APILeakageHandlingStatusStarting,
		flowv1.EventHandlingStatus_EVENT_HANDLING_STATUS_RETRYING:  APILeakageHandlingStatusRetrying,
		flowv1.EventHandlingStatus_EVENT_HANDLING_STATUS_RUNNING:   APILeakageHandlingStatusShuttingDown,
		flowv1.EventHandlingStatus_EVENT_HANDLING_STATUS_COMPLETED: APILeakageHandlingStatusDown,
		flowv1.EventHandlingStatus_EVENT_HANDLING_STATUS_SKIPPED:   APILeakageHandlingStatusSkipped,
		flowv1.EventHandlingStatus_EVENT_HANDLING_STATUS_FAILED:    APILeakageHandlingStatusFailed,
	}, status, APILeakageHandlingStatusUnknown)
}

// APILeakageHandlingReason is Flow's bounded handling diagnostic code.
// Its zero value represents the absence of a recognized reason.
type APILeakageHandlingReason string

const (
	APILeakageHandlingReasonNoTargets          APILeakageHandlingReason = "NoTargets"
	APILeakageHandlingReasonAttemptFailed      APILeakageHandlingReason = "AttemptFailed"
	APILeakageHandlingReasonAttemptInterrupted APILeakageHandlingReason = "AttemptInterrupted"
	APILeakageHandlingReasonRetryScheduled     APILeakageHandlingReason = "RetryScheduled"
	APILeakageHandlingReasonActionFailed       APILeakageHandlingReason = "ActionFailed"
	APILeakageHandlingReasonTaskFailed         APILeakageHandlingReason = "TaskFailed"
	APILeakageHandlingReasonIncompleteState    APILeakageHandlingReason = "IncompleteState"
)

// FromProto clears the receiver for unspecified or unrecognized reasons.
func (r *APILeakageHandlingReason) FromProto(reason flowv1.EventHandlingReason) {
	*r = map[flowv1.EventHandlingReason]APILeakageHandlingReason{
		flowv1.EventHandlingReason_EVENT_HANDLING_REASON_NO_TARGETS:          APILeakageHandlingReasonNoTargets,
		flowv1.EventHandlingReason_EVENT_HANDLING_REASON_ATTEMPT_FAILED:      APILeakageHandlingReasonAttemptFailed,
		flowv1.EventHandlingReason_EVENT_HANDLING_REASON_ATTEMPT_INTERRUPTED: APILeakageHandlingReasonAttemptInterrupted,
		flowv1.EventHandlingReason_EVENT_HANDLING_REASON_RETRY_SCHEDULED:     APILeakageHandlingReasonRetryScheduled,
		flowv1.EventHandlingReason_EVENT_HANDLING_REASON_ACTION_FAILED:       APILeakageHandlingReasonActionFailed,
		flowv1.EventHandlingReason_EVENT_HANDLING_REASON_TASK_FAILED:         APILeakageHandlingReasonTaskFailed,
		flowv1.EventHandlingReason_EVENT_HANDLING_REASON_INCOMPLETE_STATE:    APILeakageHandlingReasonIncompleteState,
	}[reason]
}

// APILeakageHandlingTask identifies a handling Task and its current task-API status.
type APILeakageHandlingTask struct {
	ID     string `json:"id"`
	Status string `json:"status"`
}

// APILeakageHandling is one application of leakage policy and its handling work.
type APILeakageHandling struct {
	Event          APILeakageEvent           `json:"event"`
	AppliedPolicy  APILeakageAppliedPolicy   `json:"appliedPolicy"`
	Reason         *APILeakageHandlingReason `json:"reason"`
	Message        string                    `json:"message"`
	HandlingStatus APILeakageHandlingStatus  `json:"handlingStatus"`
	Tasks          []APILeakageHandlingTask  `json:"tasks"`
	HandledAt      time.Time                 `json:"handledAt"`
}

const (
	LeakageHandlingSourceTypeComponent = "Component"
	LeakageHandlingSourceTypeRack      = "Rack"
)

var protoToAPILeakageSourceTypeName = map[flowv1.EventResourceType]string{
	flowv1.EventResourceType_EVENT_RESOURCE_TYPE_COMPONENT: LeakageHandlingSourceTypeComponent,
	flowv1.EventResourceType_EVENT_RESOURCE_TYPE_RACK:      LeakageHandlingSourceTypeRack,
}

// FromProto converts a leakage event and its captured policy and handling work.
// An absent or unsupported applied policy returns an error rather than substituting
// the effective policy at the time of the read.
func (h *APILeakageHandling) FromProto(handling *flowv1.EventHandling) error {
	if handling == nil || handling.GetEventType() != string(flowtypes.EventTypeHardwareLeakDetected) {
		return fmt.Errorf("leakage handling event is missing or unsupported")
	}
	var policy APILeakageAppliedPolicy
	err := policy.FromProto(handling.GetAppliedPolicy())
	if err != nil {
		return err
	}
	result := APILeakageHandling{
		Event: APILeakageEvent{
			Summary:    handling.GetEventSummary(),
			SourceID:   handling.GetResourceId(),
			SourceType: enumOr(protoToAPILeakageSourceTypeName, handling.GetResourceType(), "Unknown"),
		},
		AppliedPolicy: policy,

		Tasks:   make([]APILeakageHandlingTask, 0, len(handling.GetTasks())),
		Message: handling.GetMessage(),
	}
	if handling.GetEventFirstObservedAt() != nil {
		detectedAt := handling.GetEventFirstObservedAt().AsTime().UTC()
		result.Event.DetectedAt = &detectedAt
	}
	result.HandlingStatus.FromProto(handling.GetHandlingStatus())
	var reason APILeakageHandlingReason
	reason.FromProto(handling.GetReason())
	if reason != "" {
		result.Reason = &reason
	}
	for _, task := range handling.GetTasks() {
		result.Tasks = append(result.Tasks, APILeakageHandlingTask{
			ID:     task.GetId().GetId(),
			Status: enumOr(ProtoToAPITaskStatusName, task.GetStatus(), "Unknown"),
		})
	}
	if handling.GetHandledAt() != nil {
		result.HandledAt = handling.GetHandledAt().AsTime().UTC()
	}
	*h = result
	return nil
}

// APILeakagePolicyQuery supplies the required Site routing scope.
type APILeakagePolicyQuery struct {
	SiteID string `query:"siteId"`
}

func (q APILeakagePolicyQuery) Validate() error {
	return validation.ValidateStruct(&q,
		validation.Field(&q.SiteID, validation.Required.Error("siteId query parameter is required")),
	)
}

// APILeakageHandlingQuery supplies Site scope and history selection.
type APILeakageHandlingQuery struct {
	SiteID     string `query:"siteId"`
	SourceType string `query:"sourceType"`
	SourceID   string `query:"sourceId"`
}

// Validate checks the required source selectors. Pagination is validated separately.
func (q APILeakageHandlingQuery) Validate() error {
	return validation.ValidateStruct(&q,
		validation.Field(&q.SiteID, validation.Required.Error("siteId query parameter is required")),
		validation.Field(&q.SourceType,
			validation.Required.Error("sourceType must be Component or Rack"),
			validation.In(LeakageHandlingSourceTypeComponent, LeakageHandlingSourceTypeRack).Error("sourceType must be Component or Rack"),
		),
		validation.Field(&q.SourceID, validation.Required.Error("sourceId query parameter is required")),
	)
}

// ToProto converts source selectors and validated, normalized pagination to Flow.
// Call Validate and PageRequest.Validate before converting.
func (q APILeakageHandlingQuery) ToProto(page pagination.PageRequest) *flowv1.GetEventHandlingRequest {
	return &flowv1.GetEventHandlingRequest{
		EventType:    string(flowtypes.EventTypeHardwareLeakDetected),
		ResourceType: q.ProtoResourceType(),
		ResourceId:   q.SourceID,
		Offset:       int32(*page.Offset),
		Limit:        int32(*page.Limit),
	}
}

// ProtoResourceType converts the validated REST source type to Flow's generic
// event resource type.
func (q APILeakageHandlingQuery) ProtoResourceType() flowv1.EventResourceType {
	if q.SourceType == LeakageHandlingSourceTypeRack {
		return flowv1.EventResourceType_EVENT_RESOURCE_TYPE_RACK
	}
	return flowv1.EventResourceType_EVENT_RESOURCE_TYPE_COMPONENT
}
