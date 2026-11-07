// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

// Package probeapi holds the wire types of the site health probe /v1 API
// that the runner reads and sends. The Rust crate
// crates/site-health-probe-api is the source of the contract, and its golden
// vectors keep these types in step. jsonx applies the serde rules.
package probeapi

import "github.com/NVIDIA/infra-controller/dev/k8s/scale-test/internal/jsonx"

// ContractVersion is the probe's metric contract version this runner reads
// (metrics::CONTRACT_VERSION in the Rust crate). The probe serves it as
// metrics.contractVersion in /v1/config.
const ContractVersion uint32 = 1

// UnitType is the kind of unit a Filter selects.
type UnitType string

const (
	UnitTypeRack                UnitType = "rack"
	UnitTypeHost                UnitType = "host"
	UnitTypeDpu                 UnitType = "dpu"
	UnitTypeSwitch              UnitType = "switch"
	UnitTypePowerShelf          UnitType = "power_shelf"
	UnitTypeInstance            UnitType = "instance"
	UnitTypeExpectedRecord      UnitType = "expected_record"
	UnitTypeIdentityMismatch    UnitType = "identity_mismatch"
	UnitTypeHostFirmwareRequest UnitType = "host_firmware_request"
	UnitTypeRestTotal           UnitType = "rest_total"
)

// Stage is the lifecycle stage of a unit.
type Stage string

const (
	StageExpected   Stage = "expected"
	StageRegistered Stage = "registered"
	StageReady      Stage = "ready"
	StageError      Stage = "error"
)

// Source is the API a unit was read from.
type Source string

const (
	SourceNicoAPI Source = "nico-api"
	SourceRest    Source = "rest"
)

// SiteMatch says whether the REST site's machines are this NICo site's
// machines. Only SiteMatchMatch binds the site.
type SiteMatch string

const (
	SiteMatchMatch    SiteMatch = "match"
	SiteMatchMismatch SiteMatch = "mismatch"
	SiteMatchUnknown  SiteMatch = "unknown"
)

// RecordType is the kind of expected record.
type RecordType string

const (
	RecordTypeHost       RecordType = "host"
	RecordTypeSwitch     RecordType = "switch"
	RecordTypePowerShelf RecordType = "power_shelf"
	RecordTypeRack       RecordType = "rack"
	RecordTypeRackGroup  RecordType = "rack_group"
)

// RestResource is the REST collection whose total type=rest_total reads.
type RestResource string

const (
	RestResourceMachine  RestResource = "machine"
	RestResourceInstance RestResource = "instance"
	RestResourceRack     RestResource = "rack"
	RestResourceTray     RestResource = "tray"
)

// Initiator is who started a host firmware request.
type Initiator string

const (
	InitiatorAutomatic Initiator = "automatic"
	InitiatorManual    Initiator = "manual"
	InitiatorOther     Initiator = "other"
)

// Component is a firmware component of a host.
type Component string

const (
	ComponentBmc     Component = "bmc"
	ComponentUefi    Component = "uefi"
	ComponentBmcUefi Component = "bmc_uefi"
	ComponentOther   Component = "other"
)

// MismatchKind is why a device and its expected record do not pair up.
type MismatchKind string

const (
	MismatchKindAmbiguousLink    MismatchKind = "ambiguous_link"
	MismatchKindNoExpectedRecord MismatchKind = "no_expected_record"
)

// Direction is whether a count is waited on to rise or to fall.
type Direction string

const (
	DirectionUp   Direction = "up"
	DirectionDown Direction = "down"
)

// VersionSource is where host firmware versions were read.
type VersionSource string

const (
	VersionSourceMachineRecord  VersionSource = "machine_record"
	VersionSourceExploredReport VersionSource = "explored_report"
)

// Family is a snapshot family. Each family is read and replaced as a whole,
// and has its own sweep times.
type Family string

const (
	FamilyHost                 Family = "host"
	FamilyDpu                  Family = "dpu"
	FamilySwitch               Family = "switch"
	FamilyPowerShelf           Family = "power_shelf"
	FamilyRack                 Family = "rack"
	FamilyInstance             Family = "instance"
	FamilyExpected             Family = "expected"
	FamilyHostFirmwareRequests Family = "host_firmware_requests"
	FamilyRestInstances        Family = "rest_instances"
	FamilyRestTotals           Family = "rest_totals"
	FamilyRestSiteMatch        Family = "rest_site_match"
	FamilyRestTenantInstances  Family = "rest_tenant_instances"
	FamilyConfigNicoAPI        Family = "config_nico_api"
	FamilyConfigRest           Family = "config_rest"
)

// ErrorCode is the code of an error reply.
type ErrorCode string

