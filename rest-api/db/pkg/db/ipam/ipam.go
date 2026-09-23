// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package ipam

import (
	"context"
	"errors"
	"fmt"
	"net/netip"
	"strings"

	cipam "github.com/NVIDIA/infra-controller/rest-api/ipam"
	"github.com/uptrace/bun"

	cutil "github.com/NVIDIA/infra-controller/rest-api/common/pkg/util"
	cdb "github.com/NVIDIA/infra-controller/rest-api/db/pkg/db"
	cdbm "github.com/NVIDIA/infra-controller/rest-api/db/pkg/db/model"
)

var (
	// ErrPrefixDoesNotExistForIPBlock is the error returned when ipam does not have the entry for the IPBlock
	ErrPrefixDoesNotExistForIPBlock = errors.New("prefix does not exist for IPBlock in ipam db")
	// ErrNilIPBlock is the error when a nil IPBlock was passed
	ErrNilIPBlock = errors.New("ipblock parameter is nil")
	// ErrParentIPBlockReload is returned when allocation cannot reload and lock
	// the current parent IP Block.
	ErrParentIPBlockReload = errors.New("failed to reload parent IP Block for allocation")
)

// ~~~~~ IPAM Utilities ~~~~~ //

// NewIpamStorage will return a bun ipam storage interface
func NewIpamStorage(db *bun.DB, tx *bun.Tx) cipam.Storage {
	return cipam.NewBunStorage(db, tx)
}

// GetFirstIPFromCidr will parse a cidr, and returns the first IP address in that cidr
// this is used as the gateway IP
func GetFirstIPFromCidr(cidr string) (string, error) {
	ipPref, err := netip.ParsePrefix(cidr)
	if err != nil {
		return "", err
	}
	return ipPref.Addr().Next().String(), nil
}

// ParseCidrIntoPrefixAndBlockSize will parse a cidr into the masked prefix, and blocksize
func ParseCidrIntoPrefixAndBlockSize(cidr string) (string, int, error) {
	ipPref, err := netip.ParsePrefix(cidr)
	if err != nil {
		return "", 0, err
	}
	return ipPref.Masked().Addr().String(), int(ipPref.Bits()), nil
}

// GetIpamNamespaceForIPBlock will return the namespace string for the IPBlock
// namespace is currently: routingtype/infrastructureProviderID/siteID
func GetIpamNamespaceForIPBlock(ctx context.Context, routingType string, infrastructureProviderID, siteID string) string {
	return fmt.Sprintf("%s/%s/%s", routingType, infrastructureProviderID, siteID)
}

// GetCidrForIPBlock will return the cidr given the prefix, and block size
func GetCidrForIPBlock(ctx context.Context, prefix string, blockSize int) string {
	return fmt.Sprintf("%s/%d", prefix, blockSize)
}

// CreateIpamEntryForIPBlock will create an ipam entry in the ipam DB for the IPBlock
// will error if there is a prefix clash in that same namespace
func CreateIpamEntryForIPBlock(ctx context.Context, ipamDB cipam.Storage, prefix string, blockSize int, routingType string, infrastructureProviderID, siteID string) (*cipam.Prefix, error) {
	ipamer := cipam.NewWithStorage(ipamDB)
	namespace := GetIpamNamespaceForIPBlock(ctx, routingType, infrastructureProviderID, siteID)
	ipamer.SetNamespace(namespace)
	cidr := GetCidrForIPBlock(ctx, prefix, blockSize)
	ipamPrefix, err := ipamer.NewPrefix(ctx, cidr)
	if err != nil {
		return nil, err
	}
	return ipamPrefix, err
}

