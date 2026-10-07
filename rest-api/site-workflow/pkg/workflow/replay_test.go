// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package workflow

import (
	"os"
	"path/filepath"
	"testing"

	"github.com/NVIDIA/infra-controller/rest-api/site-workflow/pkg/util"
	"github.com/stretchr/testify/require"
	"go.temporal.io/sdk/worker"
)

func TestExpectedInventoryPreCutoverHistoriesReplay(t *testing.T) {
	tests := []struct {
		name           string
		workflow       any
		history        string
		legacyActivity string
	}{
		{
			name:           "create expected machine",
			workflow:       CreateExpectedMachine,
			history:        "pre_cutover_create_expected_machine.json",
			legacyActivity: "CreateExpectedMachineOnFlow",
		},
		{
			name:           "batch create expected machines",
			workflow:       CreateExpectedMachines,
			history:        "pre_cutover_create_expected_machines.json",
			legacyActivity: "CreateExpectedMachinesOnFlow",
		},
		{
			name:           "create expected power shelf",
			workflow:       CreateExpectedPowerShelf,
			history:        "pre_cutover_create_expected_power_shelf.json",
			legacyActivity: "CreateExpectedPowerShelfOnFlow",
		},
		{
			name:           "create expected rack",
			workflow:       CreateExpectedRack,
			history:        "pre_cutover_create_expected_rack.json",
			legacyActivity: "CreateExpectedRackOnFlow",
		},
		{
			name:           "create expected switch",
			workflow:       CreateExpectedSwitch,
			history:        "pre_cutover_create_expected_switch.json",
			legacyActivity: "CreateExpectedSwitchOnFlow",
		},
	}

	for _, tc := range tests {
		t.Run(tc.name, func(t *testing.T) {
			historyPath := filepath.Join("testdata", tc.history)
			history, err := os.ReadFile(historyPath)
			require.NoError(t, err)
			require.Contains(t, string(history), tc.legacyActivity)

			replayer, err := worker.NewWorkflowReplayerWithOptions(worker.WorkflowReplayerOptions{
				DataConverter: util.NewTemporalDataConverter(),
			})
			require.NoError(t, err)
			replayer.RegisterWorkflow(tc.workflow)

			// These histories came from `ed8afcec818002b7a1603f9a365f9d3e13c5fe26`,
			// the parent of the #4328 merge commit on `main`.
			// Each one includes the old `OnFlow` activity in an
			// `EVENT_TYPE_ACTIVITY_TASK_SCHEDULED` event, so replay fails if the
			// compatibility branch stops scheduling it.
			require.NoError(t, replayer.ReplayWorkflowHistoryFromJSONFile(
				nil,
				historyPath,
			))
		})
	}
}
