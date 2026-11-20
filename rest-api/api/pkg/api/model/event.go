// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package model

import (
	"fmt"
	"regexp"
	"time"

	validation "github.com/go-ozzo/ozzo-validation/v4"
	validationis "github.com/go-ozzo/ozzo-validation/v4/is"

	"github.com/NVIDIA/infra-controller/rest-api/api/pkg/api/pagination"
	flowv1 "github.com/NVIDIA/infra-controller/rest-api/proto/flow/gen/v1"
)

// EventTypeHardwareLeakDetected is the initially supported event-rule family.
// Do not import flow/pkg/types here: it registers a second copy of Flow's protobufs.
const EventTypeHardwareLeakDetected = "hardware.leak.detected"

var eventActionName = regexp.MustCompile(`^[a-z][a-z0-9_]*$`)
var eventSeverities = map[string]flowv1.EventRuleSeverity{
	"Info":     flowv1.EventRuleSeverity_EVENT_RULE_SEVERITY_INFO,
	"Warning":  flowv1.EventRuleSeverity_EVENT_RULE_SEVERITY_WARNING,
	"Critical": flowv1.EventRuleSeverity_EVENT_RULE_SEVERITY_CRITICAL,
}
var eventComponentTypes = map[string]flowv1.ComponentType{
	"Compute":    flowv1.ComponentType_COMPONENT_TYPE_COMPUTE,
	"NVSwitch":   flowv1.ComponentType_COMPONENT_TYPE_NVSWITCH,
	"PowerShelf": flowv1.ComponentType_COMPONENT_TYPE_POWERSHELF,
	"TORSwitch":  flowv1.ComponentType_COMPONENT_TYPE_TORSWITCH,
	"UMS":        flowv1.ComponentType_COMPONENT_TYPE_UMS,
	"CDU":        flowv1.ComponentType_COMPONENT_TYPE_CDU,
}
var eventTargets = map[string]flowv1.EventRuleTargetStrategy{
	"Source":             flowv1.EventRuleTargetStrategy_EVENT_RULE_TARGET_STRATEGY_SOURCE,
	"Rack":               flowv1.EventRuleTargetStrategy_EVENT_RULE_TARGET_STRATEGY_RACK,
	"AffectedComponents": flowv1.EventRuleTargetStrategy_EVENT_RULE_TARGET_STRATEGY_AFFECTED_COMPONENTS,
}
var eventConflicts = map[string]flowv1.EventRuleConflictStrategy{
	"Queue":  flowv1.EventRuleConflictStrategy_EVENT_RULE_CONFLICT_STRATEGY_QUEUE,
	"Reject": flowv1.EventRuleConflictStrategy_EVENT_RULE_CONFLICT_STRATEGY_REJECT,
}

func eventEnumRule[T comparable](values map[string]T) validation.Rule {
	accepted := make([]any, 0, len(values))
	for name := range values {
		accepted = append(accepted, name)
	}
	return validation.In(accepted...)
}
func eventEnumName[T comparable](values map[string]T, value T) string {
	for name, v := range values {
		if v == value {
			return name
		}
	}
	return ""
}

// APIEventActionCondition combines OR within each list and AND across lists.
// Null lists impose no constraint; explicitly empty lists are rejected.
type APIEventActionCondition struct {
	Severities     []string `json:"severities"`
	ComponentTypes []string `json:"componentTypes"`
}

// Validate checks the REST contract before conversion or side effects.
func (c APIEventActionCondition) Validate() error {
	return validation.ValidateStruct(&c,
		validation.Field(&c.Severities, validation.When(c.Severities != nil, validation.Required), validation.Each(eventEnumRule(eventSeverities))),
		validation.Field(&c.ComponentTypes, validation.When(c.ComponentTypes != nil, validation.Required), validation.Each(eventEnumRule(eventComponentTypes))),
	)
}

// ToProto maps validated REST values to the corresponding Flow request.
func (c APIEventActionCondition) ToProto() *flowv1.EventRuleActionCondition {
	p := &flowv1.EventRuleActionCondition{}
	for _, v := range c.Severities {
		p.Severities = append(p.Severities, eventSeverities[v])
	}
	for _, v := range c.ComponentTypes {
		p.ComponentTypes = append(p.ComponentTypes, eventComponentTypes[v])
	}
	return p
}