// DeleteIpamEntryForIPBlock will delete the ipam entry in the ipam DB for the IPBlock
// will not error if the deleted cidr is not existing (idempotent)
func DeleteIpamEntryForIPBlock(ctx context.Context, ipamDB cipam.Storage, prefix string, blockSize int, routingType string, infrastructureProviderID, siteID string) error {
	ipamer := cipam.NewWithStorage(ipamDB)
	namespace := GetIpamNamespaceForIPBlock(ctx, routingType, infrastructureProviderID, siteID)
	ipamer.SetNamespace(namespace)
	cidr := GetCidrForIPBlock(ctx, prefix, blockSize)
	_, err := ipamer.DeletePrefix(ctx, cidr)
	if err != nil {
		// TODO - encapsulate errors with types in the ipam package
		if strings.HasPrefix(err.Error(), cipam.ErrNotFound.Error()) {
			// if not found, dont consider it an error
			return nil
		}
		return err
	}
	return nil
}

// GetIpamUsageForIPBlock will get an ipam usage for the IPBlock
func GetIpamUsageForIPBlock(ctx context.Context, ipamDB cipam.Storage, ipBlock *cdbm.IPBlock) (*cipam.Usage, error) {

	if ipBlock == nil {
		return nil, ErrNilIPBlock
	}

	ipamer := cipam.NewWithStorage(ipamDB)
	namespace := GetIpamNamespaceForIPBlock(ctx, ipBlock.RoutingType, ipBlock.InfrastructureProviderID.String(), ipBlock.SiteID.String())
	ipamer.SetNamespace(namespace)
	cidr := GetCidrForIPBlock(ctx, ipBlock.Prefix, ipBlock.PrefixLength)
	ipamPrefix := ipamer.PrefixFrom(ctx, cidr)

	if ipamPrefix == nil {
		return nil, fmt.Errorf("did not find prefix for IPBlock: %s", ipBlock.ID)
	}

	// Handle full grant scenario
	if ipBlock.FullGrant {
		return &cipam.Usage{
			AvailableIPs:              0,
			AcquiredIPs:               0,
			AvailableSmallestPrefixes: 0,
			AvailablePrefixes:         nil,
			AcquiredPrefixes:          1,
		}, nil
	} else {
		return &cipam.Usage{
			AvailableIPs:              ipamPrefix.Usage().AvailableIPs,
			AcquiredIPs:               ipamPrefix.Usage().AcquiredIPs,
			AvailableSmallestPrefixes: ipamPrefix.Usage().AvailableSmallestPrefixes,
			AvailablePrefixes:         ipamPrefix.Usage().AvailablePrefixes,
			AcquiredPrefixes:          ipamPrefix.Usage().AcquiredPrefixes,
		}, nil
	}
}

