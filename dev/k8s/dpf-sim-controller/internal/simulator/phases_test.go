// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package simulator

import (
	"testing"

	provisioningv1 "github.com/nvidia/doca-platform/api/provisioning/v1alpha1"
)

// TestHappyPathWalksToReady verifies the walker reaches Ready from
// Initializing in exactly len(HappyPath)-1 steps with no cycles.
func TestHappyPathWalksToReady(t *testing.T) {
	cur := provisioningv1.DPUInitializing
	for steps := 0; !IsTerminal(cur); steps++ {
		if steps > len(HappyPath) {
			t.Fatalf("walker did not terminate; stuck at %q after %d steps", cur, steps)
		}
		next, ok := Next(cur)
		if !ok {
			t.Fatalf("Next(%q) returned ok=false before reaching a terminal phase", cur)
		}
		cur = next
	}
	if cur != provisioningv1.DPUReady {
		t.Fatalf("walk terminated at %q, want %q", cur, provisioningv1.DPUReady)
	}
}

func TestNextTerminalAndUnknown(t *testing.T) {
	for _, p := range []provisioningv1.DPUPhase{
		provisioningv1.DPUReady,
		provisioningv1.DPUError,
		provisioningv1.DPUPhase(""),
		provisioningv1.DPUPhase("Bogus"),
	} {
		if next, ok := Next(p); ok {
			t.Errorf("Next(%q) = (%q, true), want ok=false", p, next)
		}
	}
}

func TestIsTerminal(t *testing.T) {
	if !IsTerminal(provisioningv1.DPUReady) || !IsTerminal(provisioningv1.DPUError) {
		t.Error("Ready and Error must be terminal")
	}
	if IsTerminal(provisioningv1.DPUInitializing) || IsTerminal(provisioningv1.DPURebooting) {
		t.Error("non-terminal phases reported terminal")
	}
}

// TestGates pins the two externally gated phases; everything else on the happy
// path must be dwell-gated.
func TestGates(t *testing.T) {
	if Gate(provisioningv1.DPUNodeEffect) != GateHold {
		t.Error("Node Effect must be hold-gated")
	}
	if Gate(provisioningv1.DPURebooting) != GateReboot {
		t.Error("Rebooting must be reboot-gated")
	}
	for _, p := range HappyPath {
		if p == provisioningv1.DPUNodeEffect || p == provisioningv1.DPURebooting {
			continue
		}
		if Gate(p) != GateDwell {
			t.Errorf("Gate(%q) = %v, want GateDwell", p, Gate(p))
		}
	}
}