// APIEventSubmitTask resolves event-derived targets and submits a typed operation.
type APIEventSubmitTask struct {
	TargetStrategy string           `json:"targetStrategy"`
	OnConflict     string           `json:"onConflict"`
	Description    string           `json:"description"`
	Operation      APITaskOperation `json:"operation"`
}

// Validate checks the REST contract before conversion or side effects.
func (s APIEventSubmitTask) Validate() error {
	return validation.ValidateStruct(&s,
		validation.Field(&s.TargetStrategy, validation.Required, eventEnumRule(eventTargets)),
		validation.Field(&s.OnConflict, validation.Required, eventEnumRule(eventConflicts)),
		validation.Field(&s.Operation),
	)
}

// APIEventSendAlert configures an alert when the Site has an alert executor.
type APIEventSendAlert struct {
	Severity string `json:"severity"`
	Message  string `json:"message"`
}

// Validate checks the REST contract before conversion or side effects.
func (a APIEventSendAlert) Validate() error {
	return validation.ValidateStruct(&a, validation.Field(&a.Severity, validation.Required, eventEnumRule(eventSeverities)))
}

// APIEventNoAction records an explicit no-op and its reason.
type APIEventNoAction struct {
	Reason string `json:"reason"`
}

// Validate checks the REST contract before conversion or side effects.
func (a APIEventNoAction) Validate() error {
	return nil
}

// APIEventAction selects exactly one specification; names are unique within a policy.
type APIEventAction struct {
	Name       string                   `json:"name"`
	Condition  *APIEventActionCondition `json:"condition"`
	SubmitTask *APIEventSubmitTask      `json:"submitTask"`
	SendAlert  *APIEventSendAlert       `json:"sendAlert"`
	NoAction   *APIEventNoAction        `json:"noAction"`
}

// Validate checks the REST contract before conversion or side effects.
func (a APIEventAction) Validate() error {
	errors := validation.Errors{"name": validation.Validate(a.Name, validation.Required, validation.Match(eventActionName)), "condition": validation.Validate(a.Condition), "submitTask": validation.Validate(a.SubmitTask), "sendAlert": validation.Validate(a.SendAlert), "noAction": validation.Validate(a.NoAction)}
	count := 0
	if a.SubmitTask != nil {
		count++
	}
	if a.SendAlert != nil {
		count++
	}
	if a.NoAction != nil {
		count++
	}
	if count != 1 {
		errors["specification"] = fmt.Errorf("exactly one submitTask, sendAlert, or noAction is required")
	}
	return errors.Filter()
}

// ToProto maps validated REST values to the corresponding Flow request.
func (a APIEventAction) ToProto() *flowv1.EventRuleAction {
	p := &flowv1.EventRuleAction{Name: a.Name}
	if a.Condition != nil {
		p.Condition = a.Condition.ToProto()
	}
	if a.SubmitTask != nil {
		s := a.SubmitTask
		p.Spec = &flowv1.EventRuleAction_SubmitTask{SubmitTask: &flowv1.EventRuleSubmitTaskAction{TargetStrategy: eventTargets[s.TargetStrategy], ConflictStrategy: eventConflicts[s.OnConflict], Description: s.Description, Operation: s.Operation.ToProto()}}
	}
	if a.SendAlert != nil {
		p.Spec = &flowv1.EventRuleAction_SendAlert{SendAlert: &flowv1.EventRuleSendAlertAction{Severity: eventSeverities[a.SendAlert.Severity], Message: a.SendAlert.Message}}
	}
	if a.NoAction != nil {
		p.Spec = &flowv1.EventRuleAction_Noop{Noop: &flowv1.EventRuleNoopAction{Reason: a.NoAction.Reason}}
	}
	return p
}

