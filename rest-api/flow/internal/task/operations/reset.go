// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package operations

import (
	"encoding/json"
	"fmt"
	"strings"

	taskcommon "github.com/NVIDIA/infra-controller/rest-api/flow/internal/task/common"
)

// ComponentResetTaskInfo persists the reset scope. Each reset is a
// fixed, single-attempt operation; custom rules and readiness bypasses are absent.
type ComponentResetTaskInfo struct {
	Target string `json:"target"`
}

// ResetTarget preserves NVOS semantics for task payloads created before the
// target field existed. New submissions persist their explicit effective scope.
func (t *ComponentResetTaskInfo) ResetTarget() string {
	if t.Target == "" {
		return "nvos"
	}
	return t.Target
}

func (t *ComponentResetTaskInfo) Validate() error {
	if t == nil {
		return fmt.Errorf("component reset operation is required")
	}
	return validateOperationTypeAndCode(t)
}
func (t *ComponentResetTaskInfo) Clone() Operation {
	if t == nil {
		return nil
	}
	cloned := *t
	return &cloned
}
func (t *ComponentResetTaskInfo) Marshal() (json.RawMessage, error) { return json.Marshal(t) }
func (t *ComponentResetTaskInfo) Unmarshal(data json.RawMessage) error {
	return json.Unmarshal(data, t)
}
func (*ComponentResetTaskInfo) Type() taskcommon.TaskType { return taskcommon.TaskTypeComponentReset }
func (t *ComponentResetTaskInfo) Description() string {
	return "Restore " + strings.ToUpper(t.ResetTarget()) + " factory defaults"
}
func (t *ComponentResetTaskInfo) CodeString() string {
	switch t.ResetTarget() {
	case "nvos":
		return taskcommon.OpCodeNVOSFactoryReset
	case "sdn":
		return taskcommon.OpCodeSDNFactoryReset
	default:
		return ""
	}
}
