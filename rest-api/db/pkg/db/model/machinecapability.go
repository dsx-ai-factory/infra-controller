// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package model

import (
	"context"
	"database/sql"
	"encoding/json"
	"errors"
	"fmt"
	"slices"
	"strings"
	"time"

	cutil "github.com/NVIDIA/infra-controller/rest-api/common/pkg/util"
	"github.com/NVIDIA/infra-controller/rest-api/db/pkg/db"
	"github.com/NVIDIA/infra-controller/rest-api/db/pkg/db/paginator"
	validation "github.com/go-ozzo/ozzo-validation/v4"
	"github.com/google/uuid"
	"github.com/rs/zerolog/log"

	"github.com/uptrace/bun"

	stracer "github.com/NVIDIA/infra-controller/rest-api/db/pkg/tracer"
	corev1 "github.com/NVIDIA/infra-controller/rest-api/proto/core/gen/v1"
)

// MachineCapabilityType is the domain enum for the kind of capability a
// `MachineCapability` describes. Defining it as a named string lets us
// hang the workflow-proto conversion on it as methods (`(t).ToProto`,
// `(*t).FromProto`), and keeps the DB column comparable as a plain
// string at the storage layer.
type MachineCapabilityType string

// MachineCapabilityType values. Stored as plain strings in the DB
// column `machine_capability.type`.
const (
	MachineCapabilityTypeCPU        MachineCapabilityType = "CPU"
	MachineCapabilityTypeMemory     MachineCapabilityType = "Memory"
	MachineCapabilityTypeGPU        MachineCapabilityType = "GPU"
	MachineCapabilityTypeStorage    MachineCapabilityType = "Storage"
	MachineCapabilityTypeNetwork    MachineCapabilityType = "Network"
	MachineCapabilityTypeInfiniBand MachineCapabilityType = "InfiniBand"
	MachineCapabilityTypeDPU        MachineCapabilityType = "DPU"
)

// MachineCapabilityDeviceType is the domain enum for the device class a
// capability targets when the capability type alone is ambiguous (e.g.
// a Network capability against a DPU vs. against an NVLink). Defined
// as a named string for the same reason as `MachineCapabilityType`.
type MachineCapabilityDeviceType string

// MachineCapabilityDeviceType values. Stored as plain strings in the DB
// column `machine_capability.device_type`.
const (
	MachineCapabilityDeviceTypeDPU     MachineCapabilityDeviceType = "DPU"
	MachineCapabilityDeviceTypeNVLink  MachineCapabilityDeviceType = "NVLink"
	MachineCapabilityDeviceTypeSPX     MachineCapabilityDeviceType = "SPX"
	MachineCapabilityDeviceTypeUnknown MachineCapabilityDeviceType = "Unknown"
)

const (
	// MachineCapabilityRelationName is the relation name for the MachineCapability model
	MachineCapabilityRelationName = "MachineCapability"

	// MachineCapabilityOrderByDefault default field to be used for ordering when none specified
	MachineCapabilityOrderByDefault = "created"
)

var (

	// MachineCapabilityTypeChoiceMap is a map of valid MachineCapability types
	MachineCapabilityTypeChoiceMap = map[MachineCapabilityType]bool{
		MachineCapabilityTypeCPU:        true,
		MachineCapabilityTypeMemory:     true,
		MachineCapabilityTypeGPU:        true,
		MachineCapabilityTypeStorage:    true,
		MachineCapabilityTypeNetwork:    true,
		MachineCapabilityTypeInfiniBand: true,
		MachineCapabilityTypeDPU:        true,
	}

	// MachineCapabilityOrderByFields is a list of valid order by fields for the MachineCapability model
	MachineCapabilityOrderByFields = []string{"type", "created", "updated"}

	// MachineCapabilityDeviceTypeChoiceMap is a map of valid MachineCapability device types
	MachineCapabilityDeviceTypeChoiceMap = map[MachineCapabilityDeviceType]bool{
		MachineCapabilityDeviceTypeDPU:    true,
		MachineCapabilityDeviceTypeNVLink: true,
		MachineCapabilityDeviceTypeSPX:    true,
	}
)

// ToProto converts a `MachineCapabilityType` into its workflow proto
// enum. Returns the zero proto value (`MACHINE_CAPABILITY_TYPE_UNSPECIFIED`)
// for an unknown DB value with a warning logged; `MachineCapability.Validate`
// is the gate that rejects such values upstream of the wire.
func (t MachineCapabilityType) ToProto() corev1.MachineCapabilityType {
	switch t {
	case MachineCapabilityTypeCPU:
		return corev1.MachineCapabilityType_CAP_TYPE_CPU
	case MachineCapabilityTypeMemory:
		return corev1.MachineCapabilityType_CAP_TYPE_MEMORY
	case MachineCapabilityTypeGPU:
		return corev1.MachineCapabilityType_CAP_TYPE_GPU
	case MachineCapabilityTypeStorage:
		return corev1.MachineCapabilityType_CAP_TYPE_STORAGE
	case MachineCapabilityTypeNetwork:
		return corev1.MachineCapabilityType_CAP_TYPE_NETWORK
	case MachineCapabilityTypeInfiniBand:
		return corev1.MachineCapabilityType_CAP_TYPE_INFINIBAND
	case MachineCapabilityTypeDPU:
		return corev1.MachineCapabilityType_CAP_TYPE_DPU
	}
	log.Warn().Str("Type", string(t)).Msg("unsupported MachineCapabilityType requested")
	return corev1.MachineCapabilityType(0)
}

// FromProto populates the receiver from a workflow proto enum,
// mirroring `(MachineCapabilityType).ToProto`. An unknown proto enum
// silently leaves the receiver as the empty string; `(*MachineCapability).Validate`
// rejects that downstream.
func (t *MachineCapabilityType) FromProto(p corev1.MachineCapabilityType) {
	switch p {
	case corev1.MachineCapabilityType_CAP_TYPE_CPU:
		*t = MachineCapabilityTypeCPU
	case corev1.MachineCapabilityType_CAP_TYPE_MEMORY:
		*t = MachineCapabilityTypeMemory
	case corev1.MachineCapabilityType_CAP_TYPE_GPU:
		*t = MachineCapabilityTypeGPU
	case corev1.MachineCapabilityType_CAP_TYPE_STORAGE:
		*t = MachineCapabilityTypeStorage
	case corev1.MachineCapabilityType_CAP_TYPE_NETWORK:
		*t = MachineCapabilityTypeNetwork
	case corev1.MachineCapabilityType_CAP_TYPE_INFINIBAND:
		*t = MachineCapabilityTypeInfiniBand
	case corev1.MachineCapabilityType_CAP_TYPE_DPU:
		*t = MachineCapabilityTypeDPU
	default:
		log.Warn().Str("MachineCapabilityType", p.String()).Msg("unsupported MachineCapabilityType requested")
		*t = ""
	}
}

