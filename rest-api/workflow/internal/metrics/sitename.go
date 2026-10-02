// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package metrics

import (
	"context"
	"sync"

	"github.com/google/uuid"

	cdb "github.com/NVIDIA/infra-controller/rest-api/db/pkg/db"
	cdbm "github.com/NVIDIA/infra-controller/rest-api/db/pkg/db/model"
)

// SiteNameCache resolves Site names for metric labels without a DB read per
// observation. It is shared by every metrics activity rather than reimplemented
// per package, because the worker runs activities concurrently against one
// registered instance and an unsynchronized map here is a concurrent write that
// kills the process.
//
// A name is cached for the life of the worker. A Site renamed after its first
// observation keeps reporting the old name until the worker restarts, and the
// restart then splits its history across two values of the site label. Every
// metric using this cache also carries site_id, which stays stable across a
// rename, so that is the label to aggregate on.
type SiteNameCache struct {
	mutex sync.RWMutex
	names map[uuid.UUID]string
}

// Get returns the Site's name, reading it from the DB on the first call for
// that Site. Concurrent callers may both miss and read the same Site; that
// costs a duplicate query rather than holding the write lock across the DB call.
func (snc *SiteNameCache) Get(ctx context.Context, dbSession *cdb.Session, siteID uuid.UUID) (string, error) {
	snc.mutex.RLock()
	name, ok := snc.names[siteID]
	snc.mutex.RUnlock()
	if ok {
		return name, nil
	}

	siteDAO := cdbm.NewSiteDAO(dbSession)
	site, err := siteDAO.GetByID(ctx, nil, siteID, nil, false)
	if err != nil {
		return "", err
	}

	snc.mutex.Lock()
	snc.names[siteID] = site.Name
	snc.mutex.Unlock()

	return site.Name, nil
}

// Len returns the number of cached Sites. Exists for tests.
func (snc *SiteNameCache) Len() int {
	snc.mutex.RLock()
	defer snc.mutex.RUnlock()
	return len(snc.names)
}

// NewSiteNameCache returns a cache ready for concurrent use
func NewSiteNameCache() *SiteNameCache {
	return &SiteNameCache{names: map[uuid.UUID]string{}}
}
