// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

// Package firmwarecomponents converts the lowercase component-name strings
// accepted by the REST/Flow firmware-update API into the per-tray-type
// enum values used by each downstream component manager.
//
// The mappings are written explicitly so that:
//
//   - Renaming or removing a proto enum constant in Core fails the build
//     here at compile time, instead of silently turning into an empty
//     accepted set.
//   - The lowercase REST name for each enum value is reviewable in a PR
//     rather than derived from a prefix-stripping heuristic.
//   - Editors can jump from "bmc" straight to the proto enum const.
//
// Completeness against Core's proto is enforced by the unit tests in this
// package: for each NICo enum, the tests iterate the protoc-generated
// `*_name` reverse map and require every non-UNKNOWN value to appear in
// our mapping. When Core adds a new value, regen + test failure tells the
// developer to pick a lowercase name and add an entry here, on purpose.
//
// # The "dpu" special target on compute trays
//
// "dpu" is intentionally NOT a ComputeTrayComponent enum value: from
// Core's perspective DPUs are independent host machines whose firmware is
// rolled out through the DPU reprovisioning state machine, not through
// the synchronous UpdateComponentFirmware RPC that handles a compute
// tray's BMC / BIOS / NIC / etc. The compute manager treats "dpu" as an
// opt-in side-channel target name (see SplitNICoComputeTraySubTargets)
// that triggers a separate, much longer DPU reprovisioning sequence on
// each targeted host.
//
// Unlike every other compute-tray sub-target, "dpu" is NOT covered by
// the "empty `targets` means update everything" default: an empty or
// missing `targets` list always means "no DPU reprov"; DPU reprov runs
// only when "dpu" is explicitly listed.
package firmwarecomponents

import (
	"fmt"
	"sort"
	"strings"

	corev1 "github.com/NVIDIA/infra-controller/rest-api/proto/core/gen/v1"
)

// === NICo (Core) per-tray enums. ==========================================

var (
	nicoNVSwitchByName = map[string]corev1.NvSwitchComponent{
		"bmc":  corev1.NvSwitchComponent_NV_SWITCH_COMPONENT_BMC,
		"cpld": corev1.NvSwitchComponent_NV_SWITCH_COMPONENT_CPLD,
		"bios": corev1.NvSwitchComponent_NV_SWITCH_COMPONENT_BIOS,
		"nvos": corev1.NvSwitchComponent_NV_SWITCH_COMPONENT_NVOS,
	}
	nicoNVSwitchNames = sortedKeys(nicoNVSwitchByName)

	nicoPowerShelfByName = map[string]corev1.PowerShelfComponent{
		"pmc": corev1.PowerShelfComponent_POWER_SHELF_COMPONENT_PMC,
		"psu": corev1.PowerShelfComponent_POWER_SHELF_COMPONENT_PSU,
	}
	nicoPowerShelfNames = sortedKeys(nicoPowerShelfByName)

	nicoComputeTrayByName = map[string]corev1.ComputeTrayComponent{
		"bmc":               corev1.ComputeTrayComponent_COMPUTE_TRAY_COMPONENT_BMC,
		"bios":              corev1.ComputeTrayComponent_COMPUTE_TRAY_COMPONENT_BIOS,
		"cec":               corev1.ComputeTrayComponent_COMPUTE_TRAY_COMPONENT_CEC,
		"nic":               corev1.ComputeTrayComponent_COMPUTE_TRAY_COMPONENT_NIC,
		"cpld_mb":           corev1.ComputeTrayComponent_COMPUTE_TRAY_COMPONENT_CPLD_MB,
		"cpld_pdb":          corev1.ComputeTrayComponent_COMPUTE_TRAY_COMPONENT_CPLD_PDB,
		"hgx_bmc":           corev1.ComputeTrayComponent_COMPUTE_TRAY_COMPONENT_HGX_BMC,
		"combined_bmc_uefi": corev1.ComputeTrayComponent_COMPUTE_TRAY_COMPONENT_COMBINED_BMC_UEFI,
		"gpu":               corev1.ComputeTrayComponent_COMPUTE_TRAY_COMPONENT_GPU,
		"cx7":               corev1.ComputeTrayComponent_COMPUTE_TRAY_COMPONENT_CX7,
	}
	nicoComputeTrayNames = sortedKeys(nicoComputeTrayByName)
)

// ParseNICoNVSwitch maps lowercase names to NICo NvSwitchComponent values.
// Returns nil for an empty input (callers may interpret as "all components").
func ParseNICoNVSwitch(names []string) ([]corev1.NvSwitchComponent, error) {
	return lookup(names, nicoNVSwitchByName, nicoNVSwitchNames, "nvswitch")
}

