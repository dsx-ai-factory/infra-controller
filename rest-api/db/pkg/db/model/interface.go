// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package model

import (
	"context"
	"database/sql"
	"fmt"
	"net/netip"
	"net/url"
	"strconv"
	"time"

	"github.com/NVIDIA/infra-controller/rest-api/db/pkg/db"
	"github.com/NVIDIA/infra-controller/rest-api/db/pkg/db/paginator"
	stracer "github.com/NVIDIA/infra-controller/rest-api/db/pkg/tracer"
	corev1 "github.com/NVIDIA/infra-controller/rest-api/proto/core/gen/v1"
	"github.com/google/uuid"
	"github.com/uptrace/bun"
)

const (
	// InterfaceStatusPending status is pending
	InterfaceStatusPending = "Pending"
	// InterfaceStatusProvisioning status is provisioning
	InterfaceStatusProvisioning = "Provisioning"
	// InterfaceStatusReady status is ready
	InterfaceStatusReady = "Ready"
	// InterfaceStatusError status is error
	InterfaceStatusError = "Error"
	// InterfaceStatusDeleting status is deleting
	InterfaceStatusDeleting = "Deleting"
	// InterfaceRelationName is the relation name for the Interface model
	InterfaceRelationName = "Interface"

	// InterfaceOrderByStatus field to be used for ordering by status
	InterfaceOrderByStatus = "status"
	// InterfaceOrderByCreated field to be used for ordering by created
	InterfaceOrderByCreated = "created"
	// InterfaceOrderByUpdated field to be used for ordering by updated
	InterfaceOrderByUpdated = "updated"

	// InterfaceOrderByDefault default field to be used for ordering when none specified
	InterfaceOrderByDefault = InterfaceOrderByCreated
)

// InterfaceVpcIPFamilyMode is the requested IP family mode for Core-managed VPC prefix selection.
type InterfaceVpcIPFamilyMode string

const (
	// InterfaceVpcIPFamilyModeIPv4Only requests an IPv4 VPC prefix.
	InterfaceVpcIPFamilyModeIPv4Only InterfaceVpcIPFamilyMode = "IPv4Only"
	// InterfaceVpcIPFamilyModeIPv6Only requests an IPv6 VPC prefix.
	InterfaceVpcIPFamilyModeIPv6Only InterfaceVpcIPFamilyMode = "IPv6Only"
	// InterfaceVpcIPFamilyModeDualStack requests both IPv4 and IPv6 VPC prefixes.
	InterfaceVpcIPFamilyModeDualStack InterfaceVpcIPFamilyMode = "DualStack"
)

var (
	// InterfaceOrderByFields is a list of valid order by fields for the Interface model
	InterfaceOrderByFields = []string{"status", "created", "updated"}
	// InterfaceRelatedEntities is a list of valid relation by fields for the Interface model
	InterfaceRelatedEntities = map[string]bool{InstanceRelationName: true, SubnetRelationName: true, VpcPrefixRelationName: true}
	// InterfaceStatusMap is a list of valid status for the Interface model
	InterfaceStatusMap = map[string]bool{
		InterfaceStatusPending:      true,
		InterfaceStatusReady:        true,
		InterfaceStatusError:        true,
		InterfaceStatusDeleting:     true,
		InterfaceStatusProvisioning: true,
	}
)

// InterfaceInlineRoutingProfile is the DB representation of interface-local routing options.
type InterfaceInlineRoutingProfile struct {
	AllowedAnycastPrefixes []string `json:"allowedAnycastPrefixes"`
}

// ToProto converts this interface routing profile into its workflow proto representation.
func (irp *InterfaceInlineRoutingProfile) ToProto() *corev1.InstanceInterfaceRoutingProfile {
	if irp == nil {
		return nil
	}
	profile := &corev1.InstanceInterfaceRoutingProfile{
		AllowedAnycastPrefixes: make([]*corev1.PrefixFilterPolicyEntry, 0, len(irp.AllowedAnycastPrefixes)),
	}
	for _, prefix := range irp.AllowedAnycastPrefixes {
		profile.AllowedAnycastPrefixes = append(profile.AllowedAnycastPrefixes, &corev1.PrefixFilterPolicyEntry{Prefix: prefix})
	}
	return profile
}

