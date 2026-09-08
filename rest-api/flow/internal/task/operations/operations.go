// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package operations

import (
	"encoding/json"
	"fmt"
	"slices"
	"time"

	"github.com/google/uuid"

	"github.com/NVIDIA/infra-controller/rest-api/flow/internal/secret"
	taskcommon "github.com/NVIDIA/infra-controller/rest-api/flow/internal/task/common"
)

// ExtractRuleID peeks at the "rule_id" field in a serialized operation info
// JSON blob. Returns nil if absent, empty, or unparseable.
func ExtractRuleID(info json.RawMessage) *uuid.UUID {
	var peek struct {
		RuleID string `json:"rule_id"`
	}
	if err := json.Unmarshal(info, &peek); err != nil || peek.RuleID == "" {
		return nil
	}
	parsed, err := uuid.Parse(peek.RuleID)
	if err != nil {
		return nil
	}
	return &parsed
}

type Operation interface {
	Clone() Operation
	// Validate checks the complete operation payload and guarantees that Type
	// and CodeString identify a supported task operation.
	Validate() error
	Marshal() (json.RawMessage, error)
	Unmarshal(data json.RawMessage) error
	Type() taskcommon.TaskType
	Description() string
	CodeString() string // Returns operation code string (e.g., "power_on", "upgrade")
}

func New(typ taskcommon.TaskType, info json.RawMessage) (Operation, error) {
	operation, err := newEmpty(typ)
	if err != nil {
		return nil, err
	}
	if err := json.Unmarshal(info, operation); err != nil {
		return nil, fmt.Errorf("failed to unmarshal %s task info: %w", operationTypeName(typ), err)
	}

	return operation, nil
}

func newEmpty(typ taskcommon.TaskType) (Operation, error) {
	switch typ {
	case taskcommon.TaskTypePowerControl:
		return &PowerControlTaskInfo{}, nil
	case taskcommon.TaskTypeFirmwareControl:
		return &FirmwareControlTaskInfo{}, nil
	case taskcommon.TaskTypeInjectExpectation:
		return &InjectExpectationTaskInfo{}, nil
	case taskcommon.TaskTypeBringUp:
		return &BringUpTaskInfo{}, nil
	case taskcommon.TaskTypeDecommission:
		return &DecommissionTaskInfo{}, nil
	default:
		return nil, fmt.Errorf("unsupported task type: %s", typ)
	}
}

func operationTypeName(typ taskcommon.TaskType) string {
	switch typ {
	case taskcommon.TaskTypePowerControl:
		return "power control"
	case taskcommon.TaskTypeFirmwareControl:
		return "firmware control"
	case taskcommon.TaskTypeInjectExpectation:
		return "inject expectation"
	case taskcommon.TaskTypeBringUp:
		return "bring-up"
	case taskcommon.TaskTypeDecommission:
		return "decommission"
	default:
		return string(typ)
	}
}

type PowerControlTaskInfo struct {
	Operation PowerOperation `json:"operation"`
	Forced    bool           `json:"forced"`
	RuleID    string         `json:"rule_id,omitempty"`
	// OverrideReadinessCheck, when true, instructs the component manager
	// to skip the readiness gate that normally blocks power operations
	// against components whose persisted ComponentOperationStatus reports them as
	// not ready for the operation. The bypass is intended for operator-
	// supervised maintenance windows and is recorded as a warning log on
	// the worker that executes the task; authorisation lives upstream.
	OverrideReadinessCheck bool `json:"override_readiness_check,omitempty"`
}

func (t *PowerControlTaskInfo) Validate() error {
	if t == nil {
		return fmt.Errorf("power control operation is required")
	}
	if _, ok := powerOperationCodes[t.Operation]; !ok {
		return fmt.Errorf("invalid power control operation")
	}
	return validateOperationTypeAndCode(t)
}

// Clone returns an independent copy of the operation.
func (t *PowerControlTaskInfo) Clone() Operation {
	if t == nil {
		return nil
	}
	cloned := *t
	return &cloned
}

func (t *PowerControlTaskInfo) Marshal() (json.RawMessage, error) {
	raw, err := json.Marshal(t)
	if err != nil {
		return nil, fmt.Errorf("failed to marshal power control task info: %w", err)
	}
	return raw, nil
}

func (t *PowerControlTaskInfo) Unmarshal(data json.RawMessage) error {
	if err := json.Unmarshal(data, t); err != nil {
		return fmt.Errorf("failed to unmarshal power control task info: %w", err)
	}
	return nil
}

func (t *PowerControlTaskInfo) Type() taskcommon.TaskType {
	return taskcommon.TaskTypePowerControl
}

func (t *PowerControlTaskInfo) Description() string {
	return fmt.Sprintf("%s, forced %t", t.Operation.String(), t.Forced)
}

func (t *PowerControlTaskInfo) CodeString() string {
	return t.Operation.CodeString()
}

type InjectExpectationTaskInfo struct {
	Info json.RawMessage `json:"info"`
}

func (t *InjectExpectationTaskInfo) Validate() error {
	if t == nil {
		return fmt.Errorf("inject expectation operation is required")
	}
	return validateOperationTypeAndCode(t)
}