// FromProto replaces the receiver with the structured Flow representation.
func (a *APIEventAction) FromProto(p *flowv1.EventRuleAction) error {
	*a = APIEventAction{Name: p.GetName()}
	if p.GetCondition() != nil {
		a.Condition = &APIEventActionCondition{}
		for _, v := range p.GetCondition().GetSeverities() {
			a.Condition.Severities = append(a.Condition.Severities, eventEnumName(eventSeverities, v))
		}
		for _, v := range p.GetCondition().GetComponentTypes() {
			a.Condition.ComponentTypes = append(a.Condition.ComponentTypes, eventEnumName(eventComponentTypes, v))
		}
	}
	switch {
	case p.GetSubmitTask() != nil:
		s := p.GetSubmitTask()
		a.SubmitTask = &APIEventSubmitTask{TargetStrategy: eventEnumName(eventTargets, s.GetTargetStrategy()), OnConflict: eventEnumName(eventConflicts, s.GetConflictStrategy()), Description: s.GetDescription()}
		err := a.SubmitTask.Operation.FromProto(s.GetOperation())
		if err != nil {
			return err
		}
	case p.GetSendAlert() != nil:
		a.SendAlert = &APIEventSendAlert{Severity: eventEnumName(eventSeverities, p.GetSendAlert().GetSeverity()), Message: p.GetSendAlert().GetMessage()}
	case p.GetNoop() != nil:
		a.NoAction = &APIEventNoAction{Reason: p.GetNoop().GetReason()}
	default:
		return fmt.Errorf("unsupported event action")
	}
	return a.Validate()
}

// APIEventActions validates action membership and stable names.
type APIEventActions []APIEventAction

// Validate checks the REST contract before conversion or side effects.
func (a APIEventActions) Validate() error {
	err := validation.Validate([]APIEventAction(a), validation.Required, validation.Each())
	if err != nil {
		return err
	}
	names := make(map[string]bool, len(a))
	for _, action := range a {
		if names[action.Name] {
			return validation.Errors{"actions": fmt.Errorf("duplicate action name %q", action.Name)}
		}
		names[action.Name] = true
	}
	return nil
}

// ToProto maps validated REST values to the corresponding Flow request.
func (a APIEventActions) ToProto() []*flowv1.EventRuleAction {
	out := make([]*flowv1.EventRuleAction, 0, len(a))
	for _, v := range a {
		out = append(out, v.ToProto())
	}
	return out
}

// FromProto replaces the receiver with the structured Flow representation.
func (a *APIEventActions) FromProto(actions []*flowv1.EventRuleAction) error {
	*a = make(APIEventActions, 0, len(actions))
	for _, v := range actions {
		var action APIEventAction
		err := action.FromProto(v)
		if err != nil {
			return err
		}
		*a = append(*a, action)
	}
	return nil
}

// APIEventPolicy is a reusable policy definition, including its initial or current state.
type APIEventPolicy struct {
	ID          string          `json:"id"`
	Name        string          `json:"name"`
	Description string          `json:"description"`
	EventType   string          `json:"eventType"`
	Enabled     bool            `json:"enabled"`
	ReadOnly    bool            `json:"readOnly"`
	Actions     APIEventActions `json:"actions"`
	Created     *time.Time      `json:"created"`
	Updated     *time.Time      `json:"updated"`
}

// FromProto replaces the receiver with the structured Flow representation.
func (p *APIEventPolicy) FromProto(v *flowv1.EventRule) error {
	if v == nil || v.GetId().GetId() == "" || v.GetEventType() == "" || len(v.GetActions()) == 0 {
		return fmt.Errorf("missing event policy")
	}
	*p = APIEventPolicy{ID: v.GetId().GetId(), Name: v.GetName(), Description: v.GetDescription(), EventType: v.GetEventType(), Enabled: v.GetEnabled(), ReadOnly: v.GetReadOnly(), Created: timePtrFromProto(v.GetCreatedAt()), Updated: timePtrFromProto(v.GetUpdatedAt())}
	return p.Actions.FromProto(v.GetActions())
}

// APIEventPolicyCreateRequest creates an immutable definition, enabled by default.
type APIEventPolicyCreateRequest struct {
	SiteID      string          `json:"siteId"`
	Name        string          `json:"name"`
	Description string          `json:"description"`
	EventType   string          `json:"eventType"`
	Enabled     *bool           `json:"enabled"`
	Actions     APIEventActions `json:"actions"`
}

// Validate checks the REST contract before conversion or side effects.
func (r APIEventPolicyCreateRequest) Validate() error {
	return validation.ValidateStruct(&r, validation.Field(&r.SiteID, validation.Required, validationis.UUID), validation.Field(&r.Name, validation.Required, validation.Length(1, 128)), validation.Field(&r.Description, validation.Length(0, 1024)), validation.Field(&r.EventType, validation.Required, validation.In(EventTypeHardwareLeakDetected)), validation.Field(&r.Actions))
}

// ToProto maps validated REST values to the corresponding Flow request.
func (r APIEventPolicyCreateRequest) ToProto() *flowv1.CreateEventRuleRequest {
	enabled := true
	if r.Enabled != nil {
		enabled = *r.Enabled
	}
	return &flowv1.CreateEventRuleRequest{Name: r.Name, Description: r.Description, EventType: r.EventType, Enabled: &enabled, Actions: r.Actions.ToProto()}
}

