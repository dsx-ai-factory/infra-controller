// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package activity

import (
	"github.com/google/uuid"
	"github.com/stretchr/testify/require"
	"testing"
)

func TestDispatchTaskID(t *testing.T) {
	id := uuid.New()
	for _, tc := range []struct {
		name, workflowID string
		wantErr          bool
	}{
		{name: "root predecessor run", workflowID: id.String()},
		{name: "component child predecessor run", workflowID: "component-step-" + id.String() + "-Compute"},
		{name: "invalid root", workflowID: "unknown", wantErr: true},
		{name: "invalid child", workflowID: "component-step-" + id.String(), wantErr: true},
	} {
		t.Run(tc.name, func(t *testing.T) {
			got, err := dispatchTaskID(tc.workflowID)
			if tc.wantErr {
				require.Error(t, err)
			} else {
				require.NoError(t, err)
				require.Equal(t, id, got)
			}
		})
	}
}
