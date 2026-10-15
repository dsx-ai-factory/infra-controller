// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package model

import (
	"errors"
	"fmt"
	"strings"

	validation "github.com/go-ozzo/ozzo-validation/v4"
	validationis "github.com/go-ozzo/ozzo-validation/v4/is"

	cdbm "github.com/NVIDIA/infra-controller/rest-api/db/pkg/db/model"
)

// ExpectedInventoryMaxReplaceItems matches the existing Expected Machine batch limit.
const ExpectedInventoryMaxReplaceItems = ExpectedMachineMaxBatchItems

func validateExpectedInventoryListSize(field string, count int) error {
	if count > ExpectedInventoryMaxReplaceItems {
		return validation.Errors{field: fmt.Errorf("at most %d entries are allowed", ExpectedInventoryMaxReplaceItems)}
	}
	return nil
}

// APIReplaceAllExpectedMachinesRequest replaces the complete ExpectedMachine
// set for one Site. An empty ExpectedMachines list clears the set.
type APIReplaceAllExpectedMachinesRequest struct {
	SiteID           string                             `json:"siteId"`
	ExpectedMachines []*APIExpectedMachineCreateRequest `json:"expectedMachines"`
}

// Validate checks every replacement and the identities that must be unique
// within the replacement set.
func (r *APIReplaceAllExpectedMachinesRequest) Validate() error {
	err := validateExpectedInventorySiteID(r.SiteID)
	if err != nil {
		return err
	}
	if r.ExpectedMachines == nil {
		return validation.Errors{"expectedMachines": errors.New(validationErrorValueRequired)}
	}
	if err := validateExpectedInventoryListSize("expectedMachines", len(r.ExpectedMachines)); err != nil {
		return err
	}

	macs := make(map[string]int, len(r.ExpectedMachines))
	serials := make(map[string]int, len(r.ExpectedMachines))
	for i, machine := range r.ExpectedMachines {
		if machine == nil {
			return validation.Errors{"expectedMachines": errors.New("ExpectedMachine entry cannot be null")}
		}
		if err = machine.Validate(); err != nil {
			return validation.Errors{"expectedMachines": fmt.Errorf("entry %d: %w", i, err)}
		}
		if machine.SiteID != r.SiteID {
			return validation.Errors{"expectedMachines": fmt.Errorf("entry %d: siteId does not match top-level siteId", i)}
		}
		if previous, ok := macs[cdbm.NormalizeMacAddress(machine.BmcMacAddress)]; ok {
			return validation.Errors{"expectedMachines": fmt.Errorf("entry %d: duplicate bmcMacAddress also used by entry %d", i, previous)}
		}
		macs[cdbm.NormalizeMacAddress(machine.BmcMacAddress)] = i
		serial := strings.ToLower(machine.ChassisSerialNumber)
		if previous, ok := serials[serial]; ok {
			return validation.Errors{"expectedMachines": fmt.Errorf("entry %d: duplicate chassisSerialNumber also used by entry %d", i, previous)}
		}
		serials[serial] = i
	}
	return nil
}

// APIReplaceAllExpectedSwitchesRequest replaces the complete ExpectedSwitch
// set for one Site. An empty ExpectedSwitches list clears the set.
type APIReplaceAllExpectedSwitchesRequest struct {
	SiteID           string                            `json:"siteId"`
	ExpectedSwitches []*APIExpectedSwitchCreateRequest `json:"expectedSwitches"`
}