// FromProto populates this routing profile from its workflow proto representation.
func (irp *InterfaceInlineRoutingProfile) FromProto(proto *corev1.InstanceInterfaceRoutingProfile) {
	if proto == nil {
		*irp = InterfaceInlineRoutingProfile{}
		return
	}
	irp.AllowedAnycastPrefixes = make([]string, 0, len(proto.GetAllowedAnycastPrefixes()))
	for _, entry := range proto.GetAllowedAnycastPrefixes() {
		irp.AllowedAnycastPrefixes = append(irp.AllowedAnycastPrefixes, entry.GetPrefix())
	}
}

// Interface table maintains an Instance network association and its resolved state.
type Interface struct {
	bun.BaseModel `bun:"table:interface,alias:ifc"`

	ID                   uuid.UUID                      `bun:"type:uuid,pk"`
	InstanceID           uuid.UUID                      `bun:"instance_id,type:uuid,notnull"`
	Instance             *Instance                      `bun:"rel:belongs-to,join:instance_id=id"`
	SubnetID             *uuid.UUID                     `bun:"subnet_id,type:uuid"`
	Subnet               *Subnet                        `bun:"rel:belongs-to,join:subnet_id=id"`
	VpcID                *uuid.UUID                     `bun:"vpc_id,type:uuid"`
	Vpc                  *Vpc                           `bun:"rel:belongs-to,join:vpc_id=id"`
	VpcIPFamilyMode      *InterfaceVpcIPFamilyMode      `bun:"vpc_ip_family_mode"`
	VpcPrefixID          *uuid.UUID                     `bun:"vpc_prefix_id,type:uuid"`
	VpcPrefix            *VpcPrefix                     `bun:"rel:belongs-to,join:vpc_prefix_id=id"`
	SecondaryVpcPrefixID *uuid.UUID                     `bun:"secondary_vpc_prefix_id,type:uuid"`
	MachineInterfaceID   *uuid.UUID                     `bun:"machine_interface_id,type:uuid"`
	MachineInterface     *MachineInterface              `bun:"rel:belongs-to,join:machine_interface_id=id"`
	Device               *string                        `bun:"device"`
	DeviceInstance       *int                           `bun:"device_instance"`
	IsPhysical           bool                           `bun:"is_physical,notnull"`
	VirtualFunctionID    *int                           `bun:"virtual_function_id"`
	RequestedIpAddress   *string                        `bun:"requested_ip_address"`
	MacAddress           *string                        `bun:"mac_address"`
	IPAddresses          []string                       `bun:"ip_addresses,type:text[]"`
	InlineRoutingProfile *InterfaceInlineRoutingProfile `bun:"inline_routing_profile,type:jsonb"`
	Status               string                         `bun:"status,notnull"`
	Created              time.Time                      `bun:"created,nullzero,notnull,default:current_timestamp"`
	Updated              time.Time                      `bun:"updated,nullzero,notnull,default:current_timestamp"`
	Deleted              *time.Time                     `bun:"deleted,soft_delete"`
	CreatedBy            uuid.UUID                      `bun:"type:uuid,notnull"`
}

// EthernetInterfaceKey returns a stable string key for the Interface fields controlled by an update request.
func (ifc Interface) EthernetInterfaceKey() string {
	values := url.Values{}
	if ifc.SubnetID != nil {
		values.Set("subnet_id", ifc.SubnetID.String())
	}

	if ifc.VpcID != nil {
		values.Set("vpc_id", ifc.VpcID.String())
		if ifc.VpcIPFamilyMode != nil {
			values.Set("vpc_ip_family_mode", string(*ifc.VpcIPFamilyMode))
		}
	} else if ifc.VpcPrefixID != nil {
		// A Core-selected VPC interface may also have a resolved prefix. Its
		// desired identity remains the VPC selector, not that resolved result.
		values.Set("vpc_prefix_id", ifc.VpcPrefixID.String())
	}

	values.Set("is_physical", strconv.FormatBool(ifc.IsPhysical))
	if ifc.VirtualFunctionID != nil {
		values.Set("virtual_function_id", strconv.Itoa(*ifc.VirtualFunctionID))
	}
	if ifc.Device != nil {
		values.Set("device", *ifc.Device)
	}
	if ifc.DeviceInstance != nil {
		values.Set("device_instance", strconv.Itoa(*ifc.DeviceInstance))
	}
	if ifc.RequestedIpAddress != nil {
		values.Set("requested_ip_address", *ifc.RequestedIpAddress)
	}
	if ifc.InlineRoutingProfile != nil {
		values.Set("has_inline_routing_profile", "true")
		values["inline_routing_prefix"] = append([]string(nil), ifc.InlineRoutingProfile.AllowedAnycastPrefixes...)
	}

	return values.Encode()
}

