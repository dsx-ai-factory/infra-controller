// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package firmwaremanager

import (
	"context"
	"fmt"
	"strings"

	"github.com/dsx-ai-factory/infra-controller/rest-api/powershelf-manager/pkg/common/vendor"
	"github.com/dsx-ai-factory/infra-controller/rest-api/powershelf-manager/pkg/objects/pmc"
	"github.com/dsx-ai-factory/infra-controller/rest-api/powershelf-manager/pkg/redfish"

	log "github.com/sirupsen/logrus"
)

// A real firmware image is at least several megabytes; LFS pointers are typically < 200 bytes.
const minFirmwareSize = 1024

// FirmwareUpdater encapsulates a vendor's FirmwareRepo and UpgradeRule to select and execute upgrades.
type FirmwareUpdater struct {
	vendor vendor.Vendor
	repo   *FirmwareRepo
	rule   UpgradeRule
}

// Summary returns the repo and rule summaries for the vendor.
func (updater *FirmwareUpdater) Summary() (string, error) {
	sb := strings.Builder{}
	repoSummary, err := updater.repo.summary()
	if err != nil {
		return "", err
	}

	sb.WriteString(updater.vendor.String() + " Firmware Repo Summary:\n")
	sb.WriteString(repoSummary)
	sb.WriteString("\n" + updater.vendor.String() + " Firmware Upgrade Rule Summary:\n")
	sb.WriteString(updater.rule.summary() + "\n")

	return sb.String(), nil
}

func newFirmwareUpdater(v vendor.Vendor, firmwareDir string) (*FirmwareUpdater, error) {
	if err := v.IsSupported(); err != nil {
		return nil, err
	}

	repo, err := newFirmwareRepo(v, firmwareDir)
	if err != nil {
		return nil, err
	}

	if len(repo.upgrades) == 0 {
		log.Printf("no firmware artifacts available for vendor %s; firmware operations will be unavailable", v.Name)
	}

	rule, err := newUpgradeRule(v)
	if err != nil {
		return nil, err
	}

	return &FirmwareUpdater{vendor: v, repo: repo, rule: rule}, nil
}

// canUpdate checks the repo's support range for the current version.
func (fp *FirmwareUpdater) canUpdate(current firmwareVersion, targetVersion firmwareVersion) bool {
	if fp.repo.supportUpgrade(current) {
		for _, upgrade := range fp.repo.upgrades {
			if upgrade.to.cmp(targetVersion) == 0 && fp.rule.isAllowed(current, upgrade) {
				return true
			}
		}

	}

	return false
}

// getFwUpgrade selects an allowed upgrade edge for the current version per rule; returns nil if none.
func (fp *FirmwareUpdater) getFwUpgrade(current firmwareVersion, targetVersion firmwareVersion) *FirmwareUpgrade {
	for _, upgrade := range fp.repo.upgrades {
		if upgrade.to.cmp(targetVersion) == 0 && fp.rule.isAllowed(current, upgrade) {
			return &upgrade
		}
	}

	return nil
}

// getFwVersion queries the device via Redfish Manager.FirmwareVersion and parses it to firmwareVersion.
func (fp *FirmwareUpdater) getFwVersion(client *redfish.RedfishClient) (firmwareVersion, error) {
	manager, err := client.QueryManager()
	if err != nil {
		return firmwareVersion{
			major: 0,
			minor: 0,
			patch: 0,
		}, err
	}

	return fwVersionFromStr(manager.FirmwareVersion)

}

// update executes the upgrade for an existing Redfish client; when dryRun, returns a synthetic 200 OK without uploading.
func (fp *FirmwareUpdater) update(client *redfish.RedfishClient, targetVersion firmwareVersion, dryRun bool) error {
	currentVersion, err := fp.getFwVersion(client)
	if err != nil {
		return err
	}

	if fp.canUpdate(currentVersion, targetVersion) {
		upgrade := fp.getFwUpgrade(currentVersion, targetVersion)
		if upgrade != nil {
			fw, err := fp.repo.open(upgrade)
			if err != nil {
				return err
			}
			defer fw.Close()

			info, err := fw.Stat()
			if err != nil {
				return err
			}
			size := info.Size()

			if size < minFirmwareSize {
				return fmt.Errorf("firmware artifact %s is only %d bytes -- this is likely a Git LFS pointer, not the actual firmware (run 'git lfs pull')", upgrade.path, size)
			}

			log.Printf("Upgrading firmware from %s to %s using %s (size: %d bytes, dry_run: %v)\n", upgrade.from.String(), upgrade.to.String(), upgrade.path, size, dryRun)

			if dryRun {
				log.Printf("Dry run: would upgrade firmware from %s to %s using %s (size: %d bytes)\n", upgrade.from.String(), upgrade.to.String(), upgrade.path, size)
				return nil
			}

			return client.UpdateFirmware(fw)
		}
	}

	return fmt.Errorf("FW Updater does not support updating powershelf that has a PMC fw version of r.%v.%v.%v\n", currentVersion.major, currentVersion.minor, currentVersion.patch)
}

// upgrade opens a Redfish session and delegates to update.
func (fp *FirmwareUpdater) upgrade(ctx context.Context, pmc *pmc.PMC, targetVersion firmwareVersion, dryRun bool) error {
	client, err := redfish.New(ctx, pmc, false)
	if err != nil {
		return err
	}
	defer client.Logout()

	return fp.update(client, targetVersion, dryRun)
}

// canUpdatePmc reports whether an upgrade is supported for the PMC's current version.
func (fp *FirmwareUpdater) canUpdatePmc(ctx context.Context, pmc *pmc.PMC, targetVersion firmwareVersion) (bool, error) {
	client, err := redfish.New(ctx, pmc, true)
	if err != nil {
		return false, err
	}
	defer client.Logout()

	currentVersion, err := fp.getFwVersion(client)
	if err != nil {
		return false, err
	}

	return fp.canUpdate(currentVersion, targetVersion), nil
}