// APIEventPolicyUpdateRequest changes only enabled state; null is not a desired state.
type APIEventPolicyUpdateRequest struct {
	SiteID  string `json:"siteId"`
	Enabled *bool  `json:"enabled"`
}

// Validate checks the REST contract before conversion or side effects.
func (r APIEventPolicyUpdateRequest) Validate() error {
	return validation.ValidateStruct(&r, validation.Field(&r.SiteID, validation.Required, validationis.UUID), validation.Field(&r.Enabled, validation.NotNil))
}

// APISiteEventPolicyBindingRequest assigns an existing policy to the Site scope.
type APISiteEventPolicyBindingRequest struct {
	SiteID    string `json:"siteId"`
	EventType string `json:"eventType"`
	PolicyID  string `json:"policyId"`
}

// Validate checks the REST contract before conversion or side effects.
func (r APISiteEventPolicyBindingRequest) Validate() error {
	return validation.ValidateStruct(&r, validation.Field(&r.SiteID, validation.Required, validationis.UUID), validation.Field(&r.EventType, validation.Required, validation.In(EventTypeHardwareLeakDetected)), validation.Field(&r.PolicyID, validation.Required, validationis.UUID))
}

// ToProto converts the validated body into an explicit Site assignment.
func (r APISiteEventPolicyBindingRequest) ToProto() *flowv1.PutEventRuleBindingRequest {
	return &flowv1.PutEventRuleBindingRequest{
		EventType: r.EventType,
		RuleId:    &flowv1.UUID{Id: r.PolicyID},
		Scope:     &flowv1.EventRuleScopeSelector{Type: flowv1.EventRuleScopeType_EVENT_RULE_SCOPE_TYPE_SITE},
	}
}

// APIRackEventPolicyBindingRequest assigns an existing policy to the path's Rack.
type APIRackEventPolicyBindingRequest struct {
	RackID    string `param:"rackId" json:"-"`
	SiteID    string `json:"siteId"`
	EventType string `json:"eventType"`
	PolicyID  string `json:"policyId"`
}

// Validate checks the required path selector and body before side effects.
func (r APIRackEventPolicyBindingRequest) Validate() error {
	return validation.ValidateStruct(&r,
		validation.Field(&r.RackID, validation.Required.Error("rackId path parameter is required")),
		validation.Field(&r.SiteID, validation.Required, validationis.UUID),
		validation.Field(&r.EventType, validation.Required, validation.In(EventTypeHardwareLeakDetected)),
		validation.Field(&r.PolicyID, validation.Required, validationis.UUID),
	)
}

// ToProto converts the validated body and path selector into a Rack assignment.
func (r APIRackEventPolicyBindingRequest) ToProto() *flowv1.PutEventRuleBindingRequest {
	scope := EventScopeSelector(r.RackID)
	scope.Type = flowv1.EventRuleScopeType_EVENT_RULE_SCOPE_TYPE_RACK
	return &flowv1.PutEventRuleBindingRequest{EventType: r.EventType, RuleId: &flowv1.UUID{Id: r.PolicyID}, Scope: scope}
}

// APIEventPolicyBinding reports an explicit assignment, including disabled policies.
type APIEventPolicyBinding struct {
	SiteID    string  `json:"siteId"`
	EventType string  `json:"eventType"`
	PolicyID  string  `json:"policyId"`
	ScopeType string  `json:"scopeType"`
	RackID    *string `json:"rackId"`
}

// FromProto replaces the receiver with the structured Flow representation.
func (b *APIEventPolicyBinding) FromProto(p *flowv1.EventRuleBinding) error {
	if p == nil || p.GetRuleId().GetId() == "" || p.GetEventType() == "" {
		return fmt.Errorf("missing explicit event policy binding")
	}
	if p.GetScope().GetType() != flowv1.EventRuleScopeType_EVENT_RULE_SCOPE_TYPE_SITE && p.GetScope().GetType() != flowv1.EventRuleScopeType_EVENT_RULE_SCOPE_TYPE_RACK {
		return fmt.Errorf("unsupported event policy binding scope")
	}
	b.EventType = p.GetEventType()
	b.PolicyID = p.GetRuleId().GetId()
	b.ScopeType = "Site"
	b.RackID = nil
	if p.GetScope().GetType() == flowv1.EventRuleScopeType_EVENT_RULE_SCOPE_TYPE_RACK {
		b.ScopeType = "Rack"
		if p.GetRackExternalId() != "" {
			id := p.GetRackExternalId()
			b.RackID = &id
		}
	}
	return nil
}

