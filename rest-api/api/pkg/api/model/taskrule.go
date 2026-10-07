// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package model

import (
	"encoding/json"
	"fmt"
	"net/url"
	"strconv"
	"time"

	validation "github.com/go-ozzo/ozzo-validation/v4"

	"github.com/NVIDIA/infra-controller/rest-api/api/pkg/api/pagination"
	flowv1 "github.com/NVIDIA/infra-controller/rest-api/proto/flow/gen/v1"
)

// APIOperationType is the operationType field of an Operation Rule.
type APIOperationType string

const (
	APIOperationTypePowerControl    APIOperationType = "PowerControl"
	APIOperationTypeFirmwareControl APIOperationType = "FirmwareControl"
)

// validOperationTypes lists the supported APIOperationType values.
var validOperationTypes = []APIOperationType{
	APIOperationTypePowerControl,
	APIOperationTypeFirmwareControl,
}

var validOperationTypesAny = func() []any {
	out := make([]any, len(validOperationTypes))
	for i, t := range validOperationTypes {
		out[i] = t
	}
	return out
}()

var protoToAPIOperationType = map[flowv1.OperationType]APIOperationType{
	flowv1.OperationType_OPERATION_TYPE_POWER_CONTROL:    APIOperationTypePowerControl,
	flowv1.OperationType_OPERATION_TYPE_FIRMWARE_CONTROL: APIOperationTypeFirmwareControl,
}

var apiToProtoOperationType = map[APIOperationType]flowv1.OperationType{
	APIOperationTypePowerControl:    flowv1.OperationType_OPERATION_TYPE_POWER_CONTROL,
	APIOperationTypeFirmwareControl: flowv1.OperationType_OPERATION_TYPE_FIRMWARE_CONTROL,
}

// ToProto converts to Flow's protobuf OperationType. The empty value maps
// to OPERATION_TYPE_UNKNOWN; any unrecognized value returns an error.
func (aot APIOperationType) ToProto() (flowv1.OperationType, error) {
	if aot == "" {
		return flowv1.OperationType_OPERATION_TYPE_UNKNOWN, nil
	}
	v, ok := apiToProtoOperationType[aot]
	if !ok {
		return flowv1.OperationType_OPERATION_TYPE_UNKNOWN,
			fmt.Errorf("invalid operationType %q (expected one of %v)", aot, validOperationTypes)
	}
	return v, nil
}

// proto* types mirror their APITaskRule* counterparts with snake_case JSON
// tags for (de)serializing Flow's rule_definition_json blob. Keep each in
// lock-step with its API counterpart when adding fields.
type protoRuleDefinition struct {
	Version string              `json:"version"`
	Steps   []protoSequenceStep `json:"steps,omitempty"`
}

type protoSequenceStep struct {
	ComponentType string              `json:"component_type"`
	Stage         int                 `json:"stage"`
	MaxParallel   int                 `json:"max_parallel"`
	Timeout       string              `json:"timeout,omitempty"`
	Retry         *protoRetryPolicy   `json:"retry,omitempty"`
	PreOperation  []protoActionConfig `json:"pre_operation,omitempty"`
	MainOperation protoActionConfig   `json:"main_operation"`
	PostOperation []protoActionConfig `json:"post_operation,omitempty"`
	DelayAfter    string              `json:"delay_after,omitempty"`
}

type protoActionConfig struct {
	Name         string         `json:"name"`
	Timeout      string         `json:"timeout,omitempty"`
	PollInterval string         `json:"poll_interval,omitempty"`
	Parameters   map[string]any `json:"parameters,omitempty"`
}

type protoRetryPolicy struct {
	MaxAttempts        int     `json:"max_attempts"`
	InitialInterval    string  `json:"initial_interval"`
	BackoffCoefficient float64 `json:"backoff_coefficient"`
	MaxInterval        string  `json:"max_interval,omitempty"`
}

