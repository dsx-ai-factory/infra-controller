// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

//go:build acceptance

package history_test

import (
	"crypto/sha256"
	"encoding/hex"
	"os"
	"testing"
)

// TestAcceptanceReplaysARealProbeCapture replays a real collector capture from
// SCALE_TEST_HISTORY_CAPTURE and fails when unset, so the synthetic fixture is
// never passed off as real evidence. It logs the capture's hash and the
// provenance of the probe that produced it. See ACCEPTANCE.md.
func TestAcceptanceReplaysARealProbeCapture(t *testing.T) {
	path := os.Getenv("SCALE_TEST_HISTORY_CAPTURE")
	if path == "" {
		t.Fatal("SCALE_TEST_HISTORY_CAPTURE must point at a real probe capture file; see ACCEPTANCE.md")
	}
	data, err := loadCapture(path, MaxCaptureBytes)
	if err != nil {
		t.Fatal(err)
	}
	sum := sha256.Sum256(data)
	t.Logf("capture=%s sha256=%s captureProbeImage=%q captureProbeRevision=%q", path, hex.EncodeToString(sum[:]),
		os.Getenv("SCALE_TEST_CAPTURE_PROBE_IMAGE"), os.Getenv("SCALE_TEST_CAPTURE_PROBE_REVISION"))
	replayCapture(t, data)
}