// ToProto converts a `MachineCapabilityDeviceType` into its workflow
// proto enum. Returns the zero proto value with a warning logged for
// unknown DB values; `MachineCapability.Validate` is the upstream gate.
func (d MachineCapabilityDeviceType) ToProto() corev1.MachineCapabilityDeviceType {
	switch d {
	case MachineCapabilityDeviceTypeDPU:
		return corev1.MachineCapabilityDeviceType_MACHINE_CAPABILITY_DEVICE_TYPE_DPU
	case MachineCapabilityDeviceTypeNVLink:
		return corev1.MachineCapabilityDeviceType_MACHINE_CAPABILITY_DEVICE_TYPE_NVLINK
	case MachineCapabilityDeviceTypeSPX:
		return corev1.MachineCapabilityDeviceType_MACHINE_CAPABILITY_DEVICE_TYPE_SPX
	}
	log.Warn().Str("DeviceType", string(d)).Msg("unsupported MachineCapabilityDeviceType requested")
	return corev1.MachineCapabilityDeviceType(0)
}

// FromProto populates the receiver from a workflow proto enum,
// mirroring `(MachineCapabilityDeviceType).ToProto`. An unknown proto
// enum leaves the receiver as the empty string with a warning logged.
func (d *MachineCapabilityDeviceType) FromProto(p corev1.MachineCapabilityDeviceType) {
	switch p {
	case corev1.MachineCapabilityDeviceType_MACHINE_CAPABILITY_DEVICE_TYPE_DPU:
		*d = MachineCapabilityDeviceTypeDPU
	case corev1.MachineCapabilityDeviceType_MACHINE_CAPABILITY_DEVICE_TYPE_NVLINK:
		*d = MachineCapabilityDeviceTypeNVLink
	case corev1.MachineCapabilityDeviceType_MACHINE_CAPABILITY_DEVICE_TYPE_SPX:
		*d = MachineCapabilityDeviceTypeSPX
	default:
		log.Warn().Str("DeviceType", p.String()).Msg("unsupported MachineCapabilityDeviceType requested")
		*d = ""
	}
}

// MachineCapabilityCreateInput input parameters for Create method
type MachineCapabilityCreateInput struct {
	MachineID        *string
	InstanceTypeID   *uuid.UUID
	Type             MachineCapabilityType
	Name             string
	Frequency        *string
	Capacity         *string
	HardwareRevision *string
	Cores            *int
	Threads          *int
	Vendor           *string
	Count            *int
	DeviceType       *MachineCapabilityDeviceType
	InactiveDevices  []int
	Index            int
	Info             map[string]interface{}
}

// MachineCapabilityUpdateInput input parameters for Update method
type MachineCapabilityUpdateInput struct {
	ID               uuid.UUID
	MachineID        *string
	InstanceTypeID   *uuid.UUID
	Type             *MachineCapabilityType
	Name             *string
	Frequency        *string
	Capacity         *string
	HardwareRevision *string
	Cores            *int
	Threads          *int
	Vendor           *string
	Count            *int
	DeviceType       *MachineCapabilityDeviceType
	InactiveDevices  []int
	Index            *int
	Info             map[string]interface{}
}

// MachineCapability represents entries in the machine_capability table
// It describes capabilities of a Machine
type MachineCapability struct {
	bun.BaseModel `bun:"table:machine_capability,alias:mc"`

	ID               uuid.UUID                    `bun:"type:uuid,pk"`
	MachineID        *string                      `bun:"machine_id"`
	InstanceTypeID   *uuid.UUID                   `bun:"instance_type_id,type:uuid"`
	InstanceType     *InstanceType                `bun:"rel:belongs-to,join:instance_type_id=id"`
	Type             MachineCapabilityType        `bun:"type,notnull"`
	Name             string                       `bun:"name,notnull"`
	Frequency        *string                      `bun:"frequency"`
	Capacity         *string                      `bun:"capacity"`
	HardwareRevision *string                      `bun:"hardware_revision"`
	Cores            *int                         `bun:"cores"`
	Threads          *int                         `bun:"threads"`
	Vendor           *string                      `bun:"vendor"`
	Count            *int                         `bun:"count"`
	DeviceType       *MachineCapabilityDeviceType `bun:"device_type"`
	InactiveDevices  []int                        `bun:"inactive_devices"`
	Index            int                          `bun:"index"`
	Info             map[string]interface{}       `bun:"info,json_use_number"` // Any other attribute of the capability
	Created          time.Time                    `bun:"created,nullzero,notnull,default:current_timestamp"`
	Updated          time.Time                    `bun:"updated,nullzero,notnull,default:current_timestamp"`
	Deleted          *time.Time                   `bun:"deleted,soft_delete"`

	// Deprecated fields: To be deleted
	ValueStr    *string `bun:"value_str"`
	ValueInt    *int    `bun:"value_int"`
	Description *string `bun:"description"`
}

// Equal reports whether two `*MachineCapability` records carry the same
// proto-relevant fields. Used by the cloud-sync activity to skip
// no-op DAO updates when the cloud copy matches what the Site
// reported. Declared as a method (per the Go convention used by
// `time.Time.Equal`, `cmp.Equal`, etc.) so callers can write
// `mc.Equal(other)` instead of reaching for a free helper.
//
// Compares the wire-relevant fields only: `MachineID`, `InstanceTypeID`,
// `ID`, `Created`/`Updated`/`Deleted` and the deprecated `ValueStr` /
// `ValueInt` / `Description` are deliberately excluded.
func (mc *MachineCapability) Equal(other *MachineCapability) bool {
	if mc == nil || other == nil {
		return mc == other
	}
	return mcPtrsEqual(mc.Cores, other.Cores) &&
		mcPtrsEqual(mc.Threads, other.Threads) &&
		mcPtrsEqual(mc.Count, other.Count) &&
		mcPtrsEqual(mc.DeviceType, other.DeviceType) &&
		mc.Name == other.Name &&
		mc.Type == other.Type &&
		mcPtrsEqual(mc.Capacity, other.Capacity) &&
		mcPtrsEqual(mc.Frequency, other.Frequency) &&
		mcPtrsEqual(mc.HardwareRevision, other.HardwareRevision) &&
		mcPtrsEqual(mc.Vendor, other.Vendor) &&
		slices.Equal(mc.InactiveDevices, other.InactiveDevices) &&
		mc.Index == other.Index
}

