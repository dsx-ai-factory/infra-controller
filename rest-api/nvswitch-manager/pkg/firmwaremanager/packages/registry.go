// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package packages

import (
	"fmt"
	"os"
	"path/filepath"
	"strings"
	"sync"

	log "github.com/sirupsen/logrus"
	"gopkg.in/yaml.v3"
)

// Registry loads and provides access to firmware packages.
type Registry struct {
	// firmwareDir is the base directory containing firmware files
	firmwareDir string

	// packages maps bundle version to package definition
	packages map[string]*FirmwarePackage
	mu       sync.RWMutex
}

// NewRegistry creates a new package registry.
func NewRegistry(firmwareDir string) *Registry {
	return &Registry{
		firmwareDir: firmwareDir,
		packages:    make(map[string]*FirmwarePackage),
	}
}

// LoadFromDirectory loads all YAML package definitions from a directory.
func (r *Registry) LoadFromDirectory(packagesDir string) error {
	r.mu.Lock()
	defer r.mu.Unlock()

	// Clear existing packages
	r.packages = make(map[string]*FirmwarePackage)

	// Find all YAML files
	entries, err := os.ReadDir(packagesDir)
	if err != nil {
		return fmt.Errorf("failed to read packages directory %s: %w", packagesDir, err)
	}

	loaded := 0
	for _, entry := range entries {
		if entry.IsDir() {
			continue
		}

		name := entry.Name()
		if !strings.HasSuffix(name, ".yaml") && !strings.HasSuffix(name, ".yml") {
			continue
		}

		path := filepath.Join(packagesDir, name)
		if err := r.loadPackageFile(path); err != nil {
			log.Warnf("Failed to load package file %s: %v", path, err)
			continue
		}
		loaded++
	}

	log.Infof("Loaded %d firmware packages from %s", loaded, packagesDir)
	return nil
}

// loadPackageFile loads a single YAML package file.
func (r *Registry) loadPackageFile(path string) error {
	data, err := os.ReadFile(path)
	if err != nil {
		return fmt.Errorf("failed to read file: %w", err)
	}

	var pkg FirmwarePackage
	if err := yaml.Unmarshal(data, &pkg); err != nil {
		return fmt.Errorf("failed to parse YAML: %w", err)
	}

	// Validate the package
	if err := pkg.Validate(); err != nil {
		return fmt.Errorf("validation failed: %w", err)
	}

	// Check for duplicate version
	if existing, ok := r.packages[pkg.Version]; ok {
		return fmt.Errorf("duplicate version %s (already loaded from another file)", existing.Version)
	}

	// Validate firmware files exist
	for name, comp := range pkg.Components {
		firmwarePath := filepath.Join(r.firmwareDir, comp.File)
		if _, err := os.Stat(firmwarePath); os.IsNotExist(err) {
			return fmt.Errorf("component %s: firmware file not found: %s", name, firmwarePath)
		}
	}

	r.packages[pkg.Version] = &pkg
	log.Debugf("Loaded firmware package: version=%s, components=%d", pkg.Version, len(pkg.Components))
	return nil
}

// Get retrieves a firmware package by version.
func (r *Registry) Get(version string) (*FirmwarePackage, error) {
	r.mu.RLock()
	defer r.mu.RUnlock()

	pkg, ok := r.packages[version]
	if !ok {
		return nil, fmt.Errorf("firmware bundle version %q not found", version)
	}
	return pkg, nil
}

// List returns all available package versions.
func (r *Registry) List() []string {
	r.mu.RLock()
	defer r.mu.RUnlock()

	versions := make([]string, 0, len(r.packages))
	for version := range r.packages {
		versions = append(versions, version)
	}
	return versions
}

// ListPackages returns all loaded packages.
func (r *Registry) ListPackages() []*FirmwarePackage {
	r.mu.RLock()
	defer r.mu.RUnlock()

	pkgs := make([]*FirmwarePackage, 0, len(r.packages))
	for _, pkg := range r.packages {
		pkgs = append(pkgs, pkg)
	}
	return pkgs
}

// GetFirmwarePath returns the full filesystem path to a component's firmware file.
func (r *Registry) GetFirmwarePath(pkg *FirmwarePackage, componentName string) (string, error) {
	comp := pkg.GetComponent(componentName)
	if comp == nil {
		return "", fmt.Errorf("component %q not found in package %s", componentName, pkg.Version)
	}
	return filepath.Join(r.firmwareDir, comp.File), nil
}

// Count returns the number of loaded packages.
func (r *Registry) Count() int {
	r.mu.RLock()
	defer r.mu.RUnlock()
	return len(r.packages)
}