// ParseNICoPowerShelf maps lowercase names to NICo PowerShelfComponent values.
func ParseNICoPowerShelf(names []string) ([]corev1.PowerShelfComponent, error) {
	return lookup(names, nicoPowerShelfByName, nicoPowerShelfNames, "powershelf")
}

// ParseNICoComputeTray maps lowercase names to NICo ComputeTrayComponent values.
//
// The special "dpu" target name MUST be removed from `names` before this
// function is called — see SplitNICoComputeTraySubTargets. Passing "dpu"
// directly here is rejected as an unknown component, since "dpu" does
// not correspond to any ComputeTrayComponent enum value.
func ParseNICoComputeTray(names []string) ([]corev1.ComputeTrayComponent, error) {
	return lookup(names, nicoComputeTrayByName, nicoComputeTrayNames, "compute")
}

// DpuComponentName is the special target name that, when present in a
// compute-tray firmware update request's `targets` list, opts the request
// in to a DPU reprovisioning sequence on each targeted host in addition
// to (or instead of) any compute-tray-internal firmware updates.
//
// See the package doc for why "dpu" is not a ComputeTrayComponent enum
// value and why it is excluded from the "empty `targets` means everything"
// default.
const DpuComponentName = "dpu"

// SplitNICoComputeTraySubTargets separates the special DpuComponentName
// from the compute-tray-internal sub-target names. Only the returned
// computeTraySubs slice should be forwarded to ParseNICoComputeTray;
// hasDpu is the opt-in flag for the DPU reprovisioning side-channel that
// the compute manager handles separately.
//
// Whitespace is trimmed and case is normalized for the "dpu" check; all
// other entries are passed through verbatim so ParseNICoComputeTray can
// produce its canonical "unknown component" error for them.
//
// Repeated "dpu" entries are folded into a single hasDpu=true.
func SplitNICoComputeTraySubTargets(names []string) (computeTraySubs []string, hasDpu bool) {
	for _, n := range names {
		if strings.EqualFold(strings.TrimSpace(n), DpuComponentName) {
			hasDpu = true
			continue
		}
		computeTraySubs = append(computeTraySubs, n)
	}
	return computeTraySubs, hasDpu
}

// IsNICoDPUOnlySubTargets reports whether names request DPU reprovisioning
// without any compute-tray-internal firmware update.
func IsNICoDPUOnlySubTargets(names []string) bool {
	computeTraySubs, hasDpu := SplitNICoComputeTraySubTargets(names)
	return hasDpu && len(names) > 0 && len(computeTraySubs) == 0
}

// SupportedNICoNVSwitchNames returns the lowercase names accepted by
// ParseNICoNVSwitch in deterministic order. Useful for surfacing the set
// in API documentation or operator tooling.
func SupportedNICoNVSwitchNames() []string { return append([]string(nil), nicoNVSwitchNames...) }

// SupportedNICoPowerShelfNames returns the lowercase names accepted by
// ParseNICoPowerShelf in deterministic order.
func SupportedNICoPowerShelfNames() []string {
	return append([]string(nil), nicoPowerShelfNames...)
}

// SupportedNICoComputeTrayNames returns the lowercase names accepted by
// ParseNICoComputeTray in deterministic order.
func SupportedNICoComputeTrayNames() []string {
	return append([]string(nil), nicoComputeTrayNames...)
}

// === internal helpers ====================================================

// sortedKeys returns the keys of m in lexicographic order. Used to produce
// deterministic "expected one of: ..." error messages.
func sortedKeys[V any](m map[string]V) []string {
	out := make([]string, 0, len(m))
	for k := range m {
		out = append(out, k)
	}
	sort.Strings(out)
	return out
}

// lookup is the shared per-name resolution loop. An empty/nil input yields
// (nil, nil); callers decide whether nil means "all components" or "default
// to one specific component". Surrounding whitespace is tolerated; case
// is normalized to lowercase before lookup.
func lookup[E any](names []string, table map[string]E, sortedNames []string, kind string) ([]E, error) {
	if len(names) == 0 {
		return nil, nil
	}
	out := make([]E, 0, len(names))
	for _, n := range names {
		v, ok := table[strings.ToLower(strings.TrimSpace(n))]
		if !ok {
			return nil, fmt.Errorf(
				"unknown %s component %q (expected one of: %s)",
				kind, n, strings.Join(sortedNames, ", "),
			)
		}
		out = append(out, v)
	}
	return out, nil
}