// InterfaceCreateInput input parameters for Create method
type InterfaceCreateInput struct {
	InstanceID           uuid.UUID
	SubnetID             *uuid.UUID
	VpcID                *uuid.UUID
	VpcIPFamilyMode      *InterfaceVpcIPFamilyMode
	VpcPrefixID          *uuid.UUID
	IsPhysical           bool
	Device               *string
	DeviceInstance       *int
	VirtualFunctionID    *int
	RequestedIpAddress   *string
	InlineRoutingProfile *InterfaceInlineRoutingProfile
	Status               string
	CreatedBy            uuid.UUID
}

// InterfaceUpdateInput input parameters for Update method
type InterfaceUpdateInput struct {
	InterfaceID          uuid.UUID
	InstanceID           *uuid.UUID
	SubnetID             *uuid.UUID
	VpcID                *uuid.UUID
	VpcIPFamilyMode      *InterfaceVpcIPFamilyMode
	VpcPrefixID          *uuid.UUID
	SecondaryVpcPrefixID *uuid.UUID
	Device               *string
	DeviceInstance       *int
	VirtualFunctionID    *int
	RequestedIpAddress   *string
	InlineRoutingProfile *InterfaceInlineRoutingProfile
	MacAddress           *string
	IpAddresses          []string
	Status               *string
}

// InterfaceFilterInput input parameters for Filter method
type InterfaceFilterInput struct {
	InstanceIDs    []uuid.UUID
	SubnetID       *uuid.UUID
	VpcPrefixID    *uuid.UUID
	Device         *string
	DeviceInstance *int
	IsPhysical     *bool
	Statuses       []string
	IPAddresses    []string
}

// InterfaceClearInput input parameters for Clear method
type InterfaceClearInput struct {
	InterfaceID          uuid.UUID
	VpcPrefixID          bool
	SecondaryVpcPrefixID bool
	RequestedIpAddress   bool
	InlineRoutingProfile bool
}

var _ bun.BeforeAppendModelHook = (*Interface)(nil)

// BeforeAppendModel is a hook that is called before the model is appended to the query
func (ifc *Interface) BeforeAppendModel(ctx context.Context, query bun.Query) error {
	switch query.(type) {
	case *bun.InsertQuery:
		ifc.Created = db.GetCurTime()
		ifc.Updated = db.GetCurTime()
	case *bun.UpdateQuery:
		ifc.Updated = db.GetCurTime()
	}
	return nil
}

var _ bun.BeforeCreateTableHook = (*Interface)(nil)

// BeforeCreateTable is a hook that is called before the table is created
func (it *Interface) BeforeCreateTable(ctx context.Context, query *bun.CreateTableQuery) error {
	query.ForeignKey(`("instance_id") REFERENCES "instance" ("id")`).
		ForeignKey(`("subnet_id") REFERENCES "subnet" ("id")`).
		ForeignKey(`("vpc_id") REFERENCES "vpc" ("id")`).
		ForeignKey(`("machine_interface_id") REFERENCES "machine_interface" ("id")`)
	return nil
}