// Clone returns an independent copy of the operation.
func (t *InjectExpectationTaskInfo) Clone() Operation {
	if t == nil {
		return nil
	}
	cloned := *t
	cloned.Info = slices.Clone(t.Info)
	return &cloned
}

func (t *InjectExpectationTaskInfo) Marshal() (json.RawMessage, error) {
	raw, err := json.Marshal(t)
	if err != nil {
		return nil, fmt.Errorf("failed to marshal inject expectation task info: %w", err)
	}
	return raw, nil
}

func (t *InjectExpectationTaskInfo) Unmarshal(data json.RawMessage) error {
	if err := json.Unmarshal(data, t); err != nil {
		return fmt.Errorf("failed to unmarshal inject expectation task info: %w", err)
	}
	return nil
}

func (t *InjectExpectationTaskInfo) Type() taskcommon.TaskType {
	return taskcommon.TaskTypeInjectExpectation
}

func (t *InjectExpectationTaskInfo) Description() string {
	return fmt.Sprintf("inject expectation: %s", t.Info)
}

func (t *InjectExpectationTaskInfo) CodeString() string {
	return taskcommon.OpCodeInjectExpectation
}

type BringUpTaskInfo struct {
	RuleID string `json:"rule_id,omitempty"`
	OpCode string `json:"op_code,omitempty"`
	// OverrideReadinessCheck, when true, propagates through the bring-up
	// rule's synthesised PowerControl / FirmwareControl / BringUpControl
	// sub-actions so they each skip the readiness gate. Equivalent to
	// setting the field on every sub-task; intended for operator-
	// supervised maintenance and recorded as a warning log on the worker
	// that executes each sub-task.
	OverrideReadinessCheck bool `json:"override_readiness_check,omitempty"`
}

func (t *BringUpTaskInfo) Validate() error {
	if t == nil {
		return fmt.Errorf("bring-up operation is required")
	}
	return validateOperationTypeAndCode(t)
}

// Clone returns an independent copy of the operation.
func (t *BringUpTaskInfo) Clone() Operation {
	if t == nil {
		return nil
	}
	cloned := *t
	return &cloned
}

func (t *BringUpTaskInfo) Marshal() (json.RawMessage, error) {
	raw, err := json.Marshal(t)
	if err != nil {
		return nil, fmt.Errorf(
			"failed to marshal bring-up task info: %w", err,
		)
	}
	return raw, nil
}

func (t *BringUpTaskInfo) Unmarshal(
	data json.RawMessage,
) error {
	if err := json.Unmarshal(data, t); err != nil {
		return fmt.Errorf(
			"failed to unmarshal bring-up task info: %w", err,
		)
	}
	return nil
}

func (t *BringUpTaskInfo) Type() taskcommon.TaskType {
	return taskcommon.TaskTypeBringUp
}

func (t *BringUpTaskInfo) Description() string {
	switch t.CodeString() {
	case taskcommon.OpCodeIngest:
		return "rack ingest"
	default:
		return "rack bring-up"
	}
}

func (t *BringUpTaskInfo) CodeString() string {
	if t.OpCode != "" {
		return t.OpCode
	}
	return taskcommon.OpCodeBringUp
}

type FirmwareControlTaskInfo struct {
	Operation     FirmwareOperation `json:"operation"`
	TargetVersion string            `json:"target_version,omitempty"`
	StartTime     int64             `json:"start_time,omitempty"` // Unix timestamp
	EndTime       int64             `json:"end_time,omitempty"`   // Unix timestamp
	RuleID        string            `json:"rule_id,omitempty"`
	// SubTargets, when non-empty, restricts the upgrade to a subset of
	// firmware sub-parts within each targeted tray (e.g. ["bmc", "nvos"]
	// for switch trays, ["bmc", "bios"] for compute trays, ["psu"] for
	// powershelf trays). Named "SubTargets" (not "Components") to avoid
	// colliding with the carbide tray-level Component vocabulary used for
	// OperationTargetSpec / ExecutionInfo.Components, which select tray
	// INSTANCES rather than sub-parts of a tray.
	// Names are lowercase and case-sensitive. Empty or nil means "update
	// everything in the bundle" (the historical default). Mapping from
	// these strings to component-manager-specific enums is done in each
	// FirmwareControl implementation (see pkg/common/firmwarecomponents).
	SubTargets []string `json:"sub_targets,omitempty"`
	// OverrideReadinessCheck, when true, instructs the component manager
	// to skip the readiness gate that normally blocks firmware updates
	// against components whose persisted ComponentOperationStatus reports them as
	// not ready for the operation. Intended for operator-supervised
	// maintenance windows and recorded as a warning log on the worker
	// that executes the task; authorisation lives upstream.
	OverrideReadinessCheck bool `json:"override_readiness_check,omitempty"`
	// AuthenticationData remains encrypted while this payload is persisted in
	// Flow or carried by Temporal. The final FirmwareControl activity decrypts
	// it and sets AccessToken only on its in-memory copy.
	AuthenticationData *secret.EncryptedData `json:"authentication_data,omitempty"`
	AccessToken        string                `json:"-"`
}