// EventScopeSelector maps external Rack identifiers onto Flow's selector.
// UUID-shaped values remain external identifiers.
func EventScopeSelector(rackID string) *flowv1.EventRuleScopeSelector {
	if rackID != "" {
		return &flowv1.EventRuleScopeSelector{
			Type:           flowv1.EventRuleScopeType_EVENT_RULE_SCOPE_TYPE_RACK,
			RackIdentifier: &flowv1.EventRuleScopeSelector_RackExternalId{RackExternalId: rackID},
		}
	}

	return &flowv1.EventRuleScopeSelector{
		Type: flowv1.EventRuleScopeType_EVENT_RULE_SCOPE_TYPE_SITE,
	}
}

// APIEventPolicyGetRequest selects the Site for a policy read.
// The policy UUID is supplied in the URL path.
type APIEventPolicyGetRequest struct {
	SiteID string `query:"siteId"`
}

// Validate checks the Site routing selector.
func (q APIEventPolicyGetRequest) Validate() error {
	return validation.ValidateStruct(&q,
		validation.Field(&q.SiteID, validation.Required, validationis.UUID),
	)
}

// APIEventPolicyDeleteRequest selects the Site for deleting an unbound policy.
// The policy UUID is supplied in the URL path.
type APIEventPolicyDeleteRequest struct {
	SiteID string `query:"siteId"`
}

// Validate checks the Site routing selector.
func (q APIEventPolicyDeleteRequest) Validate() error {
	return validation.ValidateStruct(&q,
		validation.Field(&q.SiteID, validation.Required, validationis.UUID),
	)
}

// APIEventPolicyQuery filters the reusable policy catalog.
type APIEventPolicyQuery struct {
	SiteID    string  `query:"siteId"`
	EventType *string `query:"eventType"`
	Enabled   *bool   `query:"enabled"`
	Bound     *bool   `query:"bound"`
}

// Validate checks the policy catalog selectors.
func (q APIEventPolicyQuery) Validate() error {
	return validation.ValidateStruct(&q,
		validation.Field(&q.SiteID, validation.Required, validationis.UUID),
		validation.Field(&q.EventType, validation.When(q.EventType != nil, validation.Required), validation.In(EventTypeHardwareLeakDetected)),
	)
}

// ToProto builds Flow's policy query from validated filters and pagination.
// A nil page leaves protobuf pagination unset.
func (q APIEventPolicyQuery) ToProto(page *pagination.PageRequest) *flowv1.ListEventRulesRequest {
	return &flowv1.ListEventRulesRequest{EventType: q.EventType, Enabled: q.Enabled, Bound: q.Bound, Pagination: EventPagination(page)}
}

// APIEventPolicyBindingListRequest filters the explicit binding collection.
// Filters combine with AND; omitted filters include all assignments.
type APIEventPolicyBindingListRequest struct {
	SiteID    string  `query:"siteId"`
	EventType *string `query:"eventType"`
	ScopeType *string `query:"scopeType"`
	RackID    *string `query:"rackId"`
	PolicyID  *string `query:"policyId"`
}

// Validate checks routing, optional filters, and incompatible scope selectors.
func (q APIEventPolicyBindingListRequest) Validate() error {
	err := validation.ValidateStruct(&q,
		validation.Field(&q.SiteID, validation.Required, validationis.UUID),
		validation.Field(&q.EventType, validation.When(q.EventType != nil, validation.Required), validation.In(EventTypeHardwareLeakDetected)),
		validation.Field(&q.ScopeType, validation.When(q.ScopeType != nil, validation.Required), validation.In("Site", "Rack")),
		validation.Field(&q.PolicyID, validation.When(q.PolicyID != nil, validation.Required), validationis.UUID),
		validation.Field(&q.RackID, validation.When(q.RackID != nil, validation.Required)),
	)
	if err != nil {
		return err
	}
	if q.ScopeType != nil && *q.ScopeType == "Site" && q.RackID != nil {
		return validation.Errors{"rackId": fmt.Errorf("rackId cannot be combined with Site scope")}
	}
	return nil
}