// Validate checks every replacement and the identities that must be unique
// within the replacement set.
func (r *APIReplaceAllExpectedSwitchesRequest) Validate() error {
	err := validateExpectedInventorySiteID(r.SiteID)
	if err != nil {
		return err
	}
	if r.ExpectedSwitches == nil {
		return validation.Errors{"expectedSwitches": errors.New(validationErrorValueRequired)}
	}
	if err := validateExpectedInventoryListSize("expectedSwitches", len(r.ExpectedSwitches)); err != nil {
		return err
	}

	macs := make(map[string]int, len(r.ExpectedSwitches))
	serials := make(map[string]int, len(r.ExpectedSwitches))
	nvosMACs := make(map[string]int)
	for i, expectedSwitch := range r.ExpectedSwitches {
		if expectedSwitch == nil {
			return validation.Errors{"expectedSwitches": errors.New("ExpectedSwitch entry cannot be null")}
		}
		if err = expectedSwitch.Validate(); err != nil {
			return validation.Errors{"expectedSwitches": fmt.Errorf("entry %d: %w", i, err)}
		}
		if expectedSwitch.SiteID != r.SiteID {
			return validation.Errors{"expectedSwitches": fmt.Errorf("entry %d: siteId does not match top-level siteId", i)}
		}
		mac := cdbm.NormalizeMacAddress(expectedSwitch.BmcMacAddress)
		if previous, ok := macs[mac]; ok {
			return validation.Errors{"expectedSwitches": fmt.Errorf("entry %d: duplicate bmcMacAddress also used by entry %d", i, previous)}
		}
		macs[mac] = i
		serial := strings.ToLower(expectedSwitch.SwitchSerialNumber)
		if previous, ok := serials[serial]; ok {
			return validation.Errors{"expectedSwitches": fmt.Errorf("entry %d: duplicate switchSerialNumber also used by entry %d", i, previous)}
		}
		serials[serial] = i
		for _, rawMAC := range expectedSwitch.NvosMacAddresses {
			mac = cdbm.NormalizeMacAddress(rawMAC)
			if previous, ok := nvosMACs[mac]; ok {
				return validation.Errors{"expectedSwitches": fmt.Errorf("entry %d: duplicate nvosMacAddress also used by entry %d", i, previous)}
			}
			nvosMACs[mac] = i
		}
	}
	return nil
}

// APIReplaceAllExpectedPowerShelvesRequest replaces the complete
// ExpectedPowerShelf set for one Site. An empty list clears the set.
type APIReplaceAllExpectedPowerShelvesRequest struct {
	SiteID               string                                `json:"siteId"`
	ExpectedPowerShelves []*APIExpectedPowerShelfCreateRequest `json:"expectedPowerShelves"`
}

// Validate checks every replacement and the identities that must be unique
// within the replacement set.
func (r *APIReplaceAllExpectedPowerShelvesRequest) Validate() error {
	err := validateExpectedInventorySiteID(r.SiteID)
	if err != nil {
		return err
	}
	if r.ExpectedPowerShelves == nil {
		return validation.Errors{"expectedPowerShelves": errors.New(validationErrorValueRequired)}
	}
	if err := validateExpectedInventoryListSize("expectedPowerShelves", len(r.ExpectedPowerShelves)); err != nil {
		return err
	}

	macs := make(map[string]int, len(r.ExpectedPowerShelves))
	serials := make(map[string]int, len(r.ExpectedPowerShelves))
	for i, shelf := range r.ExpectedPowerShelves {
		if shelf == nil {
			return validation.Errors{"expectedPowerShelves": errors.New("ExpectedPowerShelf entry cannot be null")}
		}
		if err = shelf.Validate(); err != nil {
			return validation.Errors{"expectedPowerShelves": fmt.Errorf("entry %d: %w", i, err)}
		}
		if shelf.SiteID != r.SiteID {
			return validation.Errors{"expectedPowerShelves": fmt.Errorf("entry %d: siteId does not match top-level siteId", i)}
		}
		mac := cdbm.NormalizeMacAddress(shelf.BmcMacAddress)
		if previous, ok := macs[mac]; ok {
			return validation.Errors{"expectedPowerShelves": fmt.Errorf("entry %d: duplicate bmcMacAddress also used by entry %d", i, previous)}
		}
		macs[mac] = i
		serial := strings.ToLower(shelf.ShelfSerialNumber)
		if previous, ok := serials[serial]; ok {
			return validation.Errors{"expectedPowerShelves": fmt.Errorf("entry %d: duplicate shelfSerialNumber also used by entry %d", i, previous)}
		}
		serials[serial] = i
	}
	return nil
}

func validateExpectedInventorySiteID(siteID string) error {
	err := validation.Validate(siteID,
		validation.Required.Error(validationErrorValueRequired),
		validationis.UUID.Error(validationErrorInvalidUUID),
	)
	if err != nil {
		return validation.Errors{"siteId": err}
	}
	return nil
}
