// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package model

import (
	"fmt"
	"time"

	flowtypes "github.com/NVIDIA/infra-controller/rest-api/flow/pkg/types"
	flowv1 "github.com/NVIDIA/infra-controller/rest-api/proto/flow/gen/v1"
)

const (
	LeakageActionForcePowerOff = "ForcePowerOff"
	LeakageActionNoAction      = "NoAction"
	LeakageTargetComponent     = "LeakingComponent"
	LeakageTargetAffected      = "AffectedComponents"
	LeakageConflictQueue       = "Queue"
	LeakageConflictReject      = "Reject"
)

// APIPutLeakagePolicyRequest is the operator-facing leakage policy body.
type APIPutLeakagePolicyRequest struct {
	Action     string  `json:"action"`
	Target     *string `json:"target"`
	OnConflict *string `json:"onConflict"`
}

// Validate checks action-specific required and inapplicable fields.
func (r APIPutLeakagePolicyRequest) Validate() error {
	switch r.Action {
	case LeakageActionForcePowerOff:
		if r.Target == nil || (*r.Target != LeakageTargetComponent && *r.Target != LeakageTargetAffected) {
			return fmt.Errorf("target must be LeakingComponent or AffectedComponents for ForcePowerOff")
		}
		if r.OnConflict == nil || (*r.OnConflict != LeakageConflictQueue && *r.OnConflict != LeakageConflictReject) {
			return fmt.Errorf("onConflict must be Queue or Reject for ForcePowerOff")
		}
	case LeakageActionNoAction:
		if r.Target != nil || r.OnConflict != nil {
			return fmt.Errorf("target and onConflict must be null for NoAction")
		}
	default:
		return fmt.Errorf("action must be ForcePowerOff or NoAction")
	}
	return nil
}

// EventRuleActions translates the REST policy into Flow's fixed rule shape.
func (r APIPutLeakagePolicyRequest) EventRuleActions() []*flowv1.EventRuleAction {
	if r.Action == LeakageActionNoAction {
		return []*flowv1.EventRuleAction{{Name: "record_without_power_action", Spec: &flowv1.EventRuleAction_Noop{Noop: &flowv1.EventRuleNoopAction{Reason: "Leakage policy is configured for detection without an automatic power response."}}}}
	}
	target := flowv1.EventRuleTargetStrategy_EVENT_RULE_TARGET_STRATEGY_COMPONENT
	if *r.Target == LeakageTargetAffected {
		target = flowv1.EventRuleTargetStrategy_EVENT_RULE_TARGET_STRATEGY_AFFECTED_COMPONENTS
	}
	conflict := flowv1.EventRuleConflictStrategy_EVENT_RULE_CONFLICT_STRATEGY_QUEUE
	if *r.OnConflict == LeakageConflictReject {
		conflict = flowv1.EventRuleConflictStrategy_EVENT_RULE_CONFLICT_STRATEGY_REJECT
	}
	return []*flowv1.EventRuleAction{{Name: "force_power_off", Spec: &flowv1.EventRuleAction_SubmitTask{SubmitTask: &flowv1.EventRuleSubmitTaskAction{TargetStrategy: target, ConflictStrategy: conflict, Description: "Leakage response", Operation: &flowv1.TaskOperation{Operation: &flowv1.TaskOperation_PowerControl{PowerControl: &flowv1.PowerControlTaskOperation{Operation: flowv1.PowerControlOperation_POWER_CONTROL_OPERATION_FORCE_POWER_OFF}}}}}}}
}

// APILeakagePolicy is the effective leakage response at a requested scope.
type APILeakagePolicy struct {
	Action     string     `json:"action"`
	Target     *string    `json:"target"`
	OnConflict *string    `json:"onConflict"`
	Updated    *time.Time `json:"updated"`
}

// FromProto converts a representable Flow event rule into the REST policy.
func (p *APILeakagePolicy) FromProto(rule *flowv1.EventRule) error {
	if rule == nil {
		return fmt.Errorf("effective leakage rule is missing")
	}
	if !rule.GetEnabled() || rule.GetEventType() != string(flowtypes.EventTypeHardwareLeakDetected) || len(rule.GetActions()) != 1 {
		return fmt.Errorf("effective leakage rule is not representable")
	}
	action := rule.GetActions()[0]
	if action.GetCondition() != nil && (len(action.GetCondition().GetSeverities()) != 0 || len(action.GetCondition().GetComponentTypes()) != 0) {
		return fmt.Errorf("conditional leakage rule is not representable")
	}
	switch {
	case action.GetNoop() != nil:
		p.Action = LeakageActionNoAction
	case action.GetSubmitTask() != nil:
		submit := action.GetSubmitTask()
		if submit.GetOperation().GetPowerControl().GetOperation() != flowv1.PowerControlOperation_POWER_CONTROL_OPERATION_FORCE_POWER_OFF {
			return fmt.Errorf("leakage task operation is not representable")
		}
		p.Action = LeakageActionForcePowerOff
		target := LeakageTargetComponent
		if submit.GetTargetStrategy() == flowv1.EventRuleTargetStrategy_EVENT_RULE_TARGET_STRATEGY_AFFECTED_COMPONENTS {
			target = LeakageTargetAffected
		} else if submit.GetTargetStrategy() != flowv1.EventRuleTargetStrategy_EVENT_RULE_TARGET_STRATEGY_COMPONENT {
			return fmt.Errorf("leakage target is not representable")
		}
		conflict := LeakageConflictQueue
		if submit.GetConflictStrategy() == flowv1.EventRuleConflictStrategy_EVENT_RULE_CONFLICT_STRATEGY_REJECT {
			conflict = LeakageConflictReject
		} else if submit.GetConflictStrategy() != flowv1.EventRuleConflictStrategy_EVENT_RULE_CONFLICT_STRATEGY_QUEUE {
			return fmt.Errorf("leakage conflict behavior is not representable")
		}
		p.Target, p.OnConflict = &target, &conflict
	default:
		return fmt.Errorf("leakage action is not representable")
	}
	if !rule.GetReadOnly() && rule.GetUpdatedAt() != nil {
		v := rule.GetUpdatedAt().AsTime().UTC()
		p.Updated = &v
	}
	return nil
}

