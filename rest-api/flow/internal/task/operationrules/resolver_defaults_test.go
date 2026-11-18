// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package operationrules

import (
	"testing"

	"github.com/stretchr/testify/assert"

	"github.com/NVIDIA/infra-controller/rest-api/flow/internal/task/common"
)

func TestGetHardcodedDefaultRule_FirmwarePollTimeout(t *testing.T) {
	for _, tc := range []struct {
		name          string
		operationType common.TaskType
		operation     string
		firmwareSteps int
	}{
		{"firmware upgrade", common.TaskTypeFirmwareControl, SequenceUpgrade, 2},
		{"bring up", common.TaskTypeBringUp, SequenceBringUp, 1},
	} {
		t.Run(tc.name, func(t *testing.T) {
			rule := getHardcodedDefaultRule(tc.operationType, tc.operation)
			firmwareSteps := 0
			for _, step := range rule.RuleDefinition.Steps {
				if step.MainOperation.Name != ActionFirmwareControl {
					continue
				}
				firmwareSteps++
				assert.Equal(t, "60m", step.MainOperation.Parameters[ParamPollTimeout])
			}
			assert.Equal(t, tc.firmwareSteps, firmwareSteps)
		})
	}
}