// CreateChildIpamEntryForIPBlock will create an child ipam entry in the ipam DB for the given parent IP Block, with a given child block size
// Note: FullGrant is a special case when the childBlockSize matches the parentIPBlock, and the parentIPBlock has no
// child prefixes, then, the parentIPBlock is updated as a full grant in db, and its prefix is
// returned (without any updates to the ipam DB). Production callers must pass a transaction
// when allocating from a Core-linked Site fabric root or an unlinked Site fabric root that REST
// reconciliation may link to an OperatorManaged SitePrefix. When locking is required, this
// replaces *parentIPBlock with the current relationless database row.
func CreateChildIpamEntryForIPBlock(ctx context.Context, tx *cdb.Tx, dbSession *cdb.Session, ipamDB cipam.Storage, parentIPBlock *cdbm.IPBlock, childBlockSize int) (*cipam.Prefix, error) {
	err := LockAndValidateParentIPBlockForAllocation(ctx, tx, dbSession, parentIPBlock)
	if err != nil {
		return nil, err
	}
	// FullGrant of the parent IPBlock is also handled here to keep it localized so,
	// we can reason better wrt correctness.
	// TODO: look into implementing full grant in cloud-ipam library.
	if parentIPBlock.FullGrant {
		return nil, fmt.Errorf("parent IPBlock %s already has a full grant", parentIPBlock.ID)
	}
	ipamer := cipam.NewWithStorage(ipamDB)
	namespace := GetIpamNamespaceForIPBlock(ctx, parentIPBlock.RoutingType, parentIPBlock.InfrastructureProviderID.String(), parentIPBlock.SiteID.String())
	ipamer.SetNamespace(namespace)
	parentCidr := GetCidrForIPBlock(ctx, parentIPBlock.Prefix, parentIPBlock.PrefixLength)

	if childBlockSize == parentIPBlock.PrefixLength {
		parentPrefix := ipamer.PrefixFrom(ctx, parentCidr)
		if parentPrefix == nil {
			return nil, fmt.Errorf("did not find prefix for parentIPBlock: %s", parentIPBlock.ID)
		}
		parentUsage := parentPrefix.Usage()
		if parentUsage.AcquiredPrefixes > 0 {
			return nil, errors.New("parent IPBlock has allocated prefixes, cannot do a full-grant")
		}
		// mark the parentIPBlock as a full grant, and return the prefix corresponding to it
		ipbDAO := cdbm.NewIPBlockDAO(dbSession)
		_, err := ipbDAO.Update(
			ctx,
			tx,
			cdbm.IPBlockUpdateInput{
				IPBlockID: parentIPBlock.ID,
				FullGrant: cutil.GetPtr(true),
			},
		)
		if err != nil {
			return nil, errors.New("unable to update parent IPBlock full-grant field")
		}
		parentIPBlock.FullGrant = true
		return parentPrefix, nil
	}
	childPrefix, err := ipamer.AcquireChildPrefix(ctx, parentCidr, uint8(childBlockSize))
	if err != nil {
		return nil, err
	}
	return childPrefix, err
}

// AcquireSpecificChildIpamEntryForIPBlock will create a child ipam entry in the ipam DB for the
// given parent IP Block, using an exact child cidr instead of letting ipam choose one
// Note: FullGrant is tracked only in the REST DB, so the ipam DB reports a fully granted parent as
// empty. The caller must go through this helper (rather than the ipam library directly) so a
// fully granted parent cannot hand out an overlapping child prefix. Production callers must pass
// a transaction when allocating from a Core-linked Site fabric root or an unlinked Site fabric
// root that REST reconciliation may link to an OperatorManaged SitePrefix. When locking is
// required, this replaces *parentIPBlock with the current relationless database row.
func AcquireSpecificChildIpamEntryForIPBlock(ctx context.Context, tx *cdb.Tx, dbSession *cdb.Session, ipamDB cipam.Storage, parentIPBlock *cdbm.IPBlock, childCidr string) (*cipam.Prefix, error) {
	err := LockAndValidateParentIPBlockForAllocation(ctx, tx, dbSession, parentIPBlock)
	if err != nil {
		return nil, err
	}
	if parentIPBlock.FullGrant {
		return nil, fmt.Errorf("parent IPBlock %s already has a full grant", parentIPBlock.ID)
	}
	ipamer := cipam.NewWithStorage(ipamDB)
	namespace := GetIpamNamespaceForIPBlock(ctx, parentIPBlock.RoutingType, parentIPBlock.InfrastructureProviderID.String(), parentIPBlock.SiteID.String())
	ipamer.SetNamespace(namespace)
	parentCidr := GetCidrForIPBlock(ctx, parentIPBlock.Prefix, parentIPBlock.PrefixLength)
	// A child equal to the parent is a full grant. That path must go through
	// CreateChildIpamEntryForIPBlock so the REST DB FullGrant flag stays consistent.
	if childCidr == parentCidr {
		return nil, fmt.Errorf("child CIDR %s equals parent CIDR %s for IPBlock %s; use CreateChildIpamEntryForIPBlock", childCidr, parentCidr, parentIPBlock.ID)
	}
	childPrefix, err := ipamer.AcquireSpecificChildPrefix(ctx, parentCidr, childCidr)
	if err != nil {
		return nil, err
	}
	return childPrefix, nil
}

