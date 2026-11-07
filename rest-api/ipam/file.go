/*
 * SPDX-FileCopyrightText: Copyright (c) 2020 The metal-stack Authors
 * SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
 * SPDX-License-Identifier: MIT AND Apache-2.0
 */

package ipam

import (
	"context"
	"encoding/json"
	"errors"
	"fmt"
	"io/fs"
	"os"
	"path"
	"sync"
	"time"
)

type file struct {
	// path location of the state file
	path string
	// prettyJSON is always true, but up for being configurable
	prettyJSON bool
	// modTime helps with tracking external file changes
	// usages of modTime will be deprecated after implementing filesystem-level locking
	modTime time.Time
	// parent implements internal state management, currently it is always NewMemory()
	parent Storage
	// lock at some point should be replaced with filesystem lock
	lock sync.RWMutex
}

var (
	nullModTime          time.Time
	DefaultLocalFilePath string
)

// fileJSONData is a representation of JSON file's structure
type fileJSONData map[string]map[string]prefixJSON

func init() {
	nullModTime = time.Unix(0, 0)
	DefaultLocalFilePath = path.Join(getXDGDataHome(), "go-ipam", "ipam-db.json")
}

// getXDGDataHome() is an utility for finding the most suitable data directory:
// 1) $XDG_DATA_HOME
// 2) $HOME/.local/share
// 3) current directory
func getXDGDataHome() string {
	if val := os.Getenv("XDG_DATA_HOME"); val != "" {
		return val
	}

	val, err := os.UserHomeDir()
	if err != nil {
		val = "."
	} else {
		val = path.Join(val, ".local", "share")
	}
	return val
}

// NewLocalFile creates a JSON file storage for ipam
func NewLocalFile(ctx context.Context, path string) Storage {
	return &file{
		path:       path,
		prettyJSON: true,
		parent:     NewMemory(ctx),
		modTime:    nullModTime,
		lock:       sync.RWMutex{},
	}
}

// clearParent() empties the internal state
func (f *file) clearParent(ctx context.Context) (err error) {
	namespaces, err := f.parent.ListNamespaces(ctx)
	if err != nil {
		return fmt.Errorf("failed to list namespaces: %w", err)
	}
	for _, namespace := range namespaces {
		if err = f.parent.DeleteAllPrefixes(ctx, namespace); err != nil {
			return fmt.Errorf("failed to delete prefixes for %s namespace: %w", namespace, err)
		}
		if namespace == defaultNamespace {
			// skip deletion instead of replicating NewMemory behavior
			continue
		}
		if err = f.parent.DeleteNamespace(ctx, namespace); err != nil {
			return fmt.Errorf("failed to delete %s namespace: %w", namespace, err)
		}
	}
	return nil
}

// getModTime() returns file's modification time without failure (defaulting to nullModTime).
func (f *file) getModTime() time.Time {
	info, err := os.Stat(f.path)
	if err != nil {
		return nullModTime
	}
	return info.ModTime()
}

// reload() is meant to synchronize from the file to internal state representation,
// currently it bases decision solely on the modification time of the file
//
// see ipamer.NamespacedLoad for alternative implementation candidate
// after https://github.com/metal-stack/go-ipam/issues/111 is addressed
func (f *file) reload(ctx context.Context) (err error) {
	// don't do anything when file modification time didn't changed
	if modTime := f.getModTime(); modTime != nullModTime && modTime == f.modTime {
		return nil
	}

	var data []byte
	storage := make(fileJSONData)
	if _, err = os.Stat(f.path); !errors.Is(err, fs.ErrNotExist) {
		data, err = os.ReadFile(f.path)
		if err != nil {
			return fmt.Errorf("failed to read state file %q: %w", f.path, err)
		}
	}
	f.modTime = f.getModTime()
	// smallest valid piece of data is "{}"
	if len(data) >= 2 {
		err = json.Unmarshal(data, &storage)
		if err != nil {
			return fmt.Errorf("failed to parse state file %q: %w", f.path, err)
		}
	}
	if err = f.clearParent(ctx); err != nil {
		return fmt.Errorf("failed to clear memory storage: %w", err)
	}
	for namespace, prefixes := range storage {
		if err = f.parent.CreateNamespace(ctx, namespace); err != nil {
			return fmt.Errorf("failed to reload a %s namespace: %w", namespace, err)
		}
		for _, prefix := range prefixes {
			if _, err = f.parent.CreatePrefix(ctx, prefix.toPrefix(), namespace); err != nil {
				return fmt.Errorf("failed to reload a %s prefix in %s namespace: %w", prefix.Cidr, namespace, err)
			}
		}
	}
	return nil
}

