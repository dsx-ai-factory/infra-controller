/*
 * SPDX-FileCopyrightText: Copyright (c) 2020 The metal-stack Authors
 * SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
 * SPDX-License-Identifier: MIT AND Apache-2.0
 */

package ipam

import (
	"context"
	"fmt"
	"strings"
	"sync"
)

type memory struct {
	prefixes map[string]Prefix
	lock     sync.RWMutex
}

// NewMemory create a memory storage for ipam
func NewMemory(ctx context.Context) Storage {
	prefixes := make(map[string]Prefix)
	return &memory{
		prefixes: prefixes,
		lock:     sync.RWMutex{},
	}
}
func (m *memory) Name() string {
	return "memory"
}
func (m *memory) CreatePrefix(_ context.Context, prefix Prefix, namespace string) (Prefix, error) {
	m.lock.Lock()
	defer m.lock.Unlock()
	key := prefix.Cidr + "@" + prefix.Namespace
	_, ok := m.prefixes[key]
	if ok {
		return Prefix{}, fmt.Errorf("prefix already created:%v", prefix)
	}
	m.prefixes[key] = *prefix.deepCopy()
	return prefix, nil
}
func (m *memory) ReadPrefix(_ context.Context, prefix, namespace string) (Prefix, error) {
	m.lock.RLock()
	defer m.lock.RUnlock()
	key := prefix + "@" + namespace
	result, ok := m.prefixes[key]
	if !ok {
		return Prefix{}, fmt.Errorf("prefix %s not found", prefix)
	}
	return *result.deepCopy(), nil
}

func (m *memory) ReadPrefixes(ctx context.Context, namespace string) ([]Prefix, error) {
	m.lock.RLock()
	defer m.lock.RUnlock()

	ps := make([]Prefix, 0, len(m.prefixes))
	for k, v := range m.prefixes {
		if strings.HasSuffix(k, "@"+namespace) {
			ps = append(ps, *v.deepCopy())
		}
	}
	return ps, nil
}

func (m *memory) DeleteAllPrefixes(_ context.Context, namespace string) error {
	m.lock.RLock()
	defer m.lock.RUnlock()
	m.prefixes = make(map[string]Prefix)
	return nil
}
func (m *memory) ReadAllPrefixes(_ context.Context, namespace string) (Prefixes, error) {
	m.lock.RLock()
	defer m.lock.RUnlock()

	ps := make(Prefixes, 0, len(m.prefixes))
	for _, v := range m.prefixes {
		ps = append(ps, *v.deepCopy())
	}
	return ps, nil
}
func (m *memory) ReadAllPrefixCidrs(_ context.Context, namespace string) ([]string, error) {
	m.lock.RLock()
	defer m.lock.RUnlock()

	ps := make([]string, 0, len(m.prefixes))
	for cidr := range m.prefixes {
		if strings.HasSuffix(cidr, "@"+namespace) {
			c := strings.TrimSuffix(cidr, "@"+namespace)
			ps = append(ps, c)
		}
	}
	return ps, nil
}
func (m *memory) UpdatePrefix(_ context.Context, prefix Prefix, namespace string) (Prefix, error) {
	m.lock.Lock()
	defer m.lock.Unlock()

	oldVersion := prefix.version
	prefix.version = oldVersion + 1

	if prefix.Cidr == "" {
		return Prefix{}, fmt.Errorf("prefix not present:%v", prefix)
	}
	key := prefix.Cidr + "@" + prefix.Namespace
	_, ok := m.prefixes[key]
	if !ok {
		return Prefix{}, fmt.Errorf("prefix not found:%s", prefix.Cidr)
	}
	oldPrefix, ok := m.prefixes[key]
	if !ok {
		return Prefix{}, fmt.Errorf("prefix not found:%s", prefix.Cidr)
	}

	if oldPrefix.version != oldVersion {
		return Prefix{}, fmt.Errorf("%w: unable to update prefix:%s", ErrOptimisticLockError, prefix.Cidr)
	}
	m.prefixes[key] = *prefix.deepCopy()
	return prefix, nil
}
func (m *memory) DeletePrefix(_ context.Context, prefix Prefix, namespace string) (Prefix, error) {
	m.lock.Lock()
	defer m.lock.Unlock()
	key := prefix.Cidr + "@" + prefix.Namespace
	delete(m.prefixes, key)
	return *prefix.deepCopy(), nil
}

func (m *memory) CreateNamespace(_ context.Context, namespace string) error {
	return nil
}

func (m *memory) ListNamespaces(_ context.Context) ([]string, error) {
	// inefficient but used mainly in tests..
	keylist := []string{}
	namespaces := map[string]bool{}
	for _, p := range m.prefixes {
		if _, ok := namespaces[p.Namespace]; !ok {
			namespaces[p.Namespace] = true
			keylist = append(keylist, p.Namespace)
		}
	}
	return keylist, nil
}

func (m *memory) DeleteNamespace(_ context.Context, namespace string) error {
	return nil
}
