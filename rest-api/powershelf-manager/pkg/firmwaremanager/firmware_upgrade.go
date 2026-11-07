// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package firmwaremanager

import (
	"github.com/NVIDIA/infra-controller/rest-api/powershelf-manager/pkg/common/vendor"
)

// FirmwareUpgrade represents a single directed edge from a source firmware version to a target version, with the artifact path.
type FirmwareUpgrade struct {
	from firmwareVersion
	to   firmwareVersion
	path string
}

/*

	PmcMacAddress      string                        `bun:"pmc_mac_address,pk,notnull"`       // MAC address of the target PMC (FK to pmc.mac_address)
	Component          powershelf.Component          `bun:"component,pk,notnull"`             // Component being updated (e.g., "PMC", "PSU1")
	VersionStart       string                        `bun:"version_start,notnull"`            // Firmware version before upgrade
	VersionTarget      string                        `bun:"version_target,notnull"`           // Target firmware version after upgrade
	State              firmwaremanager.FirmwareState `bun:"state,notnull"`                    // Upgrade state ("Queued", "Updating", "Completed", "Failed", etc.)
	LastTransitionTime time.Time                     `bun:"last_transition_time,notnull"`     // When the state last changed
	JobID              string                        `bun:"job_id"`                           // Device job/task ID, if provided by hardware
	ErrorMessage       string                        `bun:"error_message"`                    // Error message if the upgrade failed
	CreatedAt          time.Time                     `bun:"created_at,notnull,default:now()"` // When this record was created
	UpdatedAt          time.Time                     `bun:"updated_at,notnull,default:now()"` // When this record was last updated
*/

func (upgrade *FirmwareUpgrade) UpgradeTo() firmwareVersion {
	return upgrade.to
}

// UpgradeRule determines whether a given upgrade edge is permissible from a current firmware version.
type UpgradeRule interface {
	isAllowed(currentFw firmwareVersion, upgrade FirmwareUpgrade) bool
	summary() string
}

// LiteonUpgradeRule allows upgrades where the device's current version equals
// the edge's source or target. Liteon firmware artifacts are full images, so
// re-flashing the same version (current == to) is safe.
type LiteonUpgradeRule struct{}

func (r LiteonUpgradeRule) isAllowed(currentFw firmwareVersion, upgrade FirmwareUpgrade) bool {
	return currentFw.cmp(upgrade.from) == 0 || currentFw.cmp(upgrade.to) == 0
}

func (r LiteonUpgradeRule) summary() string {
	return "Liteon upgrade rule: direct upgrades and same-version re-flash supported"
}

// DeltaUpgradeRule allows only direct upgrades where the device's current version equals the edge's source.
type DeltaUpgradeRule struct{}

func (r DeltaUpgradeRule) isAllowed(currentFw firmwareVersion, upgrade FirmwareUpgrade) bool {
	return currentFw.cmp(upgrade.from) == 0
}

func (r DeltaUpgradeRule) summary() string {
	return "Delta upgrade rule: only direct upgrades supported"
}

// newUpgradeRule returns the vendor's rule set or an error if the vendor is unsupported.
func newUpgradeRule(v vendor.Vendor) (UpgradeRule, error) {
	switch v.Code {
	case vendor.VendorCodeLiteon:
		return LiteonUpgradeRule{}, nil
	case vendor.VendorCodeDelta:
		return DeltaUpgradeRule{}, nil
	default:
		return nil, v.IsSupported()
	}
}