// APITaskRule is the API response model for an Operation Rule.
type APITaskRule struct {
	ID             string                `json:"id"`
	Name           string                `json:"name"`
	Description    string                `json:"description"`
	OperationType  APIOperationType      `json:"operationType"`
	OperationCode  string                `json:"operationCode"`
	RuleDefinition APITaskRuleDefinition `json:"ruleDefinition"`
	IsDefault      bool                  `json:"isDefault"`
	Created        time.Time             `json:"created"`
	Updated        time.Time             `json:"updated"`
}

// FromProto populates an APITaskRule from a Flow protobuf OperationRule.
// Returns an error if ruleDefinitionJson cannot be unmarshaled.
func (atr *APITaskRule) FromProto(pbRule *flowv1.OperationRule) error {
	if pbRule == nil {
		return nil
	}
	if pbRule.GetId() != nil {
		atr.ID = pbRule.GetId().GetId()
	}
	atr.Name = pbRule.GetName()
	atr.Description = pbRule.GetDescription()
	atr.OperationType = enumOr(protoToAPIOperationType, pbRule.GetOperationType(), "")
	atr.OperationCode = pbRule.GetOperationCode()
	atr.IsDefault = pbRule.GetIsDefault()
	if ts := pbRule.GetCreatedAt(); ts != nil {
		atr.Created = ts.AsTime().UTC()
	}
	if ts := pbRule.GetUpdatedAt(); ts != nil {
		atr.Updated = ts.AsTime().UTC()
	}

	// Decode into a local so a malformed blob leaves RuleDefinition untouched.
	if raw := pbRule.GetRuleDefinitionJson(); raw != "" {
		var def APITaskRuleDefinition
		if err := def.fromFlowJSON(raw); err != nil {
			return err
		}
		atr.RuleDefinition = def
	}
	return nil
}

// APITaskRuleDefinition is the executable body of a rule.
type APITaskRuleDefinition struct {
	Version string                    `json:"version"`
	Steps   []APITaskRuleSequenceStep `json:"steps"`
}

// ToProto converts the definition into its snake_case mirror.
func (atrd APITaskRuleDefinition) ToProto() protoRuleDefinition {
	out := protoRuleDefinition{Version: atrd.Version}
	if atrd.Steps != nil {
		out.Steps = make([]protoSequenceStep, len(atrd.Steps))
		for i, s := range atrd.Steps {
			out.Steps[i] = s.ToProto()
		}
	}
	return out
}

// FromProto populates the definition from its snake_case mirror.
func (atrd *APITaskRuleDefinition) FromProto(p protoRuleDefinition) {
	atrd.Version = p.Version
	if p.Steps != nil {
		atrd.Steps = make([]APITaskRuleSequenceStep, len(p.Steps))
		for i, s := range p.Steps {
			atrd.Steps[i].FromProto(s)
		}
	}
}

// toFlowJSON encodes the rule definition into Flow's rule_definition_json
// blob (snake_case JSON).
func (atrd APITaskRuleDefinition) toFlowJSON() (string, error) {
	raw, err := json.Marshal(atrd.ToProto())
	if err != nil {
		return "", fmt.Errorf("failed to encode ruleDefinition: %w", err)
	}
	return string(raw), nil
}

// fromFlowJSON decodes Flow's rule_definition_json blob into the definition.
func (atrd *APITaskRuleDefinition) fromFlowJSON(raw string) error {
	var p protoRuleDefinition
	if err := json.Unmarshal([]byte(raw), &p); err != nil {
		return fmt.Errorf("invalid ruleDefinition from Flow: %w", err)
	}
	atrd.FromProto(p)
	return nil
}

