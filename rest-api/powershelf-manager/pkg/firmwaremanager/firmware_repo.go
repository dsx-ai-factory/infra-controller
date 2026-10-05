// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package firmwaremanager

import (
	"fmt"
	"io/fs"
	"os"
	"strings"

	"github.com/dsx-ai-factory/infra-controller/rest-api/powershelf-manager/pkg/common/util"
	"github.com/dsx-ai-factory/infra-controller/rest-api/powershelf-manager/pkg/common/vendor"
)

// FirmwareRepo holds parsed firmware upgrade edges and the supported starting-version range for a vendor.
type FirmwareRepo struct {
	ff                   *FirmwareFetcher
	minStartingFwVersion firmwareVersion
	maxStartingFwVersion firmwareVersion
	upgrades             []FirmwareUpgrade
}

// summary returns a human-readable report of supported versions and artifacts.
func (repo *FirmwareRepo) summary() (string, error) {
	sb := strings.Builder{}

	if len(repo.upgrades) == 0 {
		sb.WriteString("Firmware Repo has no firmware artifacts available\n")
		return sb.String(), nil
	}

	sb.WriteString(fmt.Sprintf("Firmware Repo supports upgrading powershelves starting at PMC fw version between %s to %s (inclusive)\n", repo.minStartingFwVersion.String(), repo.maxStartingFwVersion.String()))

	for i, upgrade := range repo.upgrades {
		file, err := repo.ff.open(upgrade.path)
		if err != nil {
			return "", err
		}
		defer file.Close()

		info, err := file.Stat()
		if err != nil {
			return "", err
		}

		sb.WriteString(fmt.Sprintf("FW Upgrade %d: %v from %s to %s (size: %v bytes)\n", i, upgrade.path, upgrade.from, upgrade.to, util.HumanReadableSize(info.Size())))
	}

	return sb.String(), nil
}

// supportUpgrade returns true if the current version falls within the repo's
// supported range, which spans the minimum 'from' to the maximum 'to' across
// all upgrade edges.
func (repo *FirmwareRepo) supportUpgrade(currentFwVersion firmwareVersion) bool {
	return repo.minStartingFwVersion.cmp(currentFwVersion) <= 0 && repo.maxStartingFwVersion.cmp(currentFwVersion) >= 0
}

// open opens the firmware artifact for the given edge.
func (repo *FirmwareRepo) open(upgrade *FirmwareUpgrade) (fs.File, error) {
	return repo.ff.open(upgrade.path)
}

// newFirmwareRepo discovers firmware artifacts for a vendor, parses filename-encoded edges, and computes supported range.
func newFirmwareRepo(v vendor.Vendor, firmwareDir string) (*FirmwareRepo, error) {
	if firmwareDir == "" {
		return nil, fmt.Errorf("firmware directory not configured (set FW_DIR)")
	}

	if info, err := os.Stat(firmwareDir); err != nil {
		return nil, fmt.Errorf("firmware directory %q does not exist: %w", firmwareDir, err)
	} else if !info.IsDir() {
		return nil, fmt.Errorf("firmware path %q is not a directory", firmwareDir)
	}

	ff := newFirmwareFetcher(firmwareDir)

	fw_entries, err := ff.getPmcFirmwareEntries(v)
	if err != nil {
		return nil, err
	}

	if fw_entries == nil {
		return &FirmwareRepo{ff: ff}, nil
	}

	var upgrades []FirmwareUpgrade = make([]FirmwareUpgrade, 0, len(fw_entries))
	var minStartingFwVersion firmwareVersion
	var maxStartingFwVersion firmwareVersion

	for _, fw := range fw_entries {
		name := fw.name
		// Parse source/target from filename, e.g. "cm14mp1r-r1.3.7_to_r1.3.8.tar"
		var from, to firmwareVersion
		_, err := fmt.Sscanf(name, "cm14mp1r-r%d.%d.%d_to_r%d.%d.%d.tar",
			&from.major, &from.minor, &from.patch,
			&to.major, &to.minor, &to.patch)
		if err == nil {
			upgrades = append(upgrades, FirmwareUpgrade{
				from: from,
				to:   to,
				path: fw.path,
			})

			if len(upgrades) == 1 {
				minStartingFwVersion = from
				maxStartingFwVersion = to
			}
			if from.cmp(minStartingFwVersion) < 0 {
				minStartingFwVersion = from
			}
			if to.cmp(maxStartingFwVersion) > 0 {
				maxStartingFwVersion = to
			}
		}
	}

	return &FirmwareRepo{ff: ff, minStartingFwVersion: minStartingFwVersion, maxStartingFwVersion: maxStartingFwVersion, upgrades: upgrades}, nil
}