const (
	ErrorCodeInvalidParam      ErrorCode = "invalid_param"
	ErrorCodeUnsupportedFilter ErrorCode = "unsupported_filter"
	ErrorCodeFieldUnavailable  ErrorCode = "field_unavailable"
	ErrorCodeNoCompleteSweep   ErrorCode = "no_complete_sweep"
	ErrorCodeSnapshotExpired   ErrorCode = "snapshot_expired"
	ErrorCodeEndInFuture       ErrorCode = "end_in_future"
	ErrorCodeBusy              ErrorCode = "busy"
	ErrorCodeStoreUnavailable  ErrorCode = "store_unavailable"
	ErrorCodeStoreTimeout      ErrorCode = "store_timeout"
	ErrorCodeNotFound          ErrorCode = "not_found"
	ErrorCodeInternal          ErrorCode = "internal"
)

func init() {
	jsonx.Enum(UnitTypeRack, UnitTypeHost, UnitTypeDpu, UnitTypeSwitch, UnitTypePowerShelf, UnitTypeInstance,
		UnitTypeExpectedRecord, UnitTypeIdentityMismatch, UnitTypeHostFirmwareRequest, UnitTypeRestTotal)
	jsonx.Enum(StageExpected, StageRegistered, StageReady, StageError)
	jsonx.Enum(SourceNicoAPI, SourceRest)
	jsonx.Enum(SiteMatchMatch, SiteMatchMismatch, SiteMatchUnknown)
	jsonx.Enum(RecordTypeHost, RecordTypeSwitch, RecordTypePowerShelf, RecordTypeRack, RecordTypeRackGroup)
	jsonx.Enum(RestResourceMachine, RestResourceInstance, RestResourceRack, RestResourceTray)
	jsonx.Enum(InitiatorAutomatic, InitiatorManual, InitiatorOther)
	jsonx.Enum(ComponentBmc, ComponentUefi, ComponentBmcUefi, ComponentOther)
	jsonx.Enum(MismatchKindAmbiguousLink, MismatchKindNoExpectedRecord)
	jsonx.Enum(DirectionUp, DirectionDown)
	jsonx.Enum(VersionSourceMachineRecord, VersionSourceExploredReport)
	jsonx.Enum(FamilyHost, FamilyDpu, FamilySwitch, FamilyPowerShelf, FamilyRack, FamilyInstance, FamilyExpected,
		FamilyHostFirmwareRequests, FamilyRestInstances, FamilyRestTotals, FamilyRestSiteMatch,
		FamilyRestTenantInstances, FamilyConfigNicoAPI, FamilyConfigRest)
	jsonx.Enum(ErrorCodeInvalidParam, ErrorCodeUnsupportedFilter, ErrorCodeFieldUnavailable, ErrorCodeNoCompleteSweep,
		ErrorCodeSnapshotExpired, ErrorCodeEndInFuture, ErrorCodeBusy, ErrorCodeStoreUnavailable,
		ErrorCodeStoreTimeout, ErrorCodeNotFound, ErrorCodeInternal)
}

// Filter selects units for /v1/count and /v1/units. It is sent as query
// parameters (Query), or flattened into the UnitsQuery body.
type Filter struct {
	Type           UnitType      `json:"type"`
	Source         *Source       `json:"source,omitempty"`
	Stage          *Stage        `json:"stage,omitempty"`
	State          *string       `json:"state,omitempty"`
	Model          *string       `json:"model,omitempty"`
	InstanceType   *string       `json:"instanceType,omitempty"`
	BmcVersion     *string       `json:"bmcVersion,omitempty"`
	UefiVersion    *string       `json:"uefiVersion,omitempty"`
	Allocatable    *bool         `json:"allocatable,omitempty"`
	UpdateComplete *bool         `json:"updateComplete,omitempty"`
	UpdateEligible *bool         `json:"updateEligible,omitempty"`
	Tenant         *string       `json:"tenant,omitempty"`
	Status         *string       `json:"status,omitempty"`
	RecordType     *RecordType   `json:"recordType,omitempty"`
	Linked         *bool         `json:"linked,omitempty"`
	Resource       *RestResource `json:"resource,omitempty"`
	Initiator      *Initiator    `json:"initiator,omitempty"`
	Kind           *MismatchKind `json:"kind,omitempty"`
	// Done false selects units that are not done. /v1/units only.
	Done *bool `json:"done,omitempty"`
}

// JSONClosed makes Decode refuse unknown fields. UnitsQuery gets it through
// its embedded Filter, as serde's deny_unknown_fields covers the flattened
// body.
func (Filter) JSONClosed() {}