// InterfaceDAO is an interface for interacting with the Interface model
type InterfaceDAO interface {
	//
	Create(ctx context.Context, tx *db.Tx, input InterfaceCreateInput) (*Interface, error)
	//
	CreateMultiple(ctx context.Context, tx *db.Tx, inputs []InterfaceCreateInput) ([]Interface, error)
	//
	GetByID(ctx context.Context, tx *db.Tx, id uuid.UUID, includeRelations []string) (*Interface, error)
	//
	GetAll(ctx context.Context, tx *db.Tx, filter InterfaceFilterInput, page paginator.PageInput, includeRelations []string) ([]Interface, int, error)
	//
	Update(ctx context.Context, tx *db.Tx, input InterfaceUpdateInput) (*Interface, error)
	//
	Clear(ctx context.Context, tx *db.Tx, input InterfaceClearInput) (*Interface, error)
	//
	Delete(ctx context.Context, tx *db.Tx, id uuid.UUID) error
	//
	DeleteAllByInstanceIDs(ctx context.Context, tx *db.Tx, instanceIDs []uuid.UUID) error
}

// InterfaceSQLDAO is an implementation of the InterfaceDAO interface
type InterfaceSQLDAO struct {
	dbSession *db.Session
	InterfaceDAO
	tracerSpan *stracer.TracerSpan
}

// Create creates a new Interface from the given parameters
func (ifcd InterfaceSQLDAO) Create(ctx context.Context, tx *db.Tx, input InterfaceCreateInput) (*Interface, error) {
	// Create a child span and set the attributes for current request
	ctx, interfaceDAOSpan := ifcd.tracerSpan.CreateChildInCurrentContext(ctx, "InterfaceDAO.Create")
	if interfaceDAOSpan != nil {
		defer interfaceDAOSpan.End()
	}

	results, err := ifcd.CreateMultiple(ctx, tx, []InterfaceCreateInput{input})
	if err != nil {
		return nil, err
	}
	return &results[0], nil
}

// GetByID returns a Interface by ID
// returns db.ErrDoesNotExist error if the record is not found
func (ifcd InterfaceSQLDAO) GetByID(ctx context.Context, tx *db.Tx, id uuid.UUID, includeRelations []string) (*Interface, error) {
	// Create a child span and set the attributes for current request
	ctx, interfaceDAOSpan := ifcd.tracerSpan.CreateChildInCurrentContext(ctx, "InterfaceDAO.GetByID")
	if interfaceDAOSpan != nil {
		defer interfaceDAOSpan.End()

		ifcd.tracerSpan.SetAttribute(interfaceDAOSpan, "id", id.String())
	}

	is := &Interface{}

	query := db.GetIDB(tx, ifcd.dbSession).NewSelect().Model(is).Where("ifc.id = ?", id)

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

	return is, nil
}

