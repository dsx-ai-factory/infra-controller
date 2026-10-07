// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package model

import (
	"fmt"
	"time"

	cutil "github.com/NVIDIA/infra-controller/rest-api/common/pkg/util"
	cdbm "github.com/NVIDIA/infra-controller/rest-api/db/pkg/db/model"
	corev1 "github.com/NVIDIA/infra-controller/rest-api/proto/core/gen/v1"
	validation "github.com/go-ozzo/ozzo-validation/v4"
	validationis "github.com/go-ozzo/ozzo-validation/v4/is"
	"google.golang.org/protobuf/proto"
)

// APISku is the data structure to capture API representation of a SKU
type APISku struct {
	// ID is the unique identifier for the SKU
	ID string `json:"id"`
	// SiteID is the ID of the Site this SKU belongs to
	SiteID string `json:"siteId"`
	// Description is the human-readable SKU description
	Description string `json:"description"`
	// SchemaVersion is the Core SKU schema version when known.
	// When creating a new SKU or updating the components of an existing SKU,
	// the schema version must be the current schema version.
	SchemaVersion uint32 `json:"schemaVersion"`
	// DeviceType is the optional device type identifier
	DeviceType *string `json:"deviceType"`
	// AssociatedMachineIds is the list of machine IDs associated with this SKU
	AssociatedMachineIds []string `json:"associatedMachineIds"`
	// Components contains the hardware components of this SKU
	Components *APISkuComponents `json:"components"`
	// Created is the date and time the entity was created
	Created *time.Time `json:"created"`
}

// APISkuCreateRequest is the POST /sku request body.
type APISkuCreateRequest struct {
	// SiteID is the Site whose Core service will own the SKU.
	SiteID string `json:"siteId"`
	// ID is the unique SKU identifier.
	ID string `json:"id"`
	// Description is the human-readable SKU description.
	Description *string `json:"description"`
	// DeviceType is the optional device type identifier.
	DeviceType *string `json:"deviceType,omitempty"`
	// Components is the expected hardware configuration.
	Components *APISkuComponents `json:"components"`
}

// APISkuUpdateRequest is the PATCH /sku/:id request body.
type APISkuUpdateRequest struct {
	// SkuID is populated from the request path before proto conversion.
	SkuID string `json:"-"`
	// Description replaces the description when provided.
	Description *string `json:"description,omitempty"`
	// DeviceType replaces the device type when provided.
	DeviceType *string `json:"deviceType,omitempty"`
	// Components replaces the hardware configuration when provided.
	Components *APISkuComponents `json:"components,omitempty"`
}

// CoreSkuSchemaVersion is the Core wire format emitted by REST SKU mutations.
const CoreSkuSchemaVersion uint32 = 5

// Validate checks the create request before conversion to Core protobufs.
func (ascr APISkuCreateRequest) Validate() error {
	return validation.ValidateStruct(&ascr,
		validation.Field(&ascr.SiteID,
			validation.Required.Error(validationErrorValueRequired),
			validationis.UUID.Error(validationErrorInvalidUUID)),
		validation.Field(&ascr.ID, validation.Required.Error(validationErrorValueRequired)),
		validation.Field(&ascr.Components, validation.Required.Error(validationErrorValueRequired)),
	)
}

// ToProto converts a validated create request into Core's single-item SkuList.
func (ascr APISkuCreateRequest) ToProto() *corev1.SkuList {
	return &corev1.SkuList{Skus: []*corev1.Sku{{
		Id:            ascr.ID,
		Description:   ascr.Description,
		SchemaVersion: CoreSkuSchemaVersion,
		DeviceType:    ascr.DeviceType,
		Components:    ascr.Components.ToProto(),
	}}}
}

// Validate checks the update request and requires at least one mutable field.
func (asur APISkuUpdateRequest) Validate() error {
	if asur.Description == nil && asur.DeviceType == nil && asur.Components == nil {
		return validation.Errors{"request": validation.NewError("validation_required", "at least one mutable field is required")}
	}
	return validation.ValidateStruct(&asur,
		validation.Field(&asur.Components),
	)
}

