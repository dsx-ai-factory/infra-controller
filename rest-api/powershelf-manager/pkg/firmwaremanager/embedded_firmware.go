// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package firmwaremanager

import (
	"fmt"
	"io/fs"
	"os"
	"strings"

	"github.com/NVIDIA/infra-controller/rest-api/powershelf-manager/pkg/common/vendor"
	log "github.com/sirupsen/logrus"
)

const pmcPath = "pmc"

// FirmwareFetcher provides read-only access to firmware assets organized as firmware/<vendor>/pmc.
type FirmwareFetcher struct {
	fs fs.FS
}

// FirmwareEntry identifies a firmware artifact by name and FS path.
type FirmwareEntry struct {
	name string
	path string
}

// newFirmwareFetcher returns a FirmwareFetcher backed by the given on-disk directory.
func newFirmwareFetcher(firmwareDir string) *FirmwareFetcher {
	return &FirmwareFetcher{fs: os.DirFS(firmwareDir)}
}

// getVendorDirectories lists vendor directories under the firmware root.
func (ff *FirmwareFetcher) getVendorDirectories() ([]fs.DirEntry, error) {
	return fs.ReadDir(ff.fs, ".")
}

// getPmcFirmwareEntries returns all PMC firmware files for a vendor; entries are non-empty .tar files.
func (ff *FirmwareFetcher) getPmcFirmwareEntries(v vendor.Vendor) ([]FirmwareEntry, error) {
	vendors, err := ff.getVendorDirectories()
	if err != nil {
		return nil, err
	}

	expectedVendorName := strings.ToLower(v.Name)

	for _, vendor := range vendors {
		if vendor.IsDir() {
			vendorName := vendor.Name()
			if vendorName == expectedVendorName {
				path := fmt.Sprintf("%s/%s", vendorName, pmcPath)
				entries, err := fs.ReadDir(ff.fs, path)
				if err != nil {
					return nil, err
				}

				fwEntries := make([]FirmwareEntry, 0, len(entries))
				for _, entry := range entries {
					if entry.IsDir() {
						log.Printf("found unexpected dir entry in {%s}: {%s}\n", path, entry.Name())
						continue
					}

					name := entry.Name()
					info, err := entry.Info()
					if err != nil {
						log.Printf("failed to get info for entry in {%s}: {%s}, err: %v\n", path, entry.Name(), err)
						continue
					}

					size := info.Size()
					if size == 0 {
						log.Printf("Vendor %s: skipping empty firmware file in {%s}: {%s}\n", vendorName, path, entry.Name())
						continue
					}

					fwPath := fmt.Sprintf("%s/%s", path, name)
					log.Printf("Vendor %s: adding fw {%s} at %s (size: %d bytes)\n", vendorName, name, fwPath, size)
					fwEntries = append(fwEntries, FirmwareEntry{
						name: name,
						path: fwPath,
					})

				}

				return fwEntries, nil
			}
		}
	}

	log.Printf("no embedded firmware found for vendor %s; firmware operations will be unavailable for this vendor", v.Name)
	return nil, nil
}

// open opens a file by path.
func (ff *FirmwareFetcher) open(path string) (fs.File, error) {
	return ff.fs.Open(path)
}