// APITaskRuleSequenceStep describes one stage of execution. Duration fields
// (Timeout, DelayAfter) are Go duration strings (e.g. "30s", "2m") parsed by
// Flow.
type APITaskRuleSequenceStep struct {
	ComponentType string                    `json:"componentType"`
	Stage         int                       `json:"stage"`
	MaxParallel   int                       `json:"maxParallel"`
	Timeout       string                    `json:"timeout"`
	Retry         *APITaskRuleRetryPolicy   `json:"retry"`
	PreOperation  []APITaskRuleActionConfig `json:"preOperation"`
	MainOperation APITaskRuleActionConfig   `json:"mainOperation"`
	PostOperation []APITaskRuleActionConfig `json:"postOperation"`
	DelayAfter    string                    `json:"delayAfter"`
}

// ToProto converts the step into its snake_case mirror.
func (atrss APITaskRuleSequenceStep) ToProto() protoSequenceStep {
	out := protoSequenceStep{
		ComponentType: atrss.ComponentType,
		Stage:         atrss.Stage,
		MaxParallel:   atrss.MaxParallel,
		Timeout:       atrss.Timeout,
		MainOperation: atrss.MainOperation.ToProto(),
		DelayAfter:    atrss.DelayAfter,
	}
	if atrss.Retry != nil {
		p := atrss.Retry.ToProto()
		out.Retry = &p
	}
	if atrss.PreOperation != nil {
		out.PreOperation = make([]protoActionConfig, len(atrss.PreOperation))
		for i, a := range atrss.PreOperation {
			out.PreOperation[i] = a.ToProto()
		}
	}
	if atrss.PostOperation != nil {
		out.PostOperation = make([]protoActionConfig, len(atrss.PostOperation))
		for i, a := range atrss.PostOperation {
			out.PostOperation[i] = a.ToProto()
		}
	}
	return out
}

// FromProto populates the step from its snake_case mirror.
func (atrss *APITaskRuleSequenceStep) FromProto(p protoSequenceStep) {
	atrss.ComponentType = p.ComponentType
	atrss.Stage = p.Stage
	atrss.MaxParallel = p.MaxParallel
	atrss.Timeout = p.Timeout
	atrss.MainOperation.FromProto(p.MainOperation)
	atrss.DelayAfter = p.DelayAfter
	if p.Retry != nil {
		var rp APITaskRuleRetryPolicy
		rp.FromProto(*p.Retry)
		atrss.Retry = &rp
	}
	if p.PreOperation != nil {
		atrss.PreOperation = make([]APITaskRuleActionConfig, len(p.PreOperation))
		for i, a := range p.PreOperation {
			atrss.PreOperation[i].FromProto(a)
		}
	}
	if p.PostOperation != nil {
		atrss.PostOperation = make([]APITaskRuleActionConfig, len(p.PostOperation))
		for i, a := range p.PostOperation {
			atrss.PostOperation[i].FromProto(a)
		}
	}
}

// APITaskRuleActionConfig configures a single action within a step.
// Parameters is action-specific and passes through to Flow unchanged.
type APITaskRuleActionConfig struct {
	Name         string         `json:"name"`
	Timeout      string         `json:"timeout"`
	PollInterval string         `json:"pollInterval"`
	Parameters   map[string]any `json:"parameters"`
}

// ToProto converts the action config into its snake_case mirror.
func (atrac APITaskRuleActionConfig) ToProto() protoActionConfig {
	return protoActionConfig{
		Name:         atrac.Name,
		Timeout:      atrac.Timeout,
		PollInterval: atrac.PollInterval,
		Parameters:   atrac.Parameters,
	}
}

// FromProto populates the action config from its snake_case mirror.
func (atrac *APITaskRuleActionConfig) FromProto(p protoActionConfig) {
	atrac.Name = p.Name
	atrac.Timeout = p.Timeout
	atrac.PollInterval = p.PollInterval
	atrac.Parameters = p.Parameters
}

// APITaskRuleRetryPolicy describes retry behavior for a step's workflow.
type APITaskRuleRetryPolicy struct {
	MaxAttempts        int     `json:"maxAttempts"`
	InitialInterval    string  `json:"initialInterval"`
	BackoffCoefficient float64 `json:"backoffCoefficient"`
	MaxInterval        string  `json:"maxInterval"`
}

