// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package carbide

import "testing"

func TestNameDerivation(t *testing.T) {
	// Mirrors crates/dpf/src/sdk.rs comment: DPU CR name is
	// node-{node_id}-device-{device_id}.
	dpfID := DPFIDFromBMCMAC("02:00:00:00:00:01")
	if dpfID != "02-00-00-00-00-01" {
		t.Fatalf("dpfID = %q", dpfID)
	}
	if got := DPUNodeName(dpfID); got != "node-02-00-00-00-00-01" {
		t.Errorf("DPUNodeName = %q", got)
	}
	if got := DPUName(dpfID, "dpu0"); got != "node-02-00-00-00-00-01-device-dpu0" {
		t.Errorf("DPUName = %q", got)
	}
	if got := NodeIDFromNodeCRName("node-02-00-00-00-00-01"); got != "02-00-00-00-00-01" {
		t.Errorf("NodeIDFromNodeCRName = %q", got)
	}
}