func (t *FirmwareControlTaskInfo) Validate() error {
	if t == nil {
		return fmt.Errorf("firmware control operation is required")
	}
	if _, ok := firmwareOperationCodes[t.Operation]; !ok {
		return fmt.Errorf("invalid firmware control operation")
	}
	return validateOperationTypeAndCode(t)
}

// Clone returns an independent copy of the operation.
func (t *FirmwareControlTaskInfo) Clone() Operation {
	if t == nil {
		return nil
	}
	cloned := *t
	cloned.SubTargets = slices.Clone(t.SubTargets)
	if t.AuthenticationData != nil {
		authenticationData := *t.AuthenticationData
		authenticationData.Ciphertext = slices.Clone(t.AuthenticationData.Ciphertext)
		cloned.AuthenticationData = &authenticationData
	}
	return &cloned
}

func (t *FirmwareControlTaskInfo) Marshal() (json.RawMessage, error) {
	raw, err := json.Marshal(t)
	if err != nil {
		return nil, fmt.Errorf("failed to marshal firmware control task info: %w", err)
	}
	return raw, nil
}

func (t *FirmwareControlTaskInfo) Unmarshal(data json.RawMessage) error {
	if err := json.Unmarshal(data, t); err != nil {
		return fmt.Errorf("failed to unmarshal firmware control task info: %w", err)
	}
	return nil
}

func (t *FirmwareControlTaskInfo) Type() taskcommon.TaskType {
	return taskcommon.TaskTypeFirmwareControl
}

func (t *FirmwareControlTaskInfo) Description() string {
	return fmt.Sprintf("%s, target version %s", t.Operation.String(), t.TargetVersion)
}

func (t *FirmwareControlTaskInfo) CodeString() string {
	return t.Operation.CodeString()
}

// DecommissionTaskInfo carries parameters for a rack decommission operation.
type DecommissionTaskInfo struct {
	RuleID string `json:"rule_id,omitempty"`
}

func (t *DecommissionTaskInfo) Validate() error {
	if t == nil {
		return fmt.Errorf("decommission operation is required")
	}
	return validateOperationTypeAndCode(t)
}

// Clone returns an independent copy of the operation.
func (t *DecommissionTaskInfo) Clone() Operation {
	if t == nil {
		return nil
	}
	cloned := *t
	return &cloned
}

func (t *DecommissionTaskInfo) Marshal() (json.RawMessage, error) {
	raw, err := json.Marshal(t)
	if err != nil {
		return nil, fmt.Errorf("failed to marshal decommission task info: %w", err)
	}
	return raw, nil
}

func (t *DecommissionTaskInfo) Unmarshal(data json.RawMessage) error {
	if err := json.Unmarshal(data, t); err != nil {
		return fmt.Errorf("failed to unmarshal decommission task info: %w", err)
	}
	return nil
}

func (t *DecommissionTaskInfo) Type() taskcommon.TaskType {
	return taskcommon.TaskTypeDecommission
}

func (t *DecommissionTaskInfo) Description() string {
	return "rack decommission"
}

func (t *DecommissionTaskInfo) CodeString() string {
	return taskcommon.OpCodeDecommission
}

func validateOperationTypeAndCode(operation Operation) error {
	taskType := operation.Type()
	if !taskType.IsValid() {
		return fmt.Errorf("task operation type %q is invalid", taskType)
	}
	return taskcommon.OperationCode(operation.CodeString()).ValidateFor(taskType)
}

// SetFirmwareUpdateTimeWindowRequest is the request for setting firmware update time window.
// ComponentIDs are external IDs that identify the components.
type SetFirmwareUpdateTimeWindowRequest struct {
	ComponentIDs []string
	StartTime    time.Time
	EndTime      time.Time
}

// FirmwareUpdateState represents the state of a firmware update operation.
type FirmwareUpdateState int

const (
	FirmwareUpdateStateUnknown   FirmwareUpdateState = 0
	FirmwareUpdateStateQueued    FirmwareUpdateState = 1
	FirmwareUpdateStateVerifying FirmwareUpdateState = 2
	FirmwareUpdateStateCompleted FirmwareUpdateState = 3
	FirmwareUpdateStateFailed    FirmwareUpdateState = 4
)

func (s FirmwareUpdateState) String() string {
	switch s {
	case FirmwareUpdateStateQueued:
		return "Queued"
	case FirmwareUpdateStateVerifying:
		return "Verifying"
	case FirmwareUpdateStateCompleted:
		return "Completed"
	case FirmwareUpdateStateFailed:
		return "Failed"
	default:
		return "Unknown"
	}
}

// IsTerminal returns true if this state is a terminal state (completed or failed).
func (s FirmwareUpdateState) IsTerminal() bool {
	return s == FirmwareUpdateStateCompleted || s == FirmwareUpdateStateFailed
}

// FirmwareUpdateStatus contains the status of a firmware update operation.
type FirmwareUpdateStatus struct {
	ComponentID string
	State       FirmwareUpdateState
	Error       string
}