func (ifcd InterfaceSQLDAO) setQueryWithFilter(filter InterfaceFilterInput, query *bun.SelectQuery, interfaceDAOSpan *stracer.CurrentContextSpan) (*bun.SelectQuery, error) {
	if filter.InstanceIDs != nil {
		if len(filter.InstanceIDs) == 1 {
			query = query.Where("ifc.instance_id = ?", filter.InstanceIDs[0])
		} else {
			query = query.Where("ifc.instance_id IN (?)", bun.In(filter.InstanceIDs))
		}

		if interfaceDAOSpan != nil {
			ifcd.tracerSpan.SetAttribute(interfaceDAOSpan, "instance_ids", filter.InstanceIDs)
		}
	}

	if filter.SubnetID != nil {
		query = query.Where("ifc.subnet_id = ?", *filter.SubnetID)

		if interfaceDAOSpan != nil {
			ifcd.tracerSpan.SetAttribute(interfaceDAOSpan, "subnet_id", filter.SubnetID.String())
		}
	}

	if filter.VpcPrefixID != nil {
		query = query.Where(
			"(ifc.vpc_prefix_id = ? OR ifc.secondary_vpc_prefix_id = ?)",
			*filter.VpcPrefixID,
			*filter.VpcPrefixID,
		)

		if interfaceDAOSpan != nil {
			ifcd.tracerSpan.SetAttribute(interfaceDAOSpan, "vpc_prefix_id", filter.VpcPrefixID.String())
		}
	}

	if filter.IsPhysical != nil {
		query = query.Where("ifc.is_physical = ?", *filter.IsPhysical)

		if interfaceDAOSpan != nil {
			ifcd.tracerSpan.SetAttribute(interfaceDAOSpan, "is_physical", *filter.IsPhysical)
		}
	}

	if filter.Device != nil {
		query = query.Where("ifc.device = ?", *filter.Device)

		if interfaceDAOSpan != nil {
			ifcd.tracerSpan.SetAttribute(interfaceDAOSpan, "device", *filter.Device)
		}
	}

	if filter.DeviceInstance != nil {
		query = query.Where("ifc.device_instance = ?", *filter.DeviceInstance)
		if interfaceDAOSpan != nil {
			ifcd.tracerSpan.SetAttribute(interfaceDAOSpan, "device_instance", *filter.DeviceInstance)
		}
	}

	if filter.Statuses != nil {
		if len(filter.Statuses) == 1 {
			query = query.Where("ifc.status = ?", filter.Statuses[0])
		} else {
			query = query.Where("ifc.status IN (?)", bun.In(filter.Statuses))
		}

		if interfaceDAOSpan != nil {
			ifcd.tracerSpan.SetAttribute(interfaceDAOSpan, "statuses", filter.Statuses)
		}
	}

	if filter.IPAddresses != nil {
		// Use array overlap operator to find interfaces with any matching IP address
		query = query.Where("ifc.ip_addresses && ARRAY[?]::text[]", bun.In(filter.IPAddresses))

		if interfaceDAOSpan != nil {
			ifcd.tracerSpan.SetAttribute(interfaceDAOSpan, "ip_addresses", filter.IPAddresses)
		}
	}

	return query, nil
}

// GetAll returns all Interfaces
// errors are returned only when there is a db related error
// if records not found, then error is nil, but length of returned slice is 0
// if orderBy is nil, then records are ordered by column specified in InterfaceOrderByDefault in ascending order
func (ifcd InterfaceSQLDAO) GetAll(ctx context.Context, tx *db.Tx, filter InterfaceFilterInput, page paginator.PageInput, includeRelations []string) ([]Interface, int, error) {
	// Create a child span and set the attributes for current request
	ctx, interfaceDAOSpan := ifcd.tracerSpan.CreateChildInCurrentContext(ctx, "InterfaceDAO.GetAll")
	if interfaceDAOSpan != nil {
		defer interfaceDAOSpan.End()
	}

	iss := []Interface{}

	query := db.GetIDB(tx, ifcd.dbSession).NewSelect().Model(&iss)

	query, err := ifcd.setQueryWithFilter(filter, query, interfaceDAOSpan)
	if err != nil {
		return iss, 0, err
	}

	for _, relation := range includeRelations {
		query = query.Relation(relation)
	}

	// if no order is passed, set default to make sure objects return always in the same order and pagination works properly
	if page.OrderBy == nil {
		page.OrderBy = paginator.NewDefaultOrderBy(VpcOrderByDefault)
	}

	paginator, err := paginator.NewPaginator(ctx, query, page.Offset, page.Limit, page.OrderBy, InterfaceOrderByFields)
	if err != nil {
		return nil, 0, err
	}

	err = paginator.Query.Limit(paginator.Limit).Offset(paginator.Offset).Scan(ctx)
	if err != nil {
		return nil, 0, err
	}

	return iss, paginator.Total, nil
}