// ToMetadataProto converts a validated metadata-only PATCH request.
func (asur APISkuUpdateRequest) ToMetadataProto() *corev1.SkuUpdateMetadataRequest {
	return &corev1.SkuUpdateMetadataRequest{
		SkuId:       asur.SkuID,
		Description: asur.Description,
		DeviceType:  asur.DeviceType,
	}
}

// ApplyMetadataToProto merges metadata fields into a copy of the current Core SKU.
func (asur APISkuUpdateRequest) ApplyMetadataToProto(current *corev1.Sku) *corev1.Sku {
	updated := proto.Clone(current).(*corev1.Sku)
	updated.Id = asur.SkuID
	if asur.Description != nil {
		updated.Description = asur.Description
	}
	if asur.DeviceType != nil {
		updated.DeviceType = asur.DeviceType
	}
	return updated
}

// ToReplacementProto converts a component PATCH into Core's current SKU format.
func (asur APISkuUpdateRequest) ToReplacementProto(current *corev1.Sku) *corev1.Sku {
	updated := asur.ApplyMetadataToProto(current)
	updated.SchemaVersion = CoreSkuSchemaVersion
	if asur.Components != nil {
		updated.Components = asur.Components.ToProto()
	}
	return updated
}

// NewAPISkuFromProto converts a Core SKU into the REST API representation.
func NewAPISkuFromProto(sku *corev1.Sku, siteID string) *APISku {
	if sku == nil {
		return nil
	}
	response := &APISku{
		ID:                   sku.Id,
		SiteID:               siteID,
		Description:          sku.GetDescription(),
		SchemaVersion:        sku.SchemaVersion,
		DeviceType:           sku.DeviceType,
		AssociatedMachineIds: []string{},
		Components:           NewAPISkuComponents(sku.Components),
	}
	for _, machineID := range sku.AssociatedMachineIds {
		id := machineID.GetId()
		if id != "" {
			response.AssociatedMachineIds = append(response.AssociatedMachineIds, id)
		}
	}
	if sku.Created != nil {
		created := sku.Created.AsTime()
		response.Created = &created
	}
	return response
}

// NewAPISkuFromCreateRequest builds the best-known response
// after Core accepted a create request but the post-create read failed.
func NewAPISkuFromCreateRequest(req APISkuCreateRequest, skuID, siteID string) *APISku {
	description := ""
	if req.Description != nil {
		description = *req.Description
	}
	return &APISku{
		ID:                   skuID,
		SiteID:               siteID,
		Description:          description,
		SchemaVersion:        CoreSkuSchemaVersion,
		DeviceType:           req.DeviceType,
		AssociatedMachineIds: []string{},
		Components:           NewAPISkuComponents(req.Components.ToProto()),
		Created:              cutil.GetPtr(time.Now().UTC()), // best guess in absence of data from Core
	}
}

// NewAPISku accepts a DB layer SKU object and returns an API layer object
func NewAPISku(dbSku *cdbm.SKU) *APISku {
	if dbSku == nil {
		return nil
	}

	apiSku := &APISku{
		ID:                   dbSku.ID,
		SiteID:               dbSku.SiteID.String(),
		Description:          dbSku.Description,
		SchemaVersion:        dbSku.SchemaVersion,
		DeviceType:           dbSku.DeviceType,
		AssociatedMachineIds: dbSku.AssociatedMachineIds,
		Created:              &dbSku.Created,
	}

	// Map SKU Components if available
	if dbSku.Components != nil && dbSku.Components.SkuComponents != nil {
		apiSku.Components = NewAPISkuComponents(dbSku.Components.SkuComponents)
	}

	return apiSku
}