// ToProto builds Flow's binding query from validated filters and pagination.
// A nil page leaves protobuf pagination unset.
func (q APIEventPolicyBindingListRequest) ToProto(page *pagination.PageRequest) *flowv1.ListEventRuleBindingsRequest {
	request := &flowv1.ListEventRuleBindingsRequest{EventType: q.EventType, RackId: q.RackID, Pagination: EventPagination(page)}
	if q.PolicyID != nil {
		request.RuleId = &flowv1.UUID{Id: *q.PolicyID}
	}
	if q.ScopeType != nil {
		if *q.ScopeType == "Site" {
			request.ScopeType = flowv1.EventRuleScopeType_EVENT_RULE_SCOPE_TYPE_SITE
		} else {
			request.ScopeType = flowv1.EventRuleScopeType_EVENT_RULE_SCOPE_TYPE_RACK
		}
	}
	return request
}

// APIEventPolicyBindingQuery selects an explicit assignment for an event type.
// The Rack identifier, when applicable, is supplied in the URL path.
type APIEventPolicyBindingQuery struct {
	SiteID    string `query:"siteId"`
	EventType string `query:"eventType"`
}

// Validate requires a Site and supported event type for the binding slot.
func (q APIEventPolicyBindingQuery) Validate() error {
	return validation.ValidateStruct(&q,
		validation.Field(&q.SiteID, validation.Required, validationis.UUID),
		validation.Field(&q.EventType, validation.Required, validation.In(EventTypeHardwareLeakDetected)),
	)
}

// APIEventHandlingQuery filters handlings by their captured event associations.
type APIEventHandlingQuery struct {
	SiteID     string  `query:"siteId"`
	EventType  *string `query:"eventType"`
	PolicyID   *string `query:"policyId"`
	RackID     *string `query:"rackId"`
	SourceType *string `query:"sourceType"`
	SourceID   *string `query:"sourceId"`
}

// Validate checks optional filters and requires exact source selectors as a pair.
func (q APIEventHandlingQuery) Validate() error {
	err := validation.ValidateStruct(&q,
		validation.Field(&q.SiteID, validation.Required, validationis.UUID),
		validation.Field(&q.EventType, validation.When(q.EventType != nil, validation.Required), validation.In(EventTypeHardwareLeakDetected)),
		validation.Field(&q.PolicyID, validation.When(q.PolicyID != nil, validation.Required), validationis.UUID),
		validation.Field(&q.RackID, validation.When(q.RackID != nil, validation.Required)),
		validation.Field(&q.SourceType, validation.When(q.SourceType != nil, validation.Required), validation.In("Component", "Rack")),
		validation.Field(&q.SourceID, validation.When(q.SourceID != nil, validation.Required)),
	)
	if err != nil {
		return err
	}
	if (q.SourceType == nil) != (q.SourceID == nil) {
		return validation.Errors{"source": fmt.Errorf("sourceType and sourceId must be supplied together")}
	}
	return nil
}

// ToProto builds Flow's handling query from validated filters and pagination.
// A nil page leaves protobuf pagination unset.
func (q APIEventHandlingQuery) ToProto(page *pagination.PageRequest) *flowv1.ListEventHandlingsRequest {
	p := &flowv1.EventHandlingFilter{EventType: q.EventType, RackId: q.RackID, ResourceId: q.SourceID}
	if q.PolicyID != nil {
		p.PolicyId = &flowv1.UUID{Id: *q.PolicyID}
	}
	if q.SourceType != nil {
		if *q.SourceType == "Rack" {
			p.ResourceType = flowv1.EventResourceType_EVENT_RESOURCE_TYPE_RACK
		} else {
			p.ResourceType = flowv1.EventResourceType_EVENT_RESOURCE_TYPE_COMPONENT
		}
	}
	return &flowv1.ListEventHandlingsRequest{Filter: p, Pagination: EventPagination(page)}
}

// APIEventHandlingTaskQuery filters event-triggered Tasks and their active state.
type APIEventHandlingTaskQuery struct {
	SiteID     string  `query:"siteId"`
	EventType  *string `query:"eventType"`
	PolicyID   *string `query:"policyId"`
	RackID     *string `query:"rackId"`
	ActiveOnly bool    `query:"activeOnly"`
}