// Update updates specified fields of an existing Interface
// The updated fields are assumed to be set to non-null values
func (ifcd InterfaceSQLDAO) Update(ctx context.Context, tx *db.Tx, input InterfaceUpdateInput) (*Interface, error) {
	// Create a child span and set the attributes for current request
	ctx, interfaceDAOSpan := ifcd.tracerSpan.CreateChildInCurrentContext(ctx, "InterfaceDAO.Update")
	if interfaceDAOSpan != nil {
		defer interfaceDAOSpan.End()
	}

	is := &Interface{
		ID: input.InterfaceID,
	}

	updatedFields := []string{}

	if input.InstanceID != nil {
		is.InstanceID = *input.InstanceID
		updatedFields = append(updatedFields, "instance_id")

		if interfaceDAOSpan != nil {
			ifcd.tracerSpan.SetAttribute(interfaceDAOSpan, "instance_id", input.InstanceID.String())
		}
	}
	if input.SubnetID != nil {
		is.SubnetID = input.SubnetID
		updatedFields = append(updatedFields, "subnet_id")

		if interfaceDAOSpan != nil {
			ifcd.tracerSpan.SetAttribute(interfaceDAOSpan, "subnet_id", input.SubnetID.String())
		}
	}
	if input.VpcID != nil {
		is.VpcID = input.VpcID
		updatedFields = append(updatedFields, "vpc_id")

		if interfaceDAOSpan != nil {
			ifcd.tracerSpan.SetAttribute(interfaceDAOSpan, "vpc_id", input.VpcID.String())
		}
	}
	if input.VpcIPFamilyMode != nil {
		is.VpcIPFamilyMode = input.VpcIPFamilyMode
		updatedFields = append(updatedFields, "vpc_ip_family_mode")

		if interfaceDAOSpan != nil {
			ifcd.tracerSpan.SetAttribute(interfaceDAOSpan, "vpc_ip_family_mode", string(*input.VpcIPFamilyMode))
		}
	}
	if input.VpcPrefixID != nil {
		is.VpcPrefixID = input.VpcPrefixID
		updatedFields = append(updatedFields, "vpc_prefix_id")

		if interfaceDAOSpan != nil {
			ifcd.tracerSpan.SetAttribute(interfaceDAOSpan, "vpc_prefix_id", input.VpcPrefixID.String())
		}
	}
	if input.SecondaryVpcPrefixID != nil {
		is.SecondaryVpcPrefixID = input.SecondaryVpcPrefixID
		updatedFields = append(updatedFields, "secondary_vpc_prefix_id")

		if interfaceDAOSpan != nil {
			ifcd.tracerSpan.SetAttribute(interfaceDAOSpan, "secondary_vpc_prefix_id", input.SecondaryVpcPrefixID.String())
		}
	}
	if input.Device != nil {
		is.Device = input.Device
		updatedFields = append(updatedFields, "device")

		if interfaceDAOSpan != nil {
			ifcd.tracerSpan.SetAttribute(interfaceDAOSpan, "device", *input.Device)
		}
	}

	if input.DeviceInstance != nil {
		is.DeviceInstance = input.DeviceInstance
		updatedFields = append(updatedFields, "device_instance")

		if interfaceDAOSpan != nil {
			ifcd.tracerSpan.SetAttribute(interfaceDAOSpan, "device_instance", *input.DeviceInstance)
		}
	}

	if input.VirtualFunctionID != nil {
		is.VirtualFunctionID = input.VirtualFunctionID
		updatedFields = append(updatedFields, "virtual_function_id")

		if interfaceDAOSpan != nil {
			ifcd.tracerSpan.SetAttribute(interfaceDAOSpan, "virtual_function_id", *input.VirtualFunctionID)
		}
	}
	if input.RequestedIpAddress != nil {
		is.RequestedIpAddress = input.RequestedIpAddress
		updatedFields = append(updatedFields, "requested_ip_address")

		if interfaceDAOSpan != nil {
			ifcd.tracerSpan.SetAttribute(interfaceDAOSpan, "requested_ip_address", *input.RequestedIpAddress)
		}
	}
	if input.InlineRoutingProfile != nil {
		is.InlineRoutingProfile = input.InlineRoutingProfile
		updatedFields = append(updatedFields, "inline_routing_profile")
	}
	if input.MacAddress != nil {
		is.MacAddress = input.MacAddress
		updatedFields = append(updatedFields, "mac_address")

		if interfaceDAOSpan != nil {
			ifcd.tracerSpan.SetAttribute(interfaceDAOSpan, "mac_address", *input.MacAddress)
		}
	}
	if input.IpAddresses != nil {
		for _, ipAddress := range input.IpAddresses {
			_, parseErr := netip.ParseAddr(ipAddress)
			if parseErr != nil {
				return nil, fmt.Errorf("invalid Interface IP address %q: %w", ipAddress, parseErr)
			}
		}

		is.IPAddresses = input.IpAddresses
		updatedFields = append(updatedFields, "ip_addresses")

		if interfaceDAOSpan != nil {
			ifcd.tracerSpan.SetAttribute(interfaceDAOSpan, "ip_addresses", input.IpAddresses)
		}
	}
	if input.Status != nil {
		is.Status = *input.Status
		updatedFields = append(updatedFields, "status")

		if interfaceDAOSpan != nil {
			ifcd.tracerSpan.SetAttribute(interfaceDAOSpan, "status", *input.Status)
		}
	}

	if len(updatedFields) > 0 {
		updatedFields = append(updatedFields, "updated")

		_, err := db.GetIDB(tx, ifcd.dbSession).NewUpdate().Model(is).Column(updatedFields...).Where("id = ?", input.InterfaceID).Exec(ctx)
		if err != nil {
			return nil, err
		}
	}

	nv, err := ifcd.GetByID(ctx, tx, is.ID, nil)

	if err != nil {
		return nil, err
	}
	return nv, nil
}