type APISkuComponents struct {
	// Cpus describes CPU components
	Cpus []APISkuCpu `json:"cpus"`
	// Gpus describes GPU components
	Gpus []APISkuGpu `json:"gpus"`
	// Memory describes memory components
	Memory []APISkuMemory `json:"memory"`
	// Storage describes storage components
	Storage []APISkuStorage `json:"storage"`
	// Chassis describes chassis component
	Chassis *APISkuChassis `json:"chassis"`
	// EthernetDevices describes read-only ethernet device components.
	// REST mutation requests may omit it or provide an empty value.
	EthernetDevices []APISkuEthernetDevice `json:"ethernetDevices"`
	// InfinibandDevices describes infiniband device components
	InfinibandDevices []APISkuInfinibandDevice `json:"infinibandDevices"`
	// Tpm describes TPM components
	Tpm *APISkuTpm `json:"tpm"`
}

// Validate rejects unsupported component mutations and checks every storage component.
func (c APISkuComponents) Validate() error {
	return validation.ValidateStruct(&c,
		validation.Field(
			&c.EthernetDevices,
			validation.Length(0, 0).Error("must be empty because this field is read-only"),
		),
		validation.Field(&c.Storage, validation.Each()),
	)
}

// APISkuCpu represents a CPU component in the SKU
type APISkuCpu struct {
	// Vendor describes the vendor of the CPU
	Vendor string `json:"vendor"`
	// Model describes the model of the CPU
	Model string `json:"model"`
	// ThreadCount describes the number of threads for the CPU
	ThreadCount uint32 `json:"threadCount"`
	// Count describes the number of CPUs present
	Count uint32 `json:"count"`
}

// APISkuGpu represents a GPU component in the SKU
type APISkuGpu struct {
	// Vendor describes the vendor of the GPU
	Vendor string `json:"vendor"`
	// Model describes the model of the GPU
	Model string `json:"model"`
	// TotalMemory describes the total memory of the GPU
	TotalMemory string `json:"totalMemory"`
	// Count describes the number of GPUs present
	Count uint32 `json:"count"`
}

// APISkuMemory represents a memory component in the SKU
type APISkuMemory struct {
	// CapacityMb describes the capacity in megabytes
	CapacityMb uint32 `json:"capacityMb"`
	// MemoryType describes the type of memory (e.g., DDR4, DDR5)
	MemoryType string `json:"memoryType"`
	// Count describes the number of memory modules present
	Count uint32 `json:"count"`
}

// APISkuStorage represents a storage component in the SKU
type APISkuStorage struct {
	// Vendor participates in storage matching for schema version 4 SKUs. It is
	// read-only in REST mutation requests because component mutations use the
	// current schema version.
	Vendor *string `json:"vendor"`
	// Model is informational starting with the 2.1 release.
	Model string `json:"model"`
	// CapacityMb participates in storage matching for schema version 4 SKUs. It
	// is read-only in REST mutation requests because component mutations use the
	// current schema version.
	CapacityMb *uint32 `json:"capacityMb"`
	// Count describes the number of storage devices present
	Count uint32 `json:"count"`
	// MinSizeMiB is the inclusive minimum capacity in mebibytes for each storage device.
	// It is only used for matching for SKUs of schema version 5 and onwards.
	MinSizeMiB *uint32 `json:"minSizeMiB"`
	// MaxSizeMiB is the inclusive maximum capacity in mebibytes for each storage device.
	// It is only used for matching for SKUs of schema version 5 and onwards.
	MaxSizeMiB *uint32 `json:"maxSizeMiB"`
	// PciPatterns contains regular expressions matched against storage PCI paths.
	// It is only used for matching for SKUs of schema version 5 and onwards.
	PciPatterns []string `json:"pciPatterns"`
}

// Validate rejects read-only fields and invalid size bounds.
func (s APISkuStorage) Validate() error {
	errs := validation.Errors{}
	if s.Vendor != nil {
		errs["vendor"] = fmt.Errorf("is a read-only SKU storage field")
	}
	if s.CapacityMb != nil {
		errs["capacityMb"] = fmt.Errorf("is a read-only SKU storage field")
	}
	if s.MinSizeMiB != nil && s.MaxSizeMiB != nil && *s.MinSizeMiB > *s.MaxSizeMiB {
		errs["minSizeMiB"] = fmt.Errorf("must be less than or equal to maxSizeMiB")
	}
	if len(errs) == 0 {
		return nil
	}
	return errs
}