// mcPtrsEqual is a file-local copy of the workflow util `PtrsEqual`
// helper, declared here so `(*MachineCapability).Equal` can compare
// pointer-typed fields without taking a dependency on the workflow
// layer. The signature mirrors `util.PtrsEqual` exactly.
func mcPtrsEqual[T comparable](a, b *T) bool {
	if (a == nil) != (b == nil) {
		return false
	}
	if a == nil {
		return true
	}
	return *a == *b
}

// ToProto converts this MachineCapability to its workflow proto
// representation used in InstanceType filter attributes.
//
// Per the proto-conversion convention, this is a pure mapper and does
// not return errors. Per-enum mapping lives on `MachineCapabilityType`
// and `MachineCapabilityDeviceType`; numeric width casts trust the
// request-side `Validate` upstream.
func (mc *MachineCapability) ToProto() *corev1.InstanceTypeMachineCapabilityFilterAttributes {
	var inactiveDevices *corev1.Uint32List
	if mc.InactiveDevices != nil {
		inactiveDevices = &corev1.Uint32List{}
		for _, d := range mc.InactiveDevices {
			u := cutil.IntPtrToUint32Ptr(&d)
			inactiveDevices.Items = append(inactiveDevices.Items, *u)
		}
	}

	var deviceType *corev1.MachineCapabilityDeviceType
	if mc.DeviceType != nil {
		if dt := mc.DeviceType.ToProto(); dt != corev1.MachineCapabilityDeviceType_MACHINE_CAPABILITY_DEVICE_TYPE_UNKNOWN {
			deviceType = &dt
		}
	}

	return &corev1.InstanceTypeMachineCapabilityFilterAttributes{
		CapabilityType:   mc.Type.ToProto(),
		Name:             &mc.Name,
		Frequency:        mc.Frequency,
		Capacity:         mc.Capacity,
		Vendor:           mc.Vendor,
		Count:            cutil.IntPtrToUint32Ptr(mc.Count),
		DeviceType:       deviceType,
		HardwareRevision: mc.HardwareRevision,
		InactiveDevices:  inactiveDevices,
		Cores:            cutil.IntPtrToUint32Ptr(mc.Cores),
		Threads:          cutil.IntPtrToUint32Ptr(mc.Threads),
	}
}

// FromProto populates this MachineCapability from workflow proto filter
// attributes. idx is the per-InstanceType ordering index (NICo rejects
// updates that re-order capabilities).
//
// Per the proto-conversion convention, this is a pure mapper and does
// not return errors. Per-enum mapping lives on `MachineCapabilityType`
// and `MachineCapabilityDeviceType`. An unknown CapabilityType or nil
// Name silently leaves Type / Name as their zero values; callers that
// need to reject such cases should call `(*MachineCapability).Validate()`
// after `FromProto`. A nil attrs is a no-op (receiver untouched).
func (mc *MachineCapability) FromProto(attrs *corev1.InstanceTypeMachineCapabilityFilterAttributes, idx int) {
	if attrs == nil {
		return
	}

	mc.Type.FromProto(attrs.CapabilityType)

	var name string
	if attrs.Name != nil {
		name = *attrs.Name
	}

	var inactiveDevices []int
	if attrs.InactiveDevices != nil {
		for _, d := range attrs.InactiveDevices.Items {
			inactiveDevices = append(inactiveDevices, int(d))
		}
	}

	var deviceType *MachineCapabilityDeviceType
	if attrs.DeviceType != nil {
		var dt MachineCapabilityDeviceType
		dt.FromProto(*attrs.DeviceType)
		// Preserve presence even when the proto enum is unsupported
		// (dt stays the empty string) so the post-`FromProto`
		// `Validate()` can reject it instead of silently dropping it.
		deviceType = &dt
	}

	mc.Name = name
	mc.Frequency = attrs.Frequency
	mc.Capacity = attrs.Capacity
	mc.Vendor = attrs.Vendor
	mc.Count = cutil.Uint32PtrToIntPtr(attrs.Count)
	mc.HardwareRevision = attrs.HardwareRevision
	mc.Cores = cutil.Uint32PtrToIntPtr(attrs.Cores)
	mc.Threads = cutil.Uint32PtrToIntPtr(attrs.Threads)
	mc.InactiveDevices = inactiveDevices
	mc.DeviceType = deviceType
	mc.Index = idx
}

// Validate checks that the populated MachineCapability is wire-safe
// for the workflow proto conversion. Pairs with `FromProto`: callers
// that build a MachineCapability from site-supplied proto data should
// run `Validate` afterwards to reject unknown CapabilityType enums
// (which `FromProto` leaves as the empty Type), missing Names, and
// cross-field combinations the wire shape can't represent — DeviceType
// must pair with Network (DPU or SPX) or GPU (NVLink), and InactiveDevices is
// only valid on InfiniBand. These mirror the API-side
// `(APIMachineCapability).Validate` rules so the workflow-inventory
// path and the API path enforce the same invariants.
func (mc *MachineCapability) Validate() error {
	mctypes := make([]any, 0, len(MachineCapabilityTypeChoiceMap))
	for mctype := range MachineCapabilityTypeChoiceMap {
		mctypes = append(mctypes, mctype)
	}
	return validation.ValidateStruct(mc,
		validation.Field(&mc.Type,
			validation.Required.Error("MachineCapability Type must be specified"),
			validation.In(mctypes...).Error(fmt.Sprintf("invalid MachineCapability Type: %q", mc.Type))),
		validation.Field(&mc.Name,
			validation.Required.Error("MachineCapability Name must be specified"),
			validation.Length(2, 256).Error("MachineCapability Name must be at least 2 characters and maximum 256 characters"),
			validation.By(mc.validateNameWhitespace)),
		validation.Field(&mc.DeviceType, validation.By(mc.validateDeviceType)),
		validation.Field(&mc.InactiveDevices, validation.By(mc.validateInactiveDevices)),
	)
}

