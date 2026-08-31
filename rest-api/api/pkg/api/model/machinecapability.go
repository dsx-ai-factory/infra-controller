// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package model

import (
	"errors"
	"fmt"
	"math"

	validation "github.com/go-ozzo/ozzo-validation/v4"

	"github.com/NVIDIA/infra-controller/rest-api/api/pkg/api/model/util"
	cdbm "github.com/NVIDIA/infra-controller/rest-api/db/pkg/db/model"
	cwma "github.com/NVIDIA/infra-controller/rest-api/workflow/pkg/activity/machine"
)

// APIMachineCapabilities is a typed slice that owns the list-level
// rules across machine capabilities. It both delegates per-element
// validation to `(APIMachineCapability).Validate` and enforces the
// `(Type, Name, DeviceType)` uniqueness invariant the proto layer relies on.
type APIMachineCapabilities []APIMachineCapability

// Validate runs `APIMachineCapability.Validate` on each entry and then
// enforces that no two entries share the same `(Type, Name, DeviceType)` tuple.
func (caps APIMachineCapabilities) Validate() error {
	if err := validation.Validate([]APIMachineCapability(caps), validation.Each()); err != nil {
		return err
	}
	seen := map[string]bool{}
	for _, mc := range caps {
		key := mc.MapKey()
		if seen[key] {
			if mc.DeviceType == nil || *mc.DeviceType == "" {
				return validation.Errors{
					"machineCapabilities": fmt.Errorf("requested Capability type `%s` cannot contain duplicate Capability name: %s", mc.Type, mc.Name),
				}
			}
			return validation.Errors{
				"machineCapabilities": fmt.Errorf("requested Capability type `%s` cannot contain duplicate Capability name and device type: %s, %s", mc.Type, mc.Name, *mc.DeviceType),
			}
		}
		seen[key] = true
	}
	return nil
}

// APIMachineCapability is the datastructure to capture API representation of a MachineCapability
type APIMachineCapability struct {
	// Type is the type of the machine capability
	Type cdbm.MachineCapabilityType `json:"type"`
	// Name describes the capability
	Name string `json:"name"`
	// Frequency describes the frequency of the capability
	Frequency *string `json:"frequency,omitempty"`
	// Cores describes the number of cores for the capability
	Cores *int `json:"cores,omitempty"`
	// Threads describes the number of threads for the capability
	Threads *int `json:"threads,omitempty"`
	// Capacity describes the capacity of the capability
	Capacity *string `json:"capacity,omitempty"`
	// Vendor describes the vendor of the capability
	Vendor *string `json:"vendor,omitempty"`
	// InactiveDevices describes a set of inactive devices
	InactiveDevices []int `json:"inactiveDevices,omitempty"`
	// HardwareRevision describes the hardware revision of the capability
	HardwareRevision *string `json:"hardwareRevision,omitempty"`
	// Count describes the number of items present for this capability
	Count *int `json:"count"`
	// DeviceType describes the type of the device
	DeviceType *cdbm.MachineCapabilityDeviceType `json:"deviceType,omitempty"`
}

// MapKey returns the canonical identity string used as a map key
// for dedup or `(Type, Name, DeviceType)` lookups against the DB-side
// `(*cdbm.MachineCapability).MapKey()`. Both methods produce the same
// shape so an API request capability and its DB counterpart hash to
// the same key.
func (mc APIMachineCapability) MapKey() string {
	return cdbm.MachineCapabilityMapKey(mc.Type, mc.Name, mc.DeviceType)
}