// APISkuChassis represents the chassis component in the SKU
type APISkuChassis struct {
	// Vendor describes the vendor of the chassis
	Vendor string `json:"vendor"`
	// Model describes the model of the chassis
	Model string `json:"model"`
	// Architecture describes the chassis architecture.
	Architecture string `json:"architecture"`
}

// APISkuEthernetDevice represents an ethernet device component in the SKU
type APISkuEthernetDevice struct {
	// Vendor describes the vendor of the ethernet device
	Vendor string `json:"vendor"`
	// Model describes the model of the ethernet device
	Model string `json:"model"`
	// Count describes the number of ethernet devices present
	Count uint32 `json:"count"`
	// IsConnected reports whether the Ethernet device is connected.
	IsConnected bool `json:"isConnected"`
}

// APISkuInfinibandDevice represents an infiniband device component in the SKU
type APISkuInfinibandDevice struct {
	// Vendor describes the vendor of the infiniband device
	Vendor string `json:"vendor"`
	// Model describes the model of the infiniband device
	Model string `json:"model"`
	// Count describes the number of infiniband devices present
	Count uint32 `json:"count"`
	// InactiveDevices contains zero-based indexes of inactive devices.
	InactiveDevices []uint32 `json:"inactiveDevices"`
}

// APISkuTpm represents a TPM component in the SKU
type APISkuTpm struct {
	// Vendor describes the vendor of the TPM
	Vendor string `json:"vendor"`
	// Version describes the version of the TPM
	Version string `json:"version"`
}

// NewAPISkuComponents converts proto SkuComponents to API SkuComponents.
func NewAPISkuComponents(protoComponents *corev1.SkuComponents) *APISkuComponents {
	if protoComponents == nil {
		return nil
	}

	components := &APISkuComponents{}
	for _, cpu := range protoComponents.Cpus {
		components.Cpus = append(components.Cpus, APISkuCpu{
			Vendor:      cpu.Vendor,
			Model:       cpu.Model,
			ThreadCount: cpu.ThreadCount,
			Count:       cpu.Count,
		})
	}
	for _, gpu := range protoComponents.Gpus {
		components.Gpus = append(components.Gpus, APISkuGpu{
			Vendor:      gpu.Vendor,
			Model:       gpu.Model,
			TotalMemory: gpu.TotalMemory,
			Count:       gpu.Count,
		})
	}
	for _, memory := range protoComponents.Memory {
		components.Memory = append(components.Memory, APISkuMemory{
			CapacityMb: memory.CapacityMb,
			MemoryType: memory.MemoryType,
			Count:      memory.Count,
		})
	}
	for _, storage := range protoComponents.Storage {
		vendor := storage.Vendor
		capacityMb := storage.CapacityMb
		components.Storage = append(components.Storage, APISkuStorage{
			Vendor:      &vendor,
			Model:       storage.Model,
			CapacityMb:  &capacityMb,
			Count:       storage.Count,
			MinSizeMiB:  storage.MinSizeMb,
			MaxSizeMiB:  storage.MaxSizeMb,
			PciPatterns: storage.PciPatterns,
		})
	}
	if protoComponents.Chassis != nil {
		components.Chassis = &APISkuChassis{
			Vendor:       protoComponents.Chassis.Vendor,
			Model:        protoComponents.Chassis.Model,
			Architecture: protoComponents.Chassis.Architecture,
		}
	}
	for _, ethernet := range protoComponents.EthernetDevices {
		components.EthernetDevices = append(components.EthernetDevices, APISkuEthernetDevice{
			Vendor:      ethernet.Vendor,
			Model:       ethernet.Model,
			Count:       ethernet.Count,
			IsConnected: ethernet.IsConnected,
		})
	}
	for _, infiniband := range protoComponents.InfinibandDevices {
		components.InfinibandDevices = append(components.InfinibandDevices, APISkuInfinibandDevice{
			Vendor:          infiniband.Vendor,
			Model:           infiniband.Model,
			Count:           infiniband.Count,
			InactiveDevices: infiniband.InactiveDevices,
		})
	}
	if protoComponents.Tpm != nil {
		components.Tpm = &APISkuTpm{
			Vendor:  protoComponents.Tpm.Vendor,
			Version: protoComponents.Tpm.Version,
		}
	}
	return components
}