// APILeakageHandling is one application of leakage policy and its handling work.
type APILeakageHandling struct {
	ID             string                `json:"id"`
	SourceID       string                `json:"sourceId"`
	SourceType     string                `json:"sourceType"`
	HandlingStatus APILeakHandlingStatus `json:"handlingStatus"`
	TaskIDs        []string              `json:"taskIds"`
	HandledAt      time.Time             `json:"handledAt"`
}

const (
	LeakageHandlingSourceTypeComponent = "Component"
	LeakageHandlingSourceTypeRack      = "Rack"
)

var protoToAPILeakageSourceTypeName = map[flowv1.EventResourceType]string{
	flowv1.EventResourceType_EVENT_RESOURCE_TYPE_COMPONENT: LeakageHandlingSourceTypeComponent,
	flowv1.EventResourceType_EVENT_RESOURCE_TYPE_RACK:      LeakageHandlingSourceTypeRack,
}

// NewAPILeakageHandling converts one Flow event-handling record selected for
// the leakage event type.
func NewAPILeakageHandling(handling *flowv1.EventHandling) APILeakageHandling {
	result := APILeakageHandling{
		ID:             handling.GetId().GetId(),
		SourceID:       handling.GetResourceId(),
		SourceType:     enumOr(protoToAPILeakageSourceTypeName, handling.GetResourceType(), "Unknown"),
		HandlingStatus: leakageHandlingStatus(handling.GetTasks()),
		TaskIDs:        make([]string, 0, len(handling.GetTasks())),
	}
	for _, task := range handling.GetTasks() {
		result.TaskIDs = append(result.TaskIDs, task.GetId().GetId())
	}
	if handling.GetHandledAt() != nil {
		result.HandledAt = handling.GetHandledAt().AsTime().UTC()
	}
	return result
}

func leakageHandlingStatus(tasks []*flowv1.EventHandlingTask) APILeakHandlingStatus {
	if len(tasks) == 0 {
		return APILeakHandlingStatusNone
	}
	switch tasks[len(tasks)-1].GetStatus() {
	case flowv1.TaskStatus_TASK_STATUS_WAITING,
		flowv1.TaskStatus_TASK_STATUS_PENDING,
		flowv1.TaskStatus_TASK_STATUS_RUNNING:
		return APILeakHandlingStatusShuttingDown
	case flowv1.TaskStatus_TASK_STATUS_COMPLETED:
		return APILeakHandlingStatusDown
	case flowv1.TaskStatus_TASK_STATUS_FAILED,
		flowv1.TaskStatus_TASK_STATUS_TERMINATED:
		return APILeakHandlingStatusFailed
	default:
		return APILeakHandlingStatusUnknown
	}
}

// APILeakageHandlingPage contains one cursor-paginated handling page.
type APILeakageHandlingPage struct {
	Items      []APILeakageHandling `json:"items"`
	NextCursor *string              `json:"nextCursor"`
}

// APILeakagePolicyQuery supplies the required Site routing scope.
type APILeakagePolicyQuery struct {
	SiteID string `query:"siteId"`
}

func (q APILeakagePolicyQuery) Validate() error {
	if q.SiteID == "" {
		return fmt.Errorf("siteId query parameter is required")
	}
	return nil
}

// APILeakageHandlingQuery supplies Site scope and history selection.
type APILeakageHandlingQuery struct {
	SiteID     string `query:"siteId"`
	SourceType string `query:"sourceType"`
	SourceID   string `query:"sourceId"`
	Limit      *int32 `query:"limit"`
	Cursor     string `query:"cursor"`
}

// Validate checks the required source and cursor-pagination fields.
func (q APILeakageHandlingQuery) Validate() error {
	if q.SiteID == "" {
		return fmt.Errorf("siteId query parameter is required")
	}
	if q.SourceType != LeakageHandlingSourceTypeComponent && q.SourceType != LeakageHandlingSourceTypeRack {
		return fmt.Errorf("sourceType must be Component or Rack")
	}
	if q.SourceID == "" {
		return fmt.Errorf("sourceId query parameter is required")
	}
	if q.Limit != nil && (*q.Limit < 1 || *q.Limit > 100) {
		return fmt.Errorf("limit must be between 1 and 100")
	}
	return nil
}

// EffectiveLimit returns the requested limit or its default of one.
func (q APILeakageHandlingQuery) EffectiveLimit() int32 {
	if q.Limit == nil {
		return 1
	}
	return *q.Limit
}

// ProtoResourceType converts the validated REST source type to Flow's generic
// event resource type.
func (q APILeakageHandlingQuery) ProtoResourceType() flowv1.EventResourceType {
	if q.SourceType == LeakageHandlingSourceTypeRack {
		return flowv1.EventResourceType_EVENT_RESOURCE_TYPE_RACK
	}
	return flowv1.EventResourceType_EVENT_RESOURCE_TYPE_COMPONENT
}