// Delete deletes an Interface by ID
// error is returned only if there is a db error
// if the object being deleted doesnt exist, error is not returned
func (ifcd InterfaceSQLDAO) Delete(ctx context.Context, tx *db.Tx, id uuid.UUID) error {
	// Create a child span and set the attributes for current request
	ctx, interfaceDAOSpan := ifcd.tracerSpan.CreateChildInCurrentContext(ctx, "InterfaceDAO.DeleteByID")
	if interfaceDAOSpan != nil {
		defer interfaceDAOSpan.End()

		ifcd.tracerSpan.SetAttribute(interfaceDAOSpan, "id", id.String())
	}

	is := &Interface{
		ID: id,
	}

	_, err := db.GetIDB(tx, ifcd.dbSession).NewDelete().Model(is).Where("id = ?", id).Exec(ctx)
	if err != nil {
		return err
	}

	return nil
}

// DeleteAllByInstanceIDs soft-deletes every Interface whose instance id is in
// the provided list.
// error is returned only if there is a db error
func (ifcd InterfaceSQLDAO) DeleteAllByInstanceIDs(ctx context.Context, tx *db.Tx, instanceIDs []uuid.UUID) error {
	ctx, interfaceDAOSpan := ifcd.tracerSpan.CreateChildInCurrentContext(ctx, "InterfaceDAO.DeleteAllByInstanceIDs")
	if interfaceDAOSpan != nil {
		defer interfaceDAOSpan.End()

		ifcd.tracerSpan.SetAttribute(interfaceDAOSpan, "instance_id_count", len(instanceIDs))
	}

	if len(instanceIDs) == 0 { // no-op
		return nil
	}

	for start := 0; start < len(instanceIDs); start += db.MaxBatchItems {
		end := start + db.MaxBatchItems
		if end > len(instanceIDs) {
			end = len(instanceIDs)
		}

		_, err := db.GetIDB(tx, ifcd.dbSession).
			NewDelete().
			Model((*Interface)(nil)).
			Where("ifc.instance_id IN (?)", bun.In(instanceIDs[start:end])).
			Exec(ctx)
		if err != nil {
			return err
		}
	}

	return nil
}