// LockAndValidateParentIPBlockForAllocation orders child allocation against
// OperatorManaged SitePrefix lifecycle changes. Call it before any IPAM change
// in a compound allocation operation. Any Core-linked parent, plus an unlinked
// DatacenterOnly provider root that REST reconciliation may link to an
// OperatorManaged SitePrefix, requires a transaction so the status check uses
// the locked row. The caller's IP Block is replaced with that current database
// value. Unlinked tenant IP Blocks created through Allocation and unlinked
// Public roots do not participate in this lifecycle ordering.
func LockAndValidateParentIPBlockForAllocation(ctx context.Context, tx *cdb.Tx, dbSession *cdb.Session, parentIPBlock *cdbm.IPBlock) error {
	if parentIPBlock == nil {
		return ErrNilIPBlock
	}
	requiresLifecycleLock := parentIPBlock.SitePrefixID != nil ||
		(parentIPBlock.TenantID == nil && parentIPBlock.RoutingType == cdbm.IPBlockRoutingTypeDatacenterOnly)
	if !requiresLifecycleLock {
		return nil
	}
	if tx == nil {
		return fmt.Errorf("parent IP Block %s requires a transaction for allocation", parentIPBlock.ID)
	}

	locked, err := cdbm.NewIPBlockDAO(dbSession).GetByIDForUpdate(ctx, tx, parentIPBlock.ID)
	if err != nil {
		return fmt.Errorf("%w %s: %w", ErrParentIPBlockReload, parentIPBlock.ID, err)
	}
	*parentIPBlock = *locked
	if parentIPBlock.SitePrefixID != nil && parentIPBlock.Status != cdbm.IPBlockStatusReady {
		return fmt.Errorf("parent IP Block %s linked to an OperatorManaged SitePrefix is not Ready", parentIPBlock.ID)
	}
	return nil
}

// DeleteChildIpamEntryFromCidr will delete a child ipam entry in the ipam DB
// given the parent IPBlock, and child cidr
// Note: FullGrant is a special case when the parentIPBlock has a full grant, and the child
// is being deleted, then the parent IPBlock's full grant is cleared in db
func DeleteChildIpamEntryFromCidr(ctx context.Context, tx *cdb.Tx, dbSession *cdb.Session, ipamDB cipam.Storage, parentIPBlock *cdbm.IPBlock, childCidr string) error {
	if parentIPBlock == nil {
		return ErrNilIPBlock
	}
	// if parentIPBlock is a full grant, clear the parent's full grant
	// and return
	if parentIPBlock.FullGrant {
		parentCidr := GetCidrForIPBlock(ctx, parentIPBlock.Prefix, parentIPBlock.PrefixLength)
		// this is a consistency check
		if parentCidr != childCidr {
			// this should never happen, ie, in a full grant, parent cidr and child cidr should match
			return fmt.Errorf("parent IPBlock has full grant, but child CIDR %s does not match parent CIDR %s", childCidr, parentCidr)
		}
		ipbDAO := cdbm.NewIPBlockDAO(dbSession)
		_, err := ipbDAO.Update(
			ctx,
			tx,
			cdbm.IPBlockUpdateInput{
				IPBlockID: parentIPBlock.ID,
				FullGrant: cutil.GetPtr(false),
			},
		)

		if err != nil {
			return fmt.Errorf("unable to update IPBlock full grant for IPBlock %s", parentIPBlock.ID)
		}
		parentIPBlock.FullGrant = false
		return nil
	}
	ipamer := cipam.NewWithStorage(ipamDB)
	namespace := GetIpamNamespaceForIPBlock(ctx, parentIPBlock.RoutingType, parentIPBlock.InfrastructureProviderID.String(), parentIPBlock.SiteID.String())
	ipamer.SetNamespace(namespace)
	prefix := ipamer.PrefixFrom(ctx, childCidr)
	if prefix == nil {
		return ErrPrefixDoesNotExistForIPBlock
	}
	err := ipamer.ReleaseChildPrefix(ctx, prefix)
	if err != nil {
		return err
	}
	return nil
}