// SweepInfo is the snapshot a count or units reply was read from.
type SweepInfo struct {
	SnapshotID string     `json:"snapshotId"`
	Family     Family     `json:"family"`
	SweepStart jsonx.Time `json:"sweepStart"`
	// ReadAt is the sweep end. Every unit in the snapshot was read by then.
	ReadAt               jsonx.Time `json:"readAt"`
	SweepAgeSeconds      float64    `json:"sweepAgeSeconds"`
	SweepIntervalSeconds float64    `json:"sweepIntervalSeconds"`
}

// CountResponse is the GET /v1/count reply.
type CountResponse struct {
	Count uint64 `json:"count"`
	SweepInfo
}

// UnitsQuery is the POST /v1/units/query body.
type UnitsQuery struct {
	Filter
	// Name is a regex on the instance name. type=instance only.
	Name *string `json:"name,omitempty"`
	// IDs are the unit IDs to read. Empty means all units.
	IDs    []string `json:"ids,omitempty"`
	Limit  *uint32  `json:"limit,omitempty"`
	Cursor *string  `json:"cursor,omitempty"`
}

// UnitsResponse is the GET /v1/units and POST /v1/units/query reply. Units
// sort by (source, type, id).
type UnitsResponse struct {
	SweepInfo
	Units []Unit `json:"units"`
	// MissingIDs are asked IDs that do not exist in the snapshot, repeated
	// on every page.
	MissingIDs []string `json:"missingIds"`
	NextCursor *string  `json:"nextCursor"`
}

type Unit struct {
	ID       string          `json:"id"`
	Type     UnitType        `json:"type"`
	Source   Source          `json:"source"`
	Stage    Stage           `json:"stage"`
	Done     bool            `json:"done"`
	State    string          `json:"state"`
	Reason   *string         `json:"reason"`
	BmcIP    *string         `json:"bmcIp"`
	Host     *HostFields     `json:"host"`
	Dpu      *DpuFields      `json:"dpu"`
	Instance *InstanceFields `json:"instance"`
	Expected *ExpectedFields `json:"expected"`
}

type HostFields struct {
	Vendor        *string       `json:"vendor"`
	Model         *string       `json:"model"`
	InstanceType  *string       `json:"instanceType"`
	BmcVersion    *string       `json:"bmcVersion"`
	UefiVersion   *string       `json:"uefiVersion"`
	VersionSource VersionSource `json:"versionSource"`
	ReportAt      *jsonx.Time   `json:"reportAt"`
	ReportError   *string       `json:"reportError"`
	// Allocatable is nil until the probe can read allocatability.
	Allocatable        *bool   `json:"allocatable"`
	BlockingReason     *string `json:"blockingReason"`
	FirmwareAutoupdate *bool   `json:"firmwareAutoupdate"`
	// UpdateEligible is nil until the probe can read update eligibility.
	UpdateEligible       *bool            `json:"updateEligible"`
	UpdateBlockingReason *string          `json:"updateBlockingReason"`
	UpdateComplete       bool             `json:"updateComplete"`
	OpenFirmwareRequest  *FirmwareRequest `json:"openFirmwareRequest"`
	NvlinkDomain         *string          `json:"nvlinkDomain"`
	RackID               *string          `json:"rackId"`
	DpuIDs               []string         `json:"dpuIds"`
}

type FirmwareRequest struct {
	Initiator   Initiator   `json:"initiator"`
	RequestedAt *jsonx.Time `json:"requestedAt"`
	InitiatedAt *jsonx.Time `json:"initiatedAt"`
}

type DpuFields struct {
	// HostID is nil while no host lists this DPU.
	HostID *string `json:"hostId"`
}

type InstanceFields struct {
	Name   *string `json:"name"`
	Tenant *string `json:"tenant"`
	HostID *string `json:"hostId"`
	Status string  `json:"status"`
}

type ExpectedFields struct {
	RecordType      RecordType `json:"recordType"`
	Linked          bool       `json:"linked"`
	DeviceID        *string    `json:"deviceId"`
	BmcMac          *string    `json:"bmcMac"`
	ExploredAddress *string    `json:"exploredAddress"`
}

// ReadInfo is the times of one config read.
type ReadInfo struct {
	ReadStart           jsonx.Time `json:"readStart"`
	ReadAt              jsonx.Time `json:"readAt"`
	ReadAgeSeconds      float64    `json:"readAgeSeconds"`
	ReadIntervalSeconds float64    `json:"readIntervalSeconds"`
	// LastAttemptError is the error of the newest attempt when it failed.
	// The values are then from the last good read, at ReadAt.
	LastAttemptError *string `json:"lastAttemptError"`
}

// ConfigResponse is the GET /v1/config reply. A source is nil before its
// first good read and while its reader is off.
type ConfigResponse struct {
	NicoAPI  *ConfigNicoAPI   `json:"nicoApi"`
	Rest     *ConfigRest      `json:"rest"`
	RestSite *RestSiteBinding `json:"restSite"`
	// Metrics is where the probe's series are in the metrics store. Nil
	// when the deployment declares no store that scrapes the probe.
	Metrics *MetricsInfo `json:"metrics,omitempty"`
}

