// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0
#include <dhcp/pkt6.h>
#include <hooks/hooks.h>
#include <cstdlib>
#include <fcntl.h>
#include <time.h>
#include <unistd.h>

// Loaded before the production hook only by integration tests. Native Kea has
// already read the DUID/IAID leases; pause before ownership validation or writes.
static int before_existing(isc::hooks::CalloutHandle& handle) {
    const char* control = std::getenv("NICO_TEST_LEASE_READ_CONTROL");
    if (!control) return 0;
    int fd = open(control, O_RDWR | O_CLOEXEC);
    if (fd < 0) return 0;
    uint64_t mode = 0, client = 0;
    pread(fd, &mode, sizeof(mode), 0);
    pread(fd, &client, sizeof(client), 16);
    isc::dhcp::Pkt6Ptr query;
    handle.getArgument("query6", query);
    if (mode == 2 && query && (query->getTransid() & 0xff) == client) {
        const uint64_t hit = 1;
        pwrite(fd, &hit, sizeof(hit), 8);
        const timespec delay = {0, 10000000};
        for (int i = 0; mode && i < 1000; ++i) {
            nanosleep(&delay, nullptr);
            pread(fd, &mode, sizeof(mode), 0);
        }
    }
    close(fd);
    return 0;
}

// Corrupt one native argument to exercise an exception inside the production
// SELECT hook. The pending lease must remain uncommitted and its guard releasable.
static int before_select(isc::hooks::CalloutHandle& handle) {
    const char* control = std::getenv("NICO_TEST_LEASE_READ_CONTROL");
    if (!control) return 0;
    int fd = open(control, O_RDWR | O_CLOEXEC);
    if (fd < 0) return 0;
    uint64_t mode = 0;
    pread(fd, &mode, sizeof(mode), 0);
    if (mode == 3) {
        handle.setArgument("subnet6", 0);
        const uint64_t hit = 1;
        pwrite(fd, &hit, sizeof(hit), 8);
    }
    close(fd);
    return 0;
}

extern "C" {
int version() { return KEA_HOOKS_VERSION; }
int multi_threading_compatible() { return 1; }
int load(isc::hooks::LibraryHandle& handle) {
    handle.registerCallout("lease6_existing", before_existing);
    handle.registerCallout("lease6_select", before_select);
    return 0;
}
}