// validateNameWhitespace rejects Names with leading or trailing
// whitespace, mirroring the API-side `util.ValidateNameCharacters`
// rule so the wire-bound DB-model gate matches the API one.
func (mc *MachineCapability) validateNameWhitespace(value interface{}) error {
	s, ok := value.(string)
	if !ok {
		return errors.New("MachineCapability Name must be a string")
	}
	if strings.TrimSpace(s) != s {
		return errors.New("MachineCapability Name must not contain leading or trailing whitespace")
	}
	return nil
}

// validateDeviceType enforces the Type/DeviceType compatibility rules:
// Network capabilities require DPU or SPX, GPU capabilities require NVLink,
// every other Type must not carry a DeviceType. A nil DeviceType is
// always allowed.
func (mc *MachineCapability) validateDeviceType(value interface{}) error {
	dt, ok := value.(*MachineCapabilityDeviceType)
	if !ok || dt == nil {
		return nil
	}
	switch mc.Type {
	case MachineCapabilityTypeNetwork:
		if *dt != MachineCapabilityDeviceTypeDPU && *dt != MachineCapabilityDeviceTypeSPX {
			return fmt.Errorf("Unsupported Device Type specified for Network Capability %s", *dt)
		}
	case MachineCapabilityTypeGPU:
		if *dt != MachineCapabilityDeviceTypeNVLink {
			return fmt.Errorf("Unsupported Device Type specified for GPU Capability %s", *dt)
		}
	default:
		return fmt.Errorf("Unsupported Device Type: %s specified for Capability type %s", *dt, mc.Type)
	}
	return nil
}

// validateInactiveDevices enforces that InactiveDevices is only set on
// InfiniBand capabilities.
func (mc *MachineCapability) validateInactiveDevices(value interface{}) error {
	ids, ok := value.([]int)
	if !ok || len(ids) == 0 {
		return nil
	}
	if mc.Type != MachineCapabilityTypeInfiniBand {
		return errors.New("InactiveDevices specified for non-InfiniBand Capability Type")
	}
	return nil
}

// MachineCapabilityMapKey returns the canonical identity key for a capability.
// Device type is part of the identity so a generic network capability and an
// SPX capability can share the same hardware description without colliding.
// The capability name is length-prefixed because names may contain colons; a
// delimiter-only encoding would otherwise make a typed capability ambiguous
// with a generic capability whose name ends in the device type. Nil and empty
// device types intentionally have the same identity.
func MachineCapabilityMapKey(capabilityType MachineCapabilityType, name string, deviceType *MachineCapabilityDeviceType) string {
	key := fmt.Sprintf("%s:%d:%s", capabilityType, len(name), name)
	if deviceType == nil || *deviceType == "" {
		return key
	}
	return fmt.Sprintf("%s:%s", key, *deviceType)
}

// MapKey returns the canonical `(Type, Name, DeviceType)` identity key.
func (mc *MachineCapability) MapKey() string {
	return MachineCapabilityMapKey(mc.Type, mc.Name, mc.DeviceType)
}

// GetStrInfo returns the string value of the given key in the Info map
func (mc *MachineCapability) GetStrInfo(name string) *string {
	if mc.Info == nil {
		return nil
	}
	info, ok := mc.Info[name]
	if !ok {
		return nil
	}
	strInfo, ok := info.(string)
	if !ok {
		return nil
	}

	return &strInfo
}

// GetIntInfo returns the integer value of the given key in the Info map
func (mc *MachineCapability) GetIntInfo(name string) *int {
	if mc.Info == nil {
		return nil
	}
	info, ok := mc.Info[name]
	if !ok {
		return nil
	}

	var intInfo int

	// When info is read from DB, the value is of type json.Number
	jnInfo, ok := info.(json.Number)
	if ok {
		int64Info, err := jnInfo.Int64()
		if err != nil {
			return nil
		}

		intInfo = int(int64Info)
	} else {
		// When info is read from a freshly map, the value should be of type int
		intInfo, ok = info.(int)
		if !ok {
			return nil
		}
	}

	return &intInfo
}

// TODO: Add follow up migration to remove description, value_str and value_int

// GetIndentedJSON returns formatted json of MachineCapability
func (mc *MachineCapability) GetIndentedJSON() ([]byte, error) {
	return json.MarshalIndent(mc, "", "  ")
}

var _ bun.BeforeAppendModelHook = (*MachineCapability)(nil)

// BeforeAppendModel is a hook that is called before the model is appended to the query
func (mc *MachineCapability) BeforeAppendModel(ctx context.Context, query bun.Query) error {
	switch query.(type) {
	case *bun.InsertQuery:
		mc.Created = db.GetCurTime()
		mc.Updated = db.GetCurTime()
	case *bun.UpdateQuery:
		mc.Updated = db.GetCurTime()
	}
	return nil
}

var _ bun.BeforeCreateTableHook = (*MachineCapability)(nil)

// BeforeCreateTable is a hook that is called before the table is created
// This is only used in tests
func (mc *MachineCapability) BeforeCreateTable(ctx context.Context,
	query *bun.CreateTableQuery) error {
	query.ForeignKey(`("machine_id") REFERENCES "machine" ("id")`).
		ForeignKey(`("instance_type_id") REFERENCES "instance_type" ("id")`)
	return nil
}

// MachineCapabilityDAO is an interface for interacting with the MachineCapability model
type MachineCapabilityDAO interface {
	//
	Create(ctx context.Context, tx *db.Tx, input MachineCapabilityCreateInput) (*MachineCapability, error)
	//
	GetByID(ctx context.Context, tx *db.Tx, id uuid.UUID, includeRelations []string) (*MachineCapability, error)
	//
	GetAll(ctx context.Context, tx *db.Tx, machineIDs []string, instanceTypeIDs []uuid.UUID, capabilityType *string,
		name *string, frequency *string, capacity *string, vendor *string,
		count *int, deviceType *string, inactiveDevices []int, includeRelations []string, offset *int, limit *int, orderBy *paginator.OrderBy) ([]MachineCapability, int, error)
	//
	GetGPUStatsBySite(ctx context.Context, tx *db.Tx, infrastructureProviderID *uuid.UUID, siteID *uuid.UUID) ([]GPUSiteStat, error)
	//
	GetAllDistinct(ctx context.Context, tx *db.Tx, machineIDs []string, instanceTypeID *uuid.UUID, capabilityType *string,
		name *string, frequency *string, capacity *string, vendor *string,
		count *int, deviceType *string, inactiveDevices []int, offset *int, limit *int, orderBy *paginator.OrderBy) ([]MachineCapability, int, error)
	//
	Update(ctx context.Context, tx *db.Tx, input MachineCapabilityUpdateInput) (*MachineCapability, error)
	//
	ClearFromParams(ctx context.Context, tx *db.Tx, id uuid.UUID,
		machineID, instanceTypeID, frequency, capacity, vendor, info bool) (*MachineCapability, error)
	//
	DeleteByID(ctx context.Context, tx *db.Tx, id uuid.UUID, purge bool) error
}