// Validate ensures the values on a single capability are acceptable
// and that the row will round-trip cleanly into the workflow proto.
// Per-list concerns (e.g. duplicate `(Type, Name, DeviceType)` tuples across
// capabilities) are enforced at the list level by
// `APIMachineCapabilities.Validate`.
//
// `ToProto` trusts that this has run, so anything the proto cast or
// downstream wire shape relies on belongs here.
func (mc APIMachineCapability) Validate() error {
	mctypes := make([]interface{}, 0, len(cdbm.MachineCapabilityTypeChoiceMap))
	for mctype := range cdbm.MachineCapabilityTypeChoiceMap {
		mctypes = append(mctypes, mctype)
	}

	return validation.ValidateStruct(&mc,
		validation.Field(&mc.Type,
			validation.Required.Error("type must be specified for each Machine Capability"),
			validation.In(mctypes...).Error(fmt.Sprintf("invalid value: %v for Machine Capability type", mc.Type))),
		validation.Field(&mc.Name,
			validation.Required.Error("name must be specified for each Machine Capability"),
			validation.By(util.ValidateNameCharacters),
			validation.Length(2, 256).Error(validationErrorStringLength)),
		validation.Field(&mc.Count,
			validation.Min(0).Error(fmt.Sprintf("Count for capability %q must be non-negative", mc.Name)),
			validation.Max(math.MaxUint32).Error(fmt.Sprintf("Count for capability %q must fit in uint32 (0..%d)", mc.Name, uint32(math.MaxUint32)))),
		validation.Field(&mc.Cores,
			validation.Min(0).Error(fmt.Sprintf("Cores for capability %q must be non-negative", mc.Name)),
			validation.Max(math.MaxUint32).Error(fmt.Sprintf("Cores for capability %q must fit in uint32 (0..%d)", mc.Name, uint32(math.MaxUint32)))),
		validation.Field(&mc.Threads,
			validation.Min(0).Error(fmt.Sprintf("Threads for capability %q must be non-negative", mc.Name)),
			validation.Max(math.MaxUint32).Error(fmt.Sprintf("Threads for capability %q must fit in uint32 (0..%d)", mc.Name, uint32(math.MaxUint32)))),
		validation.Field(&mc.DeviceType, validation.By(mc.validateDeviceType)),
		validation.Field(&mc.InactiveDevices, validation.By(mc.validateInactiveDevices)),
	)
}

// validateDeviceType enforces the Type/DeviceType compatibility rules:
// Network capabilities require DPU or SpectrumX, GPU capabilities require NVLink,
// every other Type must not carry a DeviceType. A nil DeviceType is
// always allowed.
func (mc APIMachineCapability) validateDeviceType(value interface{}) error {
	dt, ok := value.(*cdbm.MachineCapabilityDeviceType)
	if !ok || dt == nil {
		return nil
	}
	switch mc.Type {
	case cdbm.MachineCapabilityTypeNetwork:
		if *dt != cdbm.MachineCapabilityDeviceTypeDPU && *dt != cdbm.MachineCapabilityDeviceTypeSpectrumX {
			return fmt.Errorf("Unsupported Device Type specified for Network Capability %s", *dt)
		}
	case cdbm.MachineCapabilityTypeGPU:
		if *dt != cdbm.MachineCapabilityDeviceTypeNVLink {
			return fmt.Errorf("Unsupported Device Type specified for GPU Capability %s", *dt)
		}
	default:
		return fmt.Errorf("Unsupported Device Type: %s specified for Capability type %s", *dt, mc.Type)
	}
	return nil
}

// validateInactiveDevices enforces that InactiveDevices is only set on
// InfiniBand capabilities and that each entry fits in the proto uint32
// width before `MachineCapability.ToProto` casts it.
func (mc APIMachineCapability) validateInactiveDevices(value interface{}) error {
	ids, ok := value.([]int)
	if !ok || len(ids) == 0 {
		return nil
	}
	if mc.Type != cdbm.MachineCapabilityTypeInfiniBand {
		return errors.New("InactiveDevices specified for non-InfiniBand Capability Type")
	}
	for i, id := range ids {
		if id < 0 || id > math.MaxUint32 {
			return fmt.Errorf("InactiveDevices[%d] for capability %q must fit in uint32 (0..%d)", i, mc.Name, uint32(math.MaxUint32))
		}
	}
	return nil
}

// NewAPIMachineCapability accepts a DB layer MachineCapability object and returns an API object
func NewAPIMachineCapability(dbmc *cdbm.MachineCapability) *APIMachineCapability {
	apimc := &APIMachineCapability{
		Type:            dbmc.Type,
		Name:            dbmc.Name,
		Frequency:       dbmc.Frequency,
		Capacity:        dbmc.Capacity,
		Vendor:          dbmc.Vendor,
		InactiveDevices: dbmc.InactiveDevices,
		Count:           dbmc.Count,
		DeviceType:      dbmc.DeviceType,
	}

	if dbmc.Type == cdbm.MachineCapabilityTypeCPU && dbmc.Info != nil {
		cores := dbmc.GetIntInfo(cwma.MachineCPUCoreCount)
		if cores != nil {
			apimc.Cores = cores
		}
		threads := dbmc.GetIntInfo(cwma.MachineCPUThreadCount)
		if threads != nil {
			apimc.Threads = threads
		}
	}

	return apimc
}
