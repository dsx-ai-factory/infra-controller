// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package tui

import (
	"context"
	"fmt"
	"strings"
)

// FetchFunc fetches resources from the API and returns them as NamedItems.
type FetchFunc func(ctx context.Context) ([]NamedItem, error)

// Resolver resolves resource names to UUIDs using the cache and interactive select.
type Resolver struct {
	cache    *Cache
	fetchers map[string]FetchFunc
}

func NewResolver(cache *Cache) *Resolver {
	return &Resolver{
		cache:    cache,
		fetchers: make(map[string]FetchFunc),
	}
}

func (r *Resolver) RegisterFetcher(resourceType string, fn FetchFunc) {
	r.fetchers[resourceType] = fn
}

// HasFetcher reports whether resourceType supports interactive name/ID
// resolution in this session.
func (r *Resolver) HasFetcher(resourceType string) bool {
	if r == nil {
		return false
	}
	_, ok := r.fetchers[resourceType]
	return ok
}

func (r *Resolver) Fetch(ctx context.Context, resourceType string) ([]NamedItem, error) {
	if items := r.cache.Get(resourceType); items != nil {
		return items, nil
	}
	fn, ok := r.fetchers[resourceType]
	if !ok {
		return nil, fmt.Errorf("no fetcher registered for resource type %q", resourceType)
	}
	items, err := fn(ctx)
	if err != nil {
		return nil, err
	}
	r.cache.Set(resourceType, items)
	return items, nil
}

func (r *Resolver) Resolve(ctx context.Context, resourceType, label string) (*NamedItem, error) {
	items, err := r.Fetch(ctx, resourceType)
	if err != nil {
		return nil, fmt.Errorf("fetching %s: %w", resourceType, err)
	}
	return r.SelectFromItems(label, items)
}

func (r *Resolver) SelectFromItems(label string, items []NamedItem) (*NamedItem, error) {
	if len(items) == 0 {
		return nil, fmt.Errorf("no %s available", label)
	}
	if len(items) == 1 {
		fmt.Printf("%s %s %s\n", Bold(label+":"), Green(items[0].Name), Dim("(auto-selected)"))
		return &items[0], nil
	}
	selectItems := make([]SelectItem, len(items))
	for i, item := range items {
		lbl := item.Name
		if item.Status != "" {
			lbl += "  " + Dim(item.Status)
		}
		selectItems[i] = SelectItem{Label: lbl, ID: item.ID}
	}
	selected, err := Select(label+":", selectItems)
	if err != nil {
		return nil, err
	}
	for _, item := range items {
		if item.ID == selected.ID {
			return &item, nil
		}
	}
	return nil, fmt.Errorf("selected item not found")
}

func (r *Resolver) ResolveWithArgs(ctx context.Context, resourceType, label string, args []string) (*NamedItem, error) {
	items, err := r.Fetch(ctx, resourceType)
	if err != nil {
		return nil, fmt.Errorf("fetching %s: %w", resourceType, err)
	}
	if len(args) > 0 && args[0] != "" {
		query := strings.ToLower(args[0])
		for _, item := range items {
			if strings.ToLower(item.Name) == query || strings.ToLower(item.ID) == query {
				fmt.Printf("%s %s %s\n", Bold(label+":"), Green(item.Name), Dim("(matched)"))
				return &item, nil
			}
		}
		return nil, fmt.Errorf("no %s matching %q found", resourceType, args[0])
	}
	return r.SelectFromItems(label, items)
}

func (r *Resolver) ResolveID(resourceType, id string) string {
	item := r.cache.LookupByID(resourceType, id)
	if item != nil {
		return item.Name
	}
	return id
}