// MachineCapabilitySQLDAO is an implementation of the MachineCapabilityDAO interface
type MachineCapabilitySQLDAO struct {
	dbSession *db.Session
	MachineCapabilityDAO
	tracerSpan *stracer.TracerSpan
}

// Create creates a new MachineCapability from the given parameters
// The returned MachineCapability will not have any related structs filled in
// since there are 2 operations (INSERT, SELECT), in this, it is required that
// this library call happens within a transaction
func (mcd MachineCapabilitySQLDAO) Create(
	ctx context.Context, tx *db.Tx,
	input MachineCapabilityCreateInput) (*MachineCapability, error) {
	// Create a child span and set the attributes for current request
	ctx, MachineCapabilityDAOSpan := mcd.tracerSpan.CreateChildInCurrentContext(ctx, "MachineCapabilityDAO.Create")
	if MachineCapabilityDAOSpan != nil {
		defer MachineCapabilityDAOSpan.End()

		mcd.tracerSpan.SetAttribute(MachineCapabilityDAOSpan, "name", input.Name)
	}

	if len(strings.TrimSpace(string(input.Type))) == 0 {
		return nil, errors.New("capabilityType is empty")
	}
	if !MachineCapabilityTypeChoiceMap[input.Type] {
		return nil, fmt.Errorf("invalid capabilityType: %q", input.Type)
	}

	if input.MachineID == nil && input.InstanceTypeID == nil {
		return nil, errors.New("instanceTypeID or machineID needs to be specified")
	}

	m := &MachineCapability{
		ID:               uuid.New(),
		MachineID:        input.MachineID,
		InstanceTypeID:   input.InstanceTypeID,
		Type:             input.Type,
		Name:             input.Name,
		Frequency:        input.Frequency,
		Capacity:         input.Capacity,
		Vendor:           input.Vendor,
		Count:            input.Count,
		DeviceType:       input.DeviceType,
		Threads:          input.Threads,
		Cores:            input.Cores,
		HardwareRevision: input.HardwareRevision,
		Info:             input.Info,
		InactiveDevices:  input.InactiveDevices,
		Index:            input.Index,
	}

	_, err := db.GetIDB(tx, mcd.dbSession).NewInsert().Model(m).Exec(ctx)
	if err != nil {
		return nil, err
	}

	nv, err := mcd.GetByID(ctx, tx, m.ID, []string{"InstanceType"})
	if err != nil {
		return nil, err
	}

	return nv, nil
}

// GetByID returns a MachineCapability by ID
// returns db.ErrDoesNotExist error if the record is not found
func (mcd MachineCapabilitySQLDAO) GetByID(ctx context.Context, tx *db.Tx, id uuid.UUID, includeRelations []string) (*MachineCapability, error) {
	// Create a child span and set the attributes for current request
	ctx, MachineCapabilityDAOSpan := mcd.tracerSpan.CreateChildInCurrentContext(ctx, "MachineCapabilityDAO.GetByID")
	if MachineCapabilityDAOSpan != nil {
		defer MachineCapabilityDAOSpan.End()

		mcd.tracerSpan.SetAttribute(MachineCapabilityDAOSpan, "id", id.String())
	}

	m := &MachineCapability{}

	query := db.GetIDB(tx, mcd.dbSession).NewSelect().Model(m).Where("mc.id = ?", id)

	for _, relation := range includeRelations {
		query = query.Relation(relation)
	}

	err := query.Scan(ctx)
	if err != nil {
		if err == sql.ErrNoRows {
			return nil, db.ErrDoesNotExist
		}
		return nil, err
	}

	return m, nil
}

// GetAll returns all MachineCapabilities filtered by the given parameters
// if orderBy is nil, then records are ordered by column specified in MachineCapabilityOrderByDefault in ascending order
func (mcd MachineCapabilitySQLDAO) GetAll(
	ctx context.Context, tx *db.Tx,
	machineIDs []string,
	instanceTypeIDs []uuid.UUID,
	capabilityType *string,
	name *string,
	frequency *string,
	capacity *string,
	vendor *string,
	count *int,
	deviceType *string,
	inactiveDevices []int,
	includeRelations []string,
	offset *int, limit *int, orderBy *paginator.OrderBy) ([]MachineCapability, int, error) {
	// Create a child span and set the attributes for current request
	ctx, MachineCapabilityDAOSpan := mcd.tracerSpan.CreateChildInCurrentContext(ctx, "MachineCapabilityDAO.GetAll")
	if MachineCapabilityDAOSpan != nil {
		defer MachineCapabilityDAOSpan.End()
	}

	mcs := []MachineCapability{}

	query := db.GetIDB(tx, mcd.dbSession).NewSelect().Model(&mcs)
	if machineIDs != nil {
		if len(machineIDs) == 1 {
			query = query.Where("mc.machine_id = ?", machineIDs[0])
		} else {
			query = query.Where("mc.machine_id IN (?)", bun.In(machineIDs))
		}

		if MachineCapabilityDAOSpan != nil {
			mcd.tracerSpan.SetAttribute(MachineCapabilityDAOSpan, "machine_ids", machineIDs)
		}
	}
	if instanceTypeIDs != nil {
		if len(machineIDs) == 1 {
			query = query.Where("mc.instance_type_id = ?", instanceTypeIDs[0])
		} else {
			query = query.Where("mc.instance_type_id IN (?)", bun.In(instanceTypeIDs))
		}

		if MachineCapabilityDAOSpan != nil {
			mcd.tracerSpan.SetAttribute(MachineCapabilityDAOSpan, "instance_type_ids", instanceTypeIDs)
		}
	}
	if capabilityType != nil {
		query = query.Where("mc.type = ?", *capabilityType)

		if MachineCapabilityDAOSpan != nil {
			mcd.tracerSpan.SetAttribute(MachineCapabilityDAOSpan, "type", *capabilityType)
		}
	}
	if name != nil {
		query = query.Where("mc.name = ?", *name)

		if MachineCapabilityDAOSpan != nil {
			mcd.tracerSpan.SetAttribute(MachineCapabilityDAOSpan, "name", *name)
		}
	}
	if frequency != nil {
		query = query.Where("mc.frequency = ?", *frequency)

		if MachineCapabilityDAOSpan != nil {
			mcd.tracerSpan.SetAttribute(MachineCapabilityDAOSpan, "frequency", *frequency)
		}
	}
	if capacity != nil {
		query = query.Where("mc.capacity = ?", *capacity)

		if MachineCapabilityDAOSpan != nil {
			mcd.tracerSpan.SetAttribute(MachineCapabilityDAOSpan, "capacity", *capacity)
		}
	}
	if vendor != nil {
		query = query.Where("mc.vendor = ?", *vendor)

		if MachineCapabilityDAOSpan != nil {
			mcd.tracerSpan.SetAttribute(MachineCapabilityDAOSpan, "vendor", *vendor)
		}
	}
	if count != nil {
		query = query.Where("mc.count = ?", *count)

		if MachineCapabilityDAOSpan != nil {
			mcd.tracerSpan.SetAttribute(MachineCapabilityDAOSpan, "count", *count)
		}
	}
	if deviceType != nil {
		query = query.Where("mc.device_type = ?", *deviceType)

		if MachineCapabilityDAOSpan != nil {
			mcd.tracerSpan.SetAttribute(MachineCapabilityDAOSpan, "device_type", *deviceType)
		}
	}
	if inactiveDevices != nil {
		query = query.Where("mc.inactive_devices = ?", inactiveDevices)

		if MachineCapabilityDAOSpan != nil {
			mcd.tracerSpan.SetAttribute(MachineCapabilityDAOSpan, "inactive_devices", inactiveDevices)
		}
	}

	for _, relation := range includeRelations {
		query = query.Relation(relation)
	}

	// if no order is passed, set default to make sure objects return always in the same order and pagination works properly
	if orderBy == nil {
		orderBy = paginator.NewDefaultOrderBy(MachineCapabilityOrderByDefault)
	}

	paginator, err := paginator.NewPaginator(ctx, query, offset, limit, orderBy, MachineCapabilityOrderByFields)
	if err != nil {
		return nil, 0, err
	}

	err = paginator.Query.Limit(paginator.Limit).Offset(paginator.Offset).Scan(ctx)
	if err != nil {
		return nil, 0, err
	}

	return mcs, paginator.Total, nil
}