// CreateMultiple creates multiple Interfaces from the given parameters
func (ifcd InterfaceSQLDAO) CreateMultiple(ctx context.Context, tx *db.Tx, inputs []InterfaceCreateInput) ([]Interface, error) {
	if len(inputs) > db.MaxBatchItems {
		return nil, fmt.Errorf("batch size %d exceeds maximum allowed %d", len(inputs), db.MaxBatchItems)
	}

	// Create a child span and set the attributes for current request
	ctx, interfaceDAOSpan := ifcd.tracerSpan.CreateChildInCurrentContext(ctx, "InterfaceDAO.CreateMultiple")
	if interfaceDAOSpan != nil {
		defer interfaceDAOSpan.End()
		ifcd.tracerSpan.SetAttribute(interfaceDAOSpan, "batch_size", len(inputs))
	}

	if len(inputs) == 0 {
		return []Interface{}, nil
	}

	interfaces := make([]Interface, 0, len(inputs))
	ids := make([]uuid.UUID, 0, len(inputs))

	for _, input := range inputs {
		is := Interface{
			ID:                   uuid.New(),
			InstanceID:           input.InstanceID,
			SubnetID:             input.SubnetID,
			VpcID:                input.VpcID,
			VpcIPFamilyMode:      input.VpcIPFamilyMode,
			VpcPrefixID:          input.VpcPrefixID,
			Device:               input.Device,
			DeviceInstance:       input.DeviceInstance,
			VirtualFunctionID:    input.VirtualFunctionID,
			RequestedIpAddress:   input.RequestedIpAddress,
			InlineRoutingProfile: input.InlineRoutingProfile,
			IsPhysical:           input.IsPhysical,
			Status:               input.Status,
			CreatedBy:            input.CreatedBy,
		}
		interfaces = append(interfaces, is)
		ids = append(ids, is.ID)
	}

	_, err := db.GetIDB(tx, ifcd.dbSession).NewInsert().Model(&interfaces).Exec(ctx)
	if err != nil {
		return nil, err
	}

	// Fetch the created interfaces
	var result []Interface
	err = db.GetIDB(tx, ifcd.dbSession).NewSelect().Model(&result).Where("ifc.id IN (?)", bun.In(ids)).Scan(ctx)
	if err != nil {
		return nil, err
	}

	// Sort result to match input order (O(n) direct index placement)
	// This check should never fail since we just inserted these records with the exact ids
	if len(result) != len(ids) {
		return nil, fmt.Errorf("unexpected result count: got %d, expected %d", len(result), len(ids))
	}
	idToIndex := make(map[uuid.UUID]int, len(ids))
	for i, id := range ids {
		idToIndex[id] = i
	}
	sorted := make([]Interface, len(result))
	for _, item := range result {
		sorted[idToIndex[item.ID]] = item
	}

	return sorted, nil
}

// NewInterfaceDAO returns a new InterfaceDAO
func NewInterfaceDAO(dbSession *db.Session) InterfaceDAO {
	return &InterfaceSQLDAO{
		dbSession:  dbSession,
		tracerSpan: stracer.NewTracerSpan(),
	}
}

// Clear sets parameters of an existing Interface to null values in db.
// Since there are 2 operations (UPDATE, SELECT), this must be within
// a transaction.
func (ifcd InterfaceSQLDAO) Clear(ctx context.Context, tx *db.Tx, input InterfaceClearInput) (*Interface, error) {
	// Create a child span and set the attributes for current request
	ctx, interfaceDAOSpan := ifcd.tracerSpan.CreateChildInCurrentContext(ctx, "InterfaceDAO.Clear")
	if interfaceDAOSpan != nil {
		defer interfaceDAOSpan.End()
		ifcd.tracerSpan.SetAttribute(interfaceDAOSpan, "id", input.InterfaceID.String())
	}

	i := &Interface{
		ID: input.InterfaceID,
	}

	updatedFields := []string{}

	if input.VpcPrefixID {
		i.VpcPrefixID = nil
		updatedFields = append(updatedFields, "vpc_prefix_id")
	}
	if input.SecondaryVpcPrefixID {
		i.SecondaryVpcPrefixID = nil
		updatedFields = append(updatedFields, "secondary_vpc_prefix_id")
	}
	if input.RequestedIpAddress {
		i.RequestedIpAddress = nil
		updatedFields = append(updatedFields, "requested_ip_address")
	}
	if input.InlineRoutingProfile {
		i.InlineRoutingProfile = nil
		updatedFields = append(updatedFields, "inline_routing_profile")
	}

	if len(updatedFields) > 0 {
		updatedFields = append(updatedFields, "updated")

		_, err := db.GetIDB(tx, ifcd.dbSession).NewUpdate().Model(i).Column(updatedFields...).Where("id = ?", input.InterfaceID).Exec(ctx)
		if err != nil {
			return nil, err
		}
	}

	nv, err := ifcd.GetByID(ctx, tx, i.ID, nil)
	if err != nil {
		return nil, err
	}
	return nv, nil
}