// ToProto converts the retry policy into its snake_case mirror.
func (atrrp APITaskRuleRetryPolicy) ToProto() protoRetryPolicy {
	return protoRetryPolicy{
		MaxAttempts:        atrrp.MaxAttempts,
		InitialInterval:    atrrp.InitialInterval,
		BackoffCoefficient: atrrp.BackoffCoefficient,
		MaxInterval:        atrrp.MaxInterval,
	}
}

// FromProto populates the retry policy from its snake_case mirror.
func (atrrp *APITaskRuleRetryPolicy) FromProto(p protoRetryPolicy) {
	atrrp.MaxAttempts = p.MaxAttempts
	atrrp.InitialInterval = p.InitialInterval
	atrrp.BackoffCoefficient = p.BackoffCoefficient
	atrrp.MaxInterval = p.MaxInterval
}

// ~~~~~ Create ~~~~~ //

// APITaskRuleCreateRequest is the JSON body for POST /rule. isDefault is
// not accepted — rules are created non-default; promotion uses Flow's
// SetRuleAsDefault RPC, which is not surfaced through this CRUD API.
type APITaskRuleCreateRequest struct {
	SiteID         string                `json:"siteId"`
	Name           string                `json:"name"`
	Description    string                `json:"description"`
	OperationType  APIOperationType      `json:"operationType"`
	OperationCode  string                `json:"operationCode"`
	RuleDefinition APITaskRuleDefinition `json:"ruleDefinition"`
}

// Validate enforces shape only; semantic checks (operation code membership,
// rule definition correctness) are performed by Flow.
func (atrcr *APITaskRuleCreateRequest) Validate() error {
	return validation.ValidateStruct(atrcr,
		validation.Field(&atrcr.SiteID, validation.Required.Error("siteId is required")),
		validation.Field(&atrcr.Name, validation.Required.Error("name is required")),
		validation.Field(&atrcr.OperationType,
			validation.Required.Error("operationType is required"),
			validation.In(validOperationTypesAny...).Error(
				fmt.Sprintf("operationType must be one of %v", validOperationTypes))),
		validation.Field(&atrcr.OperationCode, validation.Required.Error("operationCode is required")),
	)
}

// ToProto converts the request into the Flow CreateOperationRuleRequest.
func (atrcr *APITaskRuleCreateRequest) ToProto() (*flowv1.CreateOperationRuleRequest, error) {
	opType, err := atrcr.OperationType.ToProto()
	if err != nil {
		return nil, err
	}
	rdJSON, err := atrcr.RuleDefinition.toFlowJSON()
	if err != nil {
		return nil, err
	}
	return &flowv1.CreateOperationRuleRequest{
		Name:               atrcr.Name,
		Description:        atrcr.Description,
		OperationType:      opType,
		OperationCode:      atrcr.OperationCode,
		RuleDefinitionJson: rdJSON,
	}, nil
}

// ~~~~~ Update ~~~~~ //

// APITaskRuleUpdateRequest is the JSON body for PATCH /rule/{id}. Nil
// pointer fields mean "leave unchanged". operationType, operationCode, and
// isDefault are immutable after creation and not exposed here.
type APITaskRuleUpdateRequest struct {
	SiteID         string                 `json:"siteId"`
	Name           *string                `json:"name"`
	Description    *string                `json:"description"`
	RuleDefinition *APITaskRuleDefinition `json:"ruleDefinition"`
}

// Validate enforces that the request carries at least one mutable field.
func (atrur *APITaskRuleUpdateRequest) Validate() error {
	err := validation.ValidateStruct(atrur,
		validation.Field(&atrur.SiteID, validation.Required.Error("siteId is required")),
		validation.Field(&atrur.Name,
			validation.When(atrur.Name != nil,
				validation.Required.Error("name cannot be empty when provided"))),
	)
	if err != nil {
		return err
	}
	if atrur.Name == nil && atrur.Description == nil && atrur.RuleDefinition == nil {
		return fmt.Errorf("at least one of name, description, ruleDefinition must be provided")
	}
	return nil
}