// Validate checks the captured-event filters supported by the Task list.
func (q APIEventHandlingTaskQuery) Validate() error {
	return validation.ValidateStruct(&q,
		validation.Field(&q.SiteID, validation.Required, validationis.UUID),
		validation.Field(&q.EventType, validation.When(q.EventType != nil, validation.Required), validation.In(EventTypeHardwareLeakDetected)),
		validation.Field(&q.PolicyID, validation.When(q.PolicyID != nil, validation.Required), validationis.UUID),
		validation.Field(&q.RackID, validation.When(q.RackID != nil, validation.Required)),
	)
}

// ToProto builds Flow's Task query from validated filters and pagination.
// A nil page leaves protobuf pagination unset.
func (q APIEventHandlingTaskQuery) ToProto(page *pagination.PageRequest) *flowv1.ListEventTasksRequest {
	p := &flowv1.EventHandlingFilter{EventType: q.EventType, RackId: q.RackID}
	if q.PolicyID != nil {
		p.PolicyId = &flowv1.UUID{Id: *q.PolicyID}
	}
	return &flowv1.ListEventTasksRequest{Filter: p, Pagination: EventPagination(page), ActiveOnly: q.ActiveOnly}
}

// EventPagination converts a validated REST page into Flow's offset-based page.
// A nil page leaves protobuf pagination unset.
func EventPagination(page *pagination.PageRequest) *flowv1.Pagination {
	if page == nil {
		return nil
	}
	return &flowv1.Pagination{Offset: int32(*page.Offset), Limit: int32(*page.Limit)}
}

// APIEventHandlingStatus describes dispatch and Task progress for an event.
type APIEventHandlingStatus string

const (
	APIEventHandlingStatusUnknown   APIEventHandlingStatus = "Unknown"
	APIEventHandlingStatusNoAction  APIEventHandlingStatus = "NoAction"
	APIEventHandlingStatusStarting  APIEventHandlingStatus = "Starting"
	APIEventHandlingStatusRetrying  APIEventHandlingStatus = "Retrying"
	APIEventHandlingStatusSkipped   APIEventHandlingStatus = "Skipped"
	APIEventHandlingStatusRunning   APIEventHandlingStatus = "Running"
	APIEventHandlingStatusCompleted APIEventHandlingStatus = "Completed"
	APIEventHandlingStatusFailed    APIEventHandlingStatus = "Failed"
)

// FromProto maps Flow's authoritative result into event-facing status names.
// Unspecified and unrecognized values map to Unknown without recomputation.
func (s *APIEventHandlingStatus) FromProto(status flowv1.EventHandlingStatus) {
	*s = enumOr(map[flowv1.EventHandlingStatus]APIEventHandlingStatus{
		flowv1.EventHandlingStatus_EVENT_HANDLING_STATUS_NO_ACTION: APIEventHandlingStatusNoAction,
		flowv1.EventHandlingStatus_EVENT_HANDLING_STATUS_STARTING:  APIEventHandlingStatusStarting,
		flowv1.EventHandlingStatus_EVENT_HANDLING_STATUS_RETRYING:  APIEventHandlingStatusRetrying,
		flowv1.EventHandlingStatus_EVENT_HANDLING_STATUS_RUNNING:   APIEventHandlingStatusRunning,
		flowv1.EventHandlingStatus_EVENT_HANDLING_STATUS_COMPLETED: APIEventHandlingStatusCompleted,
		flowv1.EventHandlingStatus_EVENT_HANDLING_STATUS_SKIPPED:   APIEventHandlingStatusSkipped,
		flowv1.EventHandlingStatus_EVENT_HANDLING_STATUS_FAILED:    APIEventHandlingStatusFailed,
	}, status, APIEventHandlingStatusUnknown)
}

// APIEventHandlingReason is Flow's bounded handling diagnostic code.
// Its zero value represents the absence of a recognized reason.
type APIEventHandlingReason string

const (
	APIEventHandlingReasonNoTargets          APIEventHandlingReason = "NoTargets"
	APIEventHandlingReasonAttemptFailed      APIEventHandlingReason = "AttemptFailed"
	APIEventHandlingReasonAttemptInterrupted APIEventHandlingReason = "AttemptInterrupted"
	APIEventHandlingReasonRetryScheduled     APIEventHandlingReason = "RetryScheduled"
	APIEventHandlingReasonActionFailed       APIEventHandlingReason = "ActionFailed"
	APIEventHandlingReasonTaskFailed         APIEventHandlingReason = "TaskFailed"
	APIEventHandlingReasonIncompleteState    APIEventHandlingReason = "IncompleteState"
)