// persist() dumps current internal state to file
//
// see ipamer.NamespacedDump for alternative implementation candidate
// after https://github.com/metal-stack/go-ipam/issues/111 is addressed
func (f *file) persist(ctx context.Context) (err error) {
	storage := make(fileJSONData)
	var (
		prefixes map[string]prefixJSON
		ok       bool
		data     []byte
	)

	namespaces, err := f.parent.ListNamespaces(ctx)
	if err != nil {
		return fmt.Errorf("failed to list namespaces while building external state representation: %w", err)
	}
	for _, namespace := range namespaces {
		if prefixes, ok = storage[namespace]; !ok {
			prefixes = make(map[string]prefixJSON)
			storage[namespace] = prefixes
		}
		ps, err := f.parent.ReadAllPrefixes(ctx, namespace)
		if err != nil {
			return fmt.Errorf("failed to read prefixes of %s namespace while building external state representation: %w", namespace, err)
		}
		for _, prefix := range ps {
			prefixes[prefix.Cidr] = prefix.toPrefixJSON()
		}
	}
	if f.prettyJSON {
		data, err = json.MarshalIndent(storage, "", "  ")
	} else {
		data, err = json.Marshal(storage)
	}
	if err != nil {
		return fmt.Errorf("failed to serialize JSON: %w", err)
	}
	err = os.WriteFile(f.path, data, 0600)
	if err != nil {
		return fmt.Errorf("error storing state at %q: %w", f.path, err)
	}
	f.modTime = f.getModTime()
	return err
}
func (f *file) Name() string {
	return "file"
}

func (f *file) CreatePrefix(ctx context.Context, prefix Prefix, namespace string) (p Prefix, err error) {
	f.lock.Lock()
	defer f.lock.Unlock()

	if err = f.reload(ctx); err != nil {
		return p, err
	}

	if p, err = f.parent.CreatePrefix(ctx, prefix, namespace); err != nil {
		return p, err
	}

	return p, f.persist(ctx)
}

func (f *file) ReadPrefix(ctx context.Context, prefix, namespace string) (p Prefix, err error) {
	f.lock.RLock()
	defer f.lock.RUnlock()
	if err = f.reload(ctx); err != nil {
		return p, err
	}
	return f.parent.ReadPrefix(ctx, prefix, namespace)
}

func (f *file) DeleteAllPrefixes(ctx context.Context, namespace string) (err error) {
	f.lock.RLock()
	defer f.lock.RUnlock()

	if err = f.reload(ctx); err != nil {
		return err
	}
	if err = f.parent.DeleteAllPrefixes(ctx, namespace); err != nil {
		return err
	}
	return f.persist(ctx)
}

func (f *file) ReadAllPrefixes(ctx context.Context, namespace string) (ps Prefixes, err error) {
	f.lock.RLock()
	defer f.lock.RUnlock()

	if err = f.reload(ctx); err != nil {
		return ps, err
	}
	return f.parent.ReadAllPrefixes(ctx, namespace)
}

func (f *file) ReadPrefixes(ctx context.Context, namespace string) (ps []Prefix, err error) {
	f.lock.RLock()
	defer f.lock.RUnlock()

	if err = f.reload(ctx); err != nil {
		return ps, err
	}
	return f.parent.ReadAllPrefixes(ctx, namespace)
}

func (f *file) ReadAllPrefixCidrs(ctx context.Context, namespace string) (cidrs []string, err error) {
	f.lock.RLock()
	defer f.lock.RUnlock()

	if err = f.reload(ctx); err != nil {
		return cidrs, err
	}
	return f.parent.ReadAllPrefixCidrs(ctx, namespace)
}

func (f *file) UpdatePrefix(ctx context.Context, prefix Prefix, namespace string) (p Prefix, err error) {
	f.lock.Lock()
	defer f.lock.Unlock()

	if err = f.reload(ctx); err != nil {
		return p, err
	}
	if p, err = f.parent.UpdatePrefix(ctx, prefix, namespace); err != nil {
		return p, err
	}
	return p, f.persist(ctx)
}
func (f *file) DeletePrefix(ctx context.Context, prefix Prefix, namespace string) (p Prefix, err error) {
	f.lock.Lock()
	defer f.lock.Unlock()

	if err = f.reload(ctx); err != nil {
		return p, err
	}
	if p, err = f.parent.DeletePrefix(ctx, prefix, namespace); err != nil {
		return p, err
	}
	return p, f.persist(ctx)
}

func (f *file) CreateNamespace(ctx context.Context, namespace string) (err error) {
	f.lock.Lock()
	defer f.lock.Unlock()

	if err = f.reload(ctx); err != nil {
		return err
	}
	if err = f.parent.CreateNamespace(ctx, namespace); err != nil {
		return err
	}
	return f.persist(ctx)
}

func (f *file) ListNamespaces(ctx context.Context) (result []string, err error) {
	f.lock.Lock()
	defer f.lock.Unlock()
	if err = f.reload(ctx); err != nil {
		return result, err
	}
	return f.parent.ListNamespaces(ctx)
}

func (f *file) DeleteNamespace(ctx context.Context, namespace string) (err error) {
	f.lock.Lock()
	defer f.lock.Unlock()

	if err = f.reload(ctx); err != nil {
		return err
	}
	if err = f.parent.DeleteNamespace(ctx, namespace); err != nil {
		return err
	}
	return f.persist(ctx)
}