// ToProto converts the update request into the Flow UpdateOperationRuleRequest.
func (atrur *APITaskRuleUpdateRequest) ToProto(ruleID string) (*flowv1.UpdateOperationRuleRequest, error) {
	req := &flowv1.UpdateOperationRuleRequest{
		RuleId:      &flowv1.UUID{Id: ruleID},
		Name:        atrur.Name,
		Description: atrur.Description,
	}
	if atrur.RuleDefinition != nil {
		rdJSON, err := atrur.RuleDefinition.toFlowJSON()
		if err != nil {
			return nil, err
		}
		req.RuleDefinitionJson = &rdJSON
	}
	return req, nil
}

// ~~~~~ Get / Delete (siteId via query) ~~~~~ //

// APITaskRuleGetRequest captures query parameters for GET /rule/{id}.
type APITaskRuleGetRequest struct {
	SiteID string `query:"siteId"`
}

func (atrgr *APITaskRuleGetRequest) Validate() error {
	return validation.ValidateStruct(atrgr,
		validation.Field(&atrgr.SiteID, validation.Required.Error("siteId query parameter is required")),
	)
}

// APITaskRuleDeleteRequest captures query parameters for DELETE /rule/{id}.
type APITaskRuleDeleteRequest struct {
	SiteID string `query:"siteId"`
}

func (atrdr *APITaskRuleDeleteRequest) Validate() error {
	return validation.ValidateStruct(atrdr,
		validation.Field(&atrdr.SiteID, validation.Required.Error("siteId query parameter is required")),
	)
}

// ~~~~~ List ~~~~~ //

// APITaskRuleGetAllRequest binds query parameters for GET /rule. Pagination is
// bound separately via pagination.PageRequest.
type APITaskRuleGetAllRequest struct {
	SiteID        string           `query:"siteId"`
	OperationType APIOperationType `query:"operationType"`
}

func (atrgar *APITaskRuleGetAllRequest) Validate() error {
	return validation.ValidateStruct(atrgar,
		validation.Field(&atrgar.SiteID, validation.Required.Error("siteId query parameter is required")),
		validation.Field(&atrgar.OperationType,
			validation.When(atrgar.OperationType != "",
				validation.In(validOperationTypesAny...).Error(
					fmt.Sprintf("operationType must be one of %v", validOperationTypes)))),
	)
}

// ToProto converts the list filters into the Flow ListOperationRulesRequest.
// Returns an error if operationType is invalid.
func (atrgar *APITaskRuleGetAllRequest) ToProto(page pagination.PageRequest) (*flowv1.ListOperationRulesRequest, error) {
	req := &flowv1.ListOperationRulesRequest{}
	if atrgar.OperationType != "" {
		opType, err := atrgar.OperationType.ToProto()
		if err != nil {
			return nil, err
		}
		req.OperationType = &opType
	}
	if page.PageSize != nil && *page.PageSize > 0 {
		limit := int32(*page.PageSize)
		req.Limit = &limit
	}
	// Flow uses offset-based pagination; translate (pageNumber, pageSize).
	if page.PageNumber != nil && page.PageSize != nil && *page.PageNumber > 0 && *page.PageSize > 0 {
		offset := int32((*page.PageNumber - 1) * (*page.PageSize))
		req.Offset = &offset
	}
	return req, nil
}

// QueryValues returns the request fields that feed the workflow ID hash,
// including pagination so different pages map to distinct workflow IDs.
func (atrgar *APITaskRuleGetAllRequest) QueryValues(page pagination.PageRequest) url.Values {
	v := url.Values{}
	v.Set("siteId", atrgar.SiteID)
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