// GPUSiteStat is a per-site, per-GPU-name aggregation row produced by
// GetGPUStatsBySite. GPUs is the summed GPU count (a NULL capability count is
// treated as 1) and Machines is the number of distinct machines that report the
// GPU at the site.
type GPUSiteStat struct {
	SiteID   uuid.UUID `bun:"site_id"`
	Name     string    `bun:"name"`
	GPUs     int       `bun:"gpus"`
	Machines int       `bun:"machines"`
}

// GetGPUStatsBySite aggregates GPU capabilities in the database, grouped by site
// and GPU name, instead of loading every machine and capability row into the
// application. It mirrors the aggregation approach of MachineSQLDAO.GetCountByStatus.
func (mcd MachineCapabilitySQLDAO) GetGPUStatsBySite(ctx context.Context, tx *db.Tx, infrastructureProviderID *uuid.UUID, siteID *uuid.UUID) ([]GPUSiteStat, error) {
	// Create a child span and set the attributes for current request
	ctx, MachineCapabilityDAOSpan := mcd.tracerSpan.CreateChildInCurrentContext(ctx, "MachineCapabilityDAO.GetGPUStatsBySite")
	if MachineCapabilityDAOSpan != nil {
		defer MachineCapabilityDAOSpan.End()
	}

	stats := []GPUSiteStat{}

	query := db.GetIDB(tx, mcd.dbSession).NewSelect().Model((*MachineCapability)(nil)).
		Join("JOIN machine AS m ON m.id = mc.machine_id AND m.deleted IS NULL").
		Where("mc.deleted IS NULL").
		Where("mc.type = ?", MachineCapabilityTypeGPU).
		ColumnExpr("m.site_id AS site_id").
		ColumnExpr("mc.name AS name").
		ColumnExpr("SUM(COALESCE(mc.count, 1)) AS gpus").
		ColumnExpr("COUNT(DISTINCT mc.machine_id) AS machines").
		GroupExpr("m.site_id, mc.name")

	if infrastructureProviderID != nil {
		query = query.Where("m.infrastructure_provider_id = ?", *infrastructureProviderID)

		if MachineCapabilityDAOSpan != nil {
			mcd.tracerSpan.SetAttribute(MachineCapabilityDAOSpan, "infrastructure_provider_id", infrastructureProviderID.String())
		}
	}
	if siteID != nil {
		query = query.Where("m.site_id = ?", *siteID)

		if MachineCapabilityDAOSpan != nil {
			mcd.tracerSpan.SetAttribute(MachineCapabilityDAOSpan, "site_id", siteID.String())
		}
	}

	err := query.Scan(ctx, &stats)
	if err != nil {
		return nil, err
	}

	return stats, nil
}

