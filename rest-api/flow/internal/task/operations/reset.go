// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package operations

import (
	"encoding/json"
	"fmt"
	taskcommon "github.com/NVIDIA/infra-controller/rest-api/flow/internal/task/common"
)

// ComponentResetTaskInfo is the persisted operation payload. NVOS reset is a
// fixed, single-attempt operation; custom rules and readiness bypasses are absent.
type ComponentResetTaskInfo struct{}

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
func (*ComponentResetTaskInfo) Description() string       { return "Restore NVOS factory defaults" }
func (*ComponentResetTaskInfo) CodeString() string        { return taskcommon.OpCodeNVOSFactoryReset }
