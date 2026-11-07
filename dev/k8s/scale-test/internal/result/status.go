// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package result

// Status is the small record published to the run's result ConfigMap: the verdict, why it ended, and a reference to
// the authoritative result.json on the PVC, which holds the uncapped stage, invariant, journal and clock detail.
type Status struct {
	SchemaVersion uint32    `json:"schemaVersion"`
	RunID         string    `json:"runId"`
	ExecutionID   string    `json:"executionId"`
	Outcome                 // verdict and exitCode, flattened
	Reasons       []string  `json:"reasons"`
	Result        ResultRef `json:"result"`
}

// StatusOf is the ConfigMap status of full. Reasons is a list, never null.
func StatusOf(full *RunResult) Status {
	reasons := full.Reasons
	if reasons == nil {
		reasons = []string{}
	}
	return Status{
		SchemaVersion: full.SchemaVersion, RunID: full.RunID, ExecutionID: full.ExecutionID,
		Outcome: full.Outcome, Reasons: reasons, Result: full.Result,
	}
}