// GetAllDistinct returns all MachineCapabilities that have distinct type, name, frequency, capacity, vendor, count, and device_type filtered by the given parameters
func (mcd MachineCapabilitySQLDAO) GetAllDistinct(
	ctx context.Context, tx *db.Tx,
	machineIDs []string,
	instanceTypeID *uuid.UUID,
	capabilityType *string,
	name *string,
	frequency *string,
	capacity *string,
	vendor *string,
	count *int,
	deviceType *string,
	inactiveDevices []int,
	offset *int, limit *int, orderBy *paginator.OrderBy) ([]MachineCapability, int, error) {
	// Create a child span and set the attributes for current request
	ctx, MachineCapabilityDAOSpan := mcd.tracerSpan.CreateChildInCurrentContext(ctx, "MachineCapabilityDAO.GetAllDistinct")
	if MachineCapabilityDAOSpan != nil {
		defer MachineCapabilityDAOSpan.End()
	}

	mcs := []MachineCapability{}

	query := db.GetIDB(tx, mcd.dbSession).NewSelect().Model(&mcs).ColumnExpr("DISTINCT ON (mc.type, mc.name, mc.frequency, mc.capacity, mc.vendor, mc.count, mc.device_type, mc.inactive_devices) mc.*")
	if machineIDs != nil {
		if len(machineIDs) == 1 {
			query = query.Where("mc.machine_id = ?", machineIDs[0])
		} else {
			query = query.Where("mc.machine_id IN (?)", bun.In(machineIDs))
		}

		if MachineCapabilityDAOSpan != nil {
			mcd.tracerSpan.SetAttribute(MachineCapabilityDAOSpan, "machine_ids", machineIDs)
		}
	}
	if instanceTypeID != nil {
		query = query.Where("mc.instance_type_id = ?", *instanceTypeID)

		if MachineCapabilityDAOSpan != nil {
			mcd.tracerSpan.SetAttribute(MachineCapabilityDAOSpan, "instance_type_id", instanceTypeID.String())
		}
	}
	if capabilityType != nil {
		query = query.Where("mc.type = ?", *capabilityType)

		if MachineCapabilityDAOSpan != nil {
			mcd.tracerSpan.SetAttribute(MachineCapabilityDAOSpan, "type", *capabilityType)
		}
	}
	if name != nil {
		query = query.Where("mc.name = ?", *name)

		if MachineCapabilityDAOSpan != nil {
			mcd.tracerSpan.SetAttribute(MachineCapabilityDAOSpan, "name", *name)
		}
	}
	if frequency != nil {
		query = query.Where("mc.frequency = ?", *frequency)

		if MachineCapabilityDAOSpan != nil {
			mcd.tracerSpan.SetAttribute(MachineCapabilityDAOSpan, "frequency", *frequency)
		}
	}
	if capacity != nil {
		query = query.Where("mc.capacity = ?", *capacity)

		if MachineCapabilityDAOSpan != nil {
			mcd.tracerSpan.SetAttribute(MachineCapabilityDAOSpan, "capacity", *capacity)
		}
	}
	if vendor != nil {
		query = query.Where("mc.vendor = ?", *vendor)

		if MachineCapabilityDAOSpan != nil {
			mcd.tracerSpan.SetAttribute(MachineCapabilityDAOSpan, "vendor", *vendor)
		}
	}
	if count != nil {
		query = query.Where("mc.count = ?", *count)

		if MachineCapabilityDAOSpan != nil {
			mcd.tracerSpan.SetAttribute(MachineCapabilityDAOSpan, "count", *count)
		}
	}
	if deviceType != nil {
		query = query.Where("mc.device_type = ?", *deviceType)

		if MachineCapabilityDAOSpan != nil {
			mcd.tracerSpan.SetAttribute(MachineCapabilityDAOSpan, "device_type", *deviceType)
		}
	}
	if inactiveDevices != nil {
		query = query.Where("mc.inactive_devices = ?", inactiveDevices)

		if MachineCapabilityDAOSpan != nil {
			mcd.tracerSpan.SetAttribute(MachineCapabilityDAOSpan, "inactive_devices", inactiveDevices)
		}
	}

	paginator, err := paginator.NewPaginator(ctx, query, offset, limit, orderBy, MachineCapabilityOrderByFields)
	if err != nil {
		return nil, 0, err
	}

	err = paginator.Query.Limit(paginator.Limit).Offset(paginator.Offset).Scan(ctx)
	if err != nil {
		return nil, 0, err
	}

	return mcs, paginator.Total, nil
}

// Update updates specified fields of an existing MachineCapability
// The updated fields are assumed to be set to non-null values
// since there are 2 operations (UPDATE, SELECT), in this, it is required that
// this library call happens within a transaction
func (mcd MachineCapabilitySQLDAO) Update(
	ctx context.Context, tx *db.Tx,
	input MachineCapabilityUpdateInput) (*MachineCapability, error) {
	// Create a child span and set the attributes for current request
	ctx, MachineCapabilityDAOSpan := mcd.tracerSpan.CreateChildInCurrentContext(ctx, "MachineCapabilityDAO.Update")
	if MachineCapabilityDAOSpan != nil {
		defer MachineCapabilityDAOSpan.End()
	}

	m := &MachineCapability{
		ID: input.ID,
	}

	updatedFields := []string{}

	if input.MachineID != nil {
		m.MachineID = input.MachineID
		updatedFields = append(updatedFields, "machine_id")

		if MachineCapabilityDAOSpan != nil {
			mcd.tracerSpan.SetAttribute(MachineCapabilityDAOSpan, "machine_id", input.MachineID)
		}
	}
	if input.InstanceTypeID != nil {
		m.InstanceTypeID = input.InstanceTypeID
		updatedFields = append(updatedFields, "instance_type_id")

		if MachineCapabilityDAOSpan != nil {
			mcd.tracerSpan.SetAttribute(MachineCapabilityDAOSpan, "instance_type_id", input.InstanceTypeID.String())
		}
	}
	if input.Type != nil {
		if len(strings.TrimSpace(string(*input.Type))) == 0 {
			return nil, errors.New("capabilityType is empty")
		}
		if !MachineCapabilityTypeChoiceMap[*input.Type] {
			return nil, fmt.Errorf("invalid capabilityType: %q", *input.Type)
		}
		m.Type = *input.Type
		updatedFields = append(updatedFields, "type")

		if MachineCapabilityDAOSpan != nil {
			mcd.tracerSpan.SetAttribute(MachineCapabilityDAOSpan, "type", *input.Type)
		}
	}
	if input.Name != nil {
		m.Name = *input.Name
		updatedFields = append(updatedFields, "name")

		if MachineCapabilityDAOSpan != nil {
			mcd.tracerSpan.SetAttribute(MachineCapabilityDAOSpan, "name", *input.Name)
		}
	}
	if input.Frequency != nil {
		m.Frequency = input.Frequency
		updatedFields = append(updatedFields, "frequency")

		if MachineCapabilityDAOSpan != nil {
			mcd.tracerSpan.SetAttribute(MachineCapabilityDAOSpan, "frequency", *input.Frequency)
		}
	}
	if input.Capacity != nil {
		m.Capacity = input.Capacity
		updatedFields = append(updatedFields, "capacity")

		if MachineCapabilityDAOSpan != nil {
			mcd.tracerSpan.SetAttribute(MachineCapabilityDAOSpan, "capacity", *input.Capacity)
		}
	}
	if input.Vendor != nil {
		m.Vendor = input.Vendor
		updatedFields = append(updatedFields, "vendor")

		if MachineCapabilityDAOSpan != nil {
			mcd.tracerSpan.SetAttribute(MachineCapabilityDAOSpan, "vendor", *input.Vendor)
		}
	}
	if input.Count != nil {
		m.Count = input.Count
		updatedFields = append(updatedFields, "count")

		if MachineCapabilityDAOSpan != nil {
			mcd.tracerSpan.SetAttribute(MachineCapabilityDAOSpan, "count", *input.Count)
		}
	}
	if input.DeviceType != nil {
		m.DeviceType = input.DeviceType
		updatedFields = append(updatedFields, "device_type")

		if MachineCapabilityDAOSpan != nil {
			mcd.tracerSpan.SetAttribute(MachineCapabilityDAOSpan, "device_type", *input.DeviceType)
		}
	}
	if input.Threads != nil {
		m.Threads = input.Threads
		updatedFields = append(updatedFields, "threads")

		if MachineCapabilityDAOSpan != nil {
			mcd.tracerSpan.SetAttribute(MachineCapabilityDAOSpan, "threads", *input.Threads)
		}
	}

	if input.Cores != nil {
		m.Cores = input.Cores
		updatedFields = append(updatedFields, "cores")

		if MachineCapabilityDAOSpan != nil {
			mcd.tracerSpan.SetAttribute(MachineCapabilityDAOSpan, "cores", *input.Cores)
		}
	}

	if input.HardwareRevision != nil {
		m.HardwareRevision = input.HardwareRevision
		updatedFields = append(updatedFields, "hardware_revision")

		if MachineCapabilityDAOSpan != nil {
			mcd.tracerSpan.SetAttribute(MachineCapabilityDAOSpan, "hardware_revision", *input.HardwareRevision)
		}
	}

	if input.Index != nil {
		m.Index = *input.Index
		updatedFields = append(updatedFields, "index")

		if MachineCapabilityDAOSpan != nil {
			mcd.tracerSpan.SetAttribute(MachineCapabilityDAOSpan, "index", *input.Index)
		}
	}

	if input.InactiveDevices != nil {
		m.InactiveDevices = input.InactiveDevices
		updatedFields = append(updatedFields, "inactive_devices")

		if MachineCapabilityDAOSpan != nil {
			mcd.tracerSpan.SetAttribute(MachineCapabilityDAOSpan, "inactive_devices", fmt.Sprintf("%v", input.InactiveDevices))
		}
	}

	if input.Info != nil {
		m.Info = input.Info
		updatedFields = append(updatedFields, "info")
	}

	if len(updatedFields) > 0 {
		updatedFields = append(updatedFields, "updated")

		_, err := db.GetIDB(tx, mcd.dbSession).NewUpdate().Model(m).Column(updatedFields...).Where("id = ?", input.ID).Exec(ctx)
		if err != nil {
			return nil, err
		}
	}

	nv, err := mcd.GetByID(ctx, tx, m.ID, nil)
	if err != nil {
		return nil, err
	}

	return nv, nil
}

