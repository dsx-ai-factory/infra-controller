// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package service

import (
	"testing"

	"github.com/stretchr/testify/require"

	eventingestion "github.com/NVIDIA/infra-controller/rest-api/flow/internal/eventrule/ingestion"
)

func TestLeakDetectionPipelineFromEnv(t *testing.T) {
	pipeline := &eventingestion.Pipeline{}

	tests := map[string]struct {
		raw         string
		pipeline    *eventingestion.Pipeline
		wantEnabled bool
		wantErr     string
	}{
		"unset defaults to legacy": {
			pipeline: pipeline,
		},
		"false selects legacy": {
			raw:      "false",
			pipeline: pipeline,
		},
		"true selects event rule": {
			raw:         "true",
			pipeline:    pipeline,
			wantEnabled: true,
		},
		"accepted boolean selects event rule": {
			raw:         "1",
			pipeline:    pipeline,
			wantEnabled: true,
		},
		"invalid value is rejected": {
			raw:      "enabled",
			pipeline: pipeline,
			wantErr:  "must be a boolean",
		},
		"enabled requires initialized pipeline": {
			raw:     "true",
			wantErr: "event ingestion pipeline is not initialized",
		},
	}

	for name, test := range tests {
		t.Run(name, func(t *testing.T) {
			t.Setenv(eventRuleLeakDetectionEnabledEnvVar, test.raw)

			actual, err := leakDetectionPipelineFromEnv(test.pipeline)
			if test.wantErr != "" {
				require.ErrorContains(t, err, test.wantErr)
				require.Nil(t, actual)

				return
			}

			require.NoError(t, err)
			if test.wantEnabled {
				require.Same(t, test.pipeline, actual)

				return
			}

			require.Nil(t, actual)
		})
	}
}