// MetricsInfo is how a client finds this probe's series in the metrics
// store that scrapes it.
type MetricsInfo struct {
	// ContractVersion is the metric contract the probe publishes with.
	// Compare it with ContractVersion.
	ContractVersion uint32 `json:"contractVersion"`
	// Selector holds the label equality matchers that select this probe's
	// series.
	Selector map[string]string `json:"selector"`
	// ScrapeIntervalSeconds is how often the store scrapes the probe.
	ScrapeIntervalSeconds float64 `json:"scrapeIntervalSeconds"`
	// ReaderIntervalsSeconds is the reader interval of each family an
	// enabled reader sweeps. A family missing here is not read.
	ReaderIntervalsSeconds map[Family]float64 `json:"readerIntervalsSeconds"`
	// ProcessStartSeconds is the probe's process_start_timestamp_seconds.
	ProcessStartSeconds float64 `json:"processStartSeconds"`
}

// RestSiteBinding is the REST site checked against the NICo device census.
type RestSiteBinding struct {
	ReadInfo
	Result SiteMatch `json:"result"`
}

type ConfigNicoAPI struct {
	ReadInfo
	FirmwareTargets []FirmwareTarget `json:"firmwareTargets"`
	Runtime         RuntimeSettings  `json:"runtime"`
	SiteExplorer    *SiteExplorerRun `json:"siteExplorer"`
}

type FirmwareTarget struct {
	Vendor    string    `json:"vendor"`
	Model     string    `json:"model"`
	Component Component `json:"component"`
	// ComponentName is nico-api's name of the component, as host firmware
	// inventory names it, for example bmc or cx7.
	ComponentName string `json:"componentName"`
	Version       string `json:"version"`
	ExplicitStart bool   `json:"explicitStart"`
}

type RuntimeSettings struct {
	MaxConcurrentMachineUpdatesAbsolute *int64  `json:"maxConcurrentMachineUpdatesAbsolute"`
	MaxConcurrentMachineUpdatesPercent  *int64  `json:"maxConcurrentMachineUpdatesPercent"`
	MachineUpdateRunIntervalSeconds     *uint64 `json:"machineUpdateRunIntervalSeconds"`
	RequiresManualUpgrade               *bool   `json:"requiresManualUpgrade"`
	AutoHostFirmwareUpdate              *bool   `json:"autoHostFirmwareUpdate"`
}

type SiteExplorerRun struct {
	StartedAt                *jsonx.Time `json:"startedAt"`
	FinishedAt               *jsonx.Time `json:"finishedAt"`
	LastSuccessfulFinishedAt *jsonx.Time `json:"lastSuccessfulFinishedAt"`
	Success                  bool        `json:"success"`
	ExplorationsAttempted    int64       `json:"explorationsAttempted"`
	ExplorationsSucceeded    int64       `json:"explorationsSucceeded"`
	ExplorationsFailed       int64       `json:"explorationsFailed"`
}

type ConfigRest struct {
	ReadInfo
	TenantAllocation *TenantAllocation `json:"tenantAllocation"`
}

type TenantAllocation struct {
	Tenant       string `json:"tenant"`
	InstanceType string `json:"instanceType"`
	Assigned     int64  `json:"assigned"`
	Total        int64  `json:"total"`
	Used         int64  `json:"used"`
	Unused       int64  `json:"unused"`
	UnusedUsable int64  `json:"unusedUsable"`
}

// ErrorBody is the body of every non-2xx reply.
type ErrorBody struct {
	Error ErrorDetail `json:"error"`
}

type ErrorDetail struct {
	Code    ErrorCode `json:"code"`
	Message string    `json:"message"`
	// Field is the request parameter at fault, when there is one.
	Field *string `json:"field"`
}

// encoding/json reads these through jsonx.Decode. Write them with jsonx.Encode.

func (f *Filter) UnmarshalJSON(data []byte) error         { return jsonx.Decode(data, f) }
func (q *UnitsQuery) UnmarshalJSON(data []byte) error     { return jsonx.Decode(data, q) }
func (r *CountResponse) UnmarshalJSON(data []byte) error  { return jsonx.Decode(data, r) }
func (r *UnitsResponse) UnmarshalJSON(data []byte) error  { return jsonx.Decode(data, r) }
func (r *ConfigResponse) UnmarshalJSON(data []byte) error { return jsonx.Decode(data, r) }
func (b *ErrorBody) UnmarshalJSON(data []byte) error      { return jsonx.Decode(data, b) }
