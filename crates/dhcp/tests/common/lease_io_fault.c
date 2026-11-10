// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0
#define _GNU_SOURCE
#include <dlfcn.h>
#include <errno.h>
#include <fcntl.h>
#include <limits.h>
#include <pthread.h>
#include <stdint.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <time.h>
#include <unistd.h>

// Test-child-only interposition: fail or pause a complete CSV row BEFORE any
// bytes are written, while allowing earlier reclamation/tombstone writes.
// Control layout (native u64): mode (0=pass, 1=EIO, 2=pause), hits, followed by
// a NUL-terminated CSV address/DUID prefix. Rust writes mode last when arming.
static ssize_t (*native_write)(int, const void *, size_t);
static pthread_once_t once = PTHREAD_ONCE_INIT;
static pthread_mutex_t mutex = PTHREAD_MUTEX_INITIALIZER;

static void initialize(void) {
    native_write = dlsym(RTLD_NEXT, "write");
    if (!native_write) _exit(125);
}

ssize_t write(int fd, const void *buffer, size_t length) {
    pthread_once(&once, initialize);
    const char *target = getenv("NICO_TEST_LEASE_FILE");
    const char *control = getenv("NICO_TEST_LEASE_IO_CONTROL");
    char link[64], path[PATH_MAX];
    snprintf(link, sizeof(link), "/proc/self/fd/%d", fd);
    ssize_t count = target && control ? readlink(link, path, sizeof(path) - 1) : -1;
    if (count < 0) return native_write(fd, buffer, length);
    path[count] = '\0';
    if (strcmp(path, target)) return native_write(fd, buffer, length);

    int ctl = open(control, O_RDWR | O_CLOEXEC);
    if (ctl < 0) return native_write(fd, buffer, length);
    uint64_t mode = 0;
    char prefix[512] = {0};
    pread(ctl, &mode, sizeof(mode), 0);
    ssize_t prefix_length = pread(ctl, prefix, sizeof(prefix) - 1, 16);
    if (!mode || prefix_length <= 0 || !prefix[0] ||
        !memmem(buffer, length, prefix, strnlen(prefix, sizeof(prefix)))) {
        close(ctl);
        return native_write(fd, buffer, length);
    }

    pthread_mutex_lock(&mutex);
    uint64_t hits = 0;
    pread(ctl, &hits, sizeof(hits), 8);
    ++hits;
    pwrite(ctl, &hits, sizeof(hits), 8);
    pthread_mutex_unlock(&mutex);
    // Pause only this worker. Other packets must still reach the address guard.
    // Bound the pause so a panicking test cannot strand its Kea child.
    const struct timespec delay = {0, 10000000};
    for (int i = 0; mode == 2 && i < 1000; ++i) {
        nanosleep(&delay, NULL);
        pread(ctl, &mode, sizeof(mode), 0);
    }
    close(ctl);
    if (mode) {
        errno = EIO;
        return -1;
    }
    return native_write(fd, buffer, length);
}

// Preserve real capability detection by default; reject one contract only in
// the startup test. A genuinely missing native symbol still returns zero.
static int runtime_capability(const char *name) {
    const char *rejected = getenv("NICO_TEST_REJECT_KEA_CAPABILITY");
    if (rejected && (!strcmp(rejected, "*") || !strcmp(rejected, name))) return 0;
    int (*native)(void) = dlsym(RTLD_NEXT, name);
    return native ? native() : 0;
}

int nico_kea_lease6_select_contract(void) {
    return runtime_capability("nico_kea_lease6_select_contract");
}

int nico_kea_lease6_reuse_null_checked(void) {
    return runtime_capability("nico_kea_lease6_reuse_null_checked");
}