// ClearFromParams sets parameters of an existing Machine Capability to null values in db
// since there are 2 operations (UPDATE, SELECT), it is required that
// this must be within a transaction
func (mcd MachineCapabilitySQLDAO) ClearFromParams(
	ctx context.Context, tx *db.Tx,
	id uuid.UUID,
	machineID, instanceTypeID, frequency, capacity, vendor, info bool) (*MachineCapability, error) {
	// Create a child span and set the attributes for current request
	ctx, MachineCapabilityDAOSpan := mcd.tracerSpan.CreateChildInCurrentContext(ctx, "MachineCapabilityDAO.ClearFromParams")
	if MachineCapabilityDAOSpan != nil {
		defer MachineCapabilityDAOSpan.End()

		mcd.tracerSpan.SetAttribute(MachineCapabilityDAOSpan, "id", id.String())
	}

	m := &MachineCapability{
		ID: id,
	}

	if machineID && instanceTypeID {
		return nil, fmt.Errorf("machineID and instanceTypeID cannot be cleared together: %w", db.ErrInvalidParams)
	}

	updatedFields := []string{}
	if machineID {
		m.MachineID = nil
		updatedFields = append(updatedFields, "machine_id")
	}
	if instanceTypeID {
		m.InstanceTypeID = nil
		updatedFields = append(updatedFields, "instance_type_id")
	}
	if frequency {
		m.Frequency = nil
		updatedFields = append(updatedFields, "frequency")
	}
	if capacity {
		m.Capacity = nil
		updatedFields = append(updatedFields, "capacity")
	}
	if vendor {
		m.Vendor = nil
		updatedFields = append(updatedFields, "vendor")
	}
	if info {
		m.Info = nil
		updatedFields = append(updatedFields, "info")
	}

	if len(updatedFields) > 0 {
		updatedFields = append(updatedFields, "updated")

		_, err := db.GetIDB(tx, mcd.dbSession).NewUpdate().Model(m).Column(updatedFields...).Where("id = ?", id).Exec(ctx)
		if err != nil {
			return nil, err
		}
	}

	nv, err := mcd.GetByID(ctx, tx, id, nil)
	if err != nil {
		return nil, err
	}
	return nv, nil
}

// DeleteByID deletes an MachineCapability by ID
// error is returned only if there is a db error
// if the object being deleted doesnt exist, error is not returned (idempotent delete)
func (mcd MachineCapabilitySQLDAO) DeleteByID(ctx context.Context, tx *db.Tx, id uuid.UUID, purge bool) error {
	// Create a child span and set the attributes for current request
	ctx, MachineCapabilityDAOSpan := mcd.tracerSpan.CreateChildInCurrentContext(ctx, "MachineCapabilityDAO.DeleteByID")
	if MachineCapabilityDAOSpan != nil {
		defer MachineCapabilityDAOSpan.End()

		mcd.tracerSpan.SetAttribute(MachineCapabilityDAOSpan, "id", id.String())
	}

	mc := &MachineCapability{
		ID: id,
	}

	var err error

	if purge {
		_, err = db.GetIDB(tx, mcd.dbSession).NewDelete().Model(mc).Where("id = ?", id).ForceDelete().Exec(ctx)
	} else {
		_, err = db.GetIDB(tx, mcd.dbSession).NewDelete().Model(mc).Where("id = ?", id).Exec(ctx)
	}
	if err != nil {
		return err
	}

	return nil
}

// NewMachineCapabilityDAO returns a new MachineCapabilityDAO
func NewMachineCapabilityDAO(dbSession *db.Session) MachineCapabilityDAO {
	return &MachineCapabilitySQLDAO{
		dbSession:  dbSession,
		tracerSpan: stracer.NewTracerSpan(),
	}
}