// ToProto converts API SKU components into the Core protobuf shape.
func (c *APISkuComponents) ToProto() *corev1.SkuComponents {
	if c == nil {
		return nil
	}
	components := &corev1.SkuComponents{}
	if c.Chassis != nil {
		components.Chassis = &corev1.SkuComponentChassis{
			Vendor:       c.Chassis.Vendor,
			Model:        c.Chassis.Model,
			Architecture: c.Chassis.Architecture,
		}
	}
	for _, cpu := range c.Cpus {
		components.Cpus = append(components.Cpus, &corev1.SkuComponentCpu{
			Vendor:      cpu.Vendor,
			Model:       cpu.Model,
			ThreadCount: cpu.ThreadCount,
			Count:       cpu.Count,
		})
	}
	for _, gpu := range c.Gpus {
		components.Gpus = append(components.Gpus, &corev1.SkuComponentGpu{
			Vendor:      gpu.Vendor,
			Model:       gpu.Model,
			TotalMemory: gpu.TotalMemory,
			Count:       gpu.Count,
		})
	}
	for _, memory := range c.Memory {
		components.Memory = append(components.Memory, &corev1.SkuComponentMemory{
			CapacityMb: memory.CapacityMb,
			MemoryType: memory.MemoryType,
			Count:      memory.Count,
		})
	}
	for _, storage := range c.Storage {
		components.Storage = append(components.Storage, &corev1.SkuComponentStorage{
			Model:       storage.Model,
			Count:       storage.Count,
			MinSizeMb:   storage.MinSizeMiB,
			MaxSizeMb:   storage.MaxSizeMiB,
			PciPatterns: storage.PciPatterns,
		})
	}
	for _, ethernet := range c.EthernetDevices {
		components.EthernetDevices = append(components.EthernetDevices, &corev1.SkuComponentEthernetDevices{
			Vendor:      ethernet.Vendor,
			Model:       ethernet.Model,
			Count:       ethernet.Count,
			IsConnected: ethernet.IsConnected,
		})
	}
	for _, infiniband := range c.InfinibandDevices {
		components.InfinibandDevices = append(components.InfinibandDevices, &corev1.SkuComponentInfinibandDevices{
			Vendor:          infiniband.Vendor,
			Model:           infiniband.Model,
			Count:           infiniband.Count,
			InactiveDevices: infiniband.InactiveDevices,
		})
	}
	if c.Tpm != nil {
		components.Tpm = &corev1.SkuComponentTpm{
			Vendor:  c.Tpm.Vendor,
			Version: c.Tpm.Version,
		}
	}
	return components
}

// APISkuSummary is the data structure to capture summary of a SKU
type APISkuSummary struct {
	// ID is the unique identifier for the SKU
	ID string `json:"id"`
	// SiteID is the ID of the Site this SKU belongs to
	SiteID string `json:"siteId"`
	// DeviceType is the optional device type identifier
	DeviceType *string `json:"deviceType"`
}

// NewAPISkuSummary accepts a DB layer SKU object and returns an API layer summary object
func NewAPISkuSummary(dbSku *cdbm.SKU) *APISkuSummary {
	if dbSku == nil {
		return nil
	}

	return &APISkuSummary{
		ID:         dbSku.ID,
		SiteID:     dbSku.SiteID.String(),
		DeviceType: dbSku.DeviceType,
	}
}
