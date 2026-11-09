// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package leakage

import (
	"testing"

	"github.com/NVIDIA/infra-controller/rest-api/flow/internal/operation"
	"github.com/NVIDIA/infra-controller/rest-api/flow/internal/task/operations"
	"github.com/stretchr/testify/require"
)

func TestPreserveShutdownTask(t *testing.T) {
	for _, tc := range []struct {
		name     string
		op       operations.PowerOperation
		override bool
		want     bool
	}{
		{name: "direct force off retains accepted coverage", op: operations.PowerOperationForcePowerOff, override: true, want: true},
		{name: "power on can be displaced", op: operations.PowerOperationPowerOn, override: true},
		{name: "readiness-blocked force off can be displaced", op: operations.PowerOperationForcePowerOff},
	} {
		t.Run(tc.name, func(t *testing.T) {
			info := operations.PowerControlTaskInfo{Operation: tc.op, OverrideReadinessCheck: tc.override}
			raw, err := info.Marshal()
			require.NoError(t, err)
			require.Equal(t, tc.want, PreserveShutdownTask(operation.Wrapper{Type: info.Type(), Code: info.CodeString(), Info: raw}))
		})
	}
}