// FromProto clears the receiver for unspecified or unrecognized reasons.
func (r *APIEventHandlingReason) FromProto(reason flowv1.EventHandlingReason) {
	*r = map[flowv1.EventHandlingReason]APIEventHandlingReason{
		flowv1.EventHandlingReason_EVENT_HANDLING_REASON_NO_TARGETS:          APIEventHandlingReasonNoTargets,
		flowv1.EventHandlingReason_EVENT_HANDLING_REASON_ATTEMPT_FAILED:      APIEventHandlingReasonAttemptFailed,
		flowv1.EventHandlingReason_EVENT_HANDLING_REASON_ATTEMPT_INTERRUPTED: APIEventHandlingReasonAttemptInterrupted,
		flowv1.EventHandlingReason_EVENT_HANDLING_REASON_RETRY_SCHEDULED:     APIEventHandlingReasonRetryScheduled,
		flowv1.EventHandlingReason_EVENT_HANDLING_REASON_ACTION_FAILED:       APIEventHandlingReasonActionFailed,
		flowv1.EventHandlingReason_EVENT_HANDLING_REASON_TASK_FAILED:         APIEventHandlingReasonTaskFailed,
		flowv1.EventHandlingReason_EVENT_HANDLING_REASON_INCOMPLETE_STATE:    APIEventHandlingReasonIncompleteState,
	}[reason]
}

// APIEvent identifies the observed source event, without exposing storage identity.
type APIEvent struct {
	EventType  string     `json:"eventType"`
	DetectedAt *time.Time `json:"detectedAt"`
	Summary    string     `json:"summary"`
	SourceID   string     `json:"sourceId"`
	SourceType string     `json:"sourceType"`
}

// APIEventAppliedPolicy is the immutable selected action snapshot.
type APIEventAppliedPolicy struct {
	ID      string          `json:"id"`
	Actions APIEventActions `json:"actions"`
}

// APIEventHandlingTask identifies a created Task and its current public status.
type APIEventHandlingTask struct {
	ID     string `json:"id"`
	Status string `json:"status"`
}

// APIEventHandling reports Flow's authoritative aggregate for an event.
type APIEventHandling struct {
	Event          APIEvent                `json:"event"`
	AppliedPolicy  APIEventAppliedPolicy   `json:"appliedPolicy"`
	Reason         *APIEventHandlingReason `json:"reason"`
	Message        string                  `json:"message"`
	HandlingStatus APIEventHandlingStatus  `json:"handlingStatus"`
	Tasks          []APIEventHandlingTask  `json:"tasks"`
	HandledAt      time.Time               `json:"handledAt"`
}

// FromProto replaces the receiver with the structured Flow representation.
func (h *APIEventHandling) FromProto(p *flowv1.EventHandling) error {
	if p == nil || p.GetAppliedPolicy() == nil {
		return fmt.Errorf("missing captured event policy")
	}
	*h = APIEventHandling{Event: APIEvent{EventType: p.GetEventType(), DetectedAt: timePtrFromProto(p.GetEventFirstObservedAt()), Summary: p.GetEventSummary(), SourceID: p.GetResourceId(), SourceType: map[flowv1.EventResourceType]string{flowv1.EventResourceType_EVENT_RESOURCE_TYPE_COMPONENT: "Component", flowv1.EventResourceType_EVENT_RESOURCE_TYPE_RACK: "Rack"}[p.GetResourceType()]}, Message: p.GetMessage(), Tasks: make([]APIEventHandlingTask, 0, len(p.GetTasks()))}
	h.AppliedPolicy.ID = p.GetAppliedRuleId().GetId()
	err := h.AppliedPolicy.Actions.FromProto(p.GetAppliedPolicy().GetActions())
	if err != nil {
		return err
	}
	h.HandlingStatus.FromProto(p.GetHandlingStatus())
	var reason APIEventHandlingReason
	reason.FromProto(p.GetReason())
	if reason != "" {
		h.Reason = &reason
	}
	for _, task := range p.GetTasks() {
		h.Tasks = append(h.Tasks, APIEventHandlingTask{ID: task.GetId().GetId(), Status: enumOr(ProtoToAPITaskStatusName, task.GetStatus(), "Unknown")})
	}
	if p.GetHandledAt() != nil {
		h.HandledAt = p.GetHandledAt().AsTime().UTC()
	}
	return nil
}
