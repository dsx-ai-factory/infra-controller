/*
 * SPDX-FileCopyrightText: Copyright (c) 2020 The metal-stack Authors
 * SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
 * SPDX-License-Identifier: MIT AND Apache-2.0
 */

package ipam

import (
	"context"
	"sync"
)

type namespaceContextKey struct{}

const (
	defaultNamespace = "root"
)

// Ipamer can be used to do IPAM stuff.
type Ipamer interface {
	// SetNamespace to use for separating prefixes which is required to have overlapping prefixes for different purposes.
	// Prefixes must still be collision free inside the same namespace.
	// if not set, all prefixes/ips belong to a empty default namespace, collisions are not possible.
	SetNamespace(namespace string)
	// NewPrefix creates a new Prefix from a string notation.
	// This operation is scoped to the root namespace unless a different namespace is provided in the context.
	NewPrefix(ctx context.Context, cidr string) (*Prefix, error)
	// DeletePrefix delete a Prefix from a string notation.
	// If the Prefix is not found an NotFoundError is returned.
	// This operation is scoped to the root namespace unless a different namespace is provided in the context.
	DeletePrefix(ctx context.Context, cidr string) (*Prefix, error)
	// AcquireChildPrefix reserves a subnet with the given prefix length
	// within the existing `parentCidr` prefix.
	// This operation is scoped to the root namespace unless a different namespace is provided in the context.
	AcquireChildPrefix(ctx context.Context, parentCidr string, length uint8) (*Prefix, error)
	// AcquireSpecificChildPrefix reserves the subnet `childCidr` within
	// the containing `parentCidr` prefix.
	// Host bits in `childCidr` are accepted; the returned and reserved CIDR is the masked network prefix.
	// This operation is scoped to the root namespace unless a different namespace is provided in the context.
	AcquireSpecificChildPrefix(ctx context.Context, parentCidr, childCidr string) (*Prefix, error)
	// ReleaseChildPrefix will mark this child Prefix as available again.
	// This operation is scoped to the root namespace unless a different namespace is provided in the context.
	ReleaseChildPrefix(ctx context.Context, child *Prefix) error
	// PrefixFrom will return a known Prefix.
	// This operation is scoped to the root namespace unless a different namespace is provided in the context.
	PrefixFrom(ctx context.Context, cidr string) *Prefix
	// AcquireSpecificIP will acquire given IP and mark this IP as used, if already in use, return nil.
	// If specificIP is empty, the next free IP is returned.
	// If there is no free IP an NoIPAvailableError is returned.
	// This operation is scoped to the root namespace unless a different namespace is provided in the context.
	AcquireSpecificIP(ctx context.Context, prefixCidr, specificIP string) (*IP, error)
	// AcquireIP will return the next unused IP from this Prefix.
	// This operation is scoped to the root namespace unless a different namespace is provided in the context.
	AcquireIP(ctx context.Context, prefixCidr string) (*IP, error)
	// ReleaseIP will release the given IP for later usage and returns the updated Prefix.
	// If the IP is not found an NotFoundError is returned.
	// This operation is scoped to the root namespace unless a different namespace is provided in the context.
	ReleaseIP(ctx context.Context, ip *IP) (*Prefix, error)
	// ReleaseIPFromPrefix will release the given IP for later usage.
	// If the Prefix or the IP is not found an NotFoundError is returned.
	// This operation is scoped to the root namespace unless a different namespace is provided in the context.
	ReleaseIPFromPrefix(ctx context.Context, prefixCidr, ip string) error
	// Dump all stored prefixes as json formatted string
	// This operation is scoped to the root namespace unless a different namespace is provided in the context.
	Dump(ctx context.Context) (string, error)
	// Load a previously created json formatted dump, deletes all prefixes before loading.
	// This operation is scoped to the root namespace unless a different namespace is provided in the context.
	Load(ctx context.Context, dump string) error
	// ReadAllPrefixCidrs retrieves all existing Prefix CIDRs from the underlying storage.
	// This operation is scoped to the root namespace unless a different namespace is provided in the context.
	ReadAllPrefixCidrs(ctx context.Context) ([]string, error)
	// CreateNamespace creates a namespace with the given name.
	// Any namespace provided in the context is ignored for this operation.
	// It is idempotent, so attempts to create a namespace which already exists will not return an error.
	CreateNamespace(ctx context.Context, namespace string) error
	// ListNamespaces returns a list of all namespaces.
	// Any namespace provided in the context is ignored for this operation.
	ListNamespaces(ctx context.Context) ([]string, error)
	// DeleteNamespaces deletes the namespace with the given name.
	// Any namespace provided in the context is ignored for this operation.
	// It not idempotent, so attempts to delete a namespace which does not exist will return an error.
	DeleteNamespace(ctx context.Context, namespace string) error
	// GetSubnetCount will return the subnet count of a given prefix.
	GetSubnetCount(ctx context.Context, prefixCidr string) uint64
}

type ipamer struct {
	mu        sync.Mutex
	storage   Storage
	namespace string
}

func (i *ipamer) SetNamespace(namespace string) {
	i.namespace = namespace
}

// New returns a Ipamer with in memory storage for networks, prefixes and ips.
func New(ctx context.Context) Ipamer {
	storage := NewMemory(ctx)
	return &ipamer{storage: storage}
}

// NewWithStorage allows you to create a Ipamer instance with your Storage implementation.
// The Storage interface must be implemented.
func NewWithStorage(storage Storage) Ipamer {
	return &ipamer{storage: storage}
}

func NewContextWithNamespace(ctx context.Context, namespace string) context.Context {
	return context.WithValue(ctx, namespaceContextKey{}, namespace)
}
