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
// Control layout (native u64): mode (0=pass, 1=EIO, 2=pause, 3=write+rollback EIO), hits, followed by
// a 512-byte CSV address/DUID prefix, then optional state+1 (0=any).
// Rust writes mode last when arming.
static ssize_t (*native_write)(int, const void *, size_t);
static int (*native_truncate)(const char *, off_t);
static FILE *(*native_fopen)(const char *, const char *);
static FILE *(*native_fopen64)(const char *, const char *);
static pthread_once_t once = PTHREAD_ONCE_INIT;
static pthread_mutex_t mutex = PTHREAD_MUTEX_INITIALIZER;

static void initialize(void) {
    native_write = dlsym(RTLD_NEXT, "write");
    native_truncate = dlsym(RTLD_NEXT, "truncate");
    native_fopen = dlsym(RTLD_NEXT, "fopen");
    native_fopen64 = dlsym(RTLD_NEXT, "fopen64");
    if (!native_write || !native_truncate || !native_fopen || !native_fopen64) _exit(125);
}

static int fail_reopen(const char *path, const char *access) {
    const char *target = getenv("NICO_TEST_LEASE_FILE");
    const char *control = getenv("NICO_TEST_LEASE_IO_CONTROL");
    if (!target || !control || strcmp(path, target)) return 0;
    int ctl = open(control, O_RDONLY | O_CLOEXEC);
    uint64_t mode = 0;
    if (ctl >= 0) {
        pread(ctl, &mode, sizeof(mode), 0);
        close(ctl);
    }
    // Mode 4 faults the read-only size probe but permits a destructive
    // create fallback. Mode 5 refuses every reopen of the retained file.
    if (mode == 5 || (mode == 4 && access[0] == 'r' && !strchr(access, '+'))) {
        errno = EIO;
        return 1;
    }
    return 0;
}

FILE *fopen(const char *path, const char *access) {
    pthread_once(&once, initialize);
    return fail_reopen(path, access) ? NULL : native_fopen(path, access);
}

FILE *fopen64(const char *path, const char *access) {
    pthread_once(&once, initialize);
    return fail_reopen(path, access) ? NULL : native_fopen64(path, access);
}

int truncate(const char *path, off_t length) {
    pthread_once(&once, initialize);
    const char *target = getenv("NICO_TEST_LEASE_FILE");
    const char *control = getenv("NICO_TEST_LEASE_IO_CONTROL");
    if (target && control && !strcmp(path, target)) {
        int ctl = open(control, O_RDONLY | O_CLOEXEC);
        uint64_t mode = 0;
        if (ctl >= 0) {
            pread(ctl, &mode, sizeof(mode), 0);
            close(ctl);
        }
        if (mode == 3) {
            errno = EIO;
            return -1;
        }
    }
    return native_truncate(path, length);
}

int truncate64(const char *path, off64_t length) {
    return truncate(path, length);
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

    uint64_t state_filter = 0;
    pread(ctl, &state_filter, sizeof(state_filter), 528);
    if (state_filter) {
        const char *row = memmem(buffer, length, prefix, strnlen(prefix, sizeof(prefix)));
        const char *end = (const char *)buffer + length;
        for (int column = 0; row && column < 13; ++column) {
            row = memchr(row, ',', end - row);
            if (row) ++row;
        }
        if (!row || row >= end || *row != '0' + state_filter - 1) {
            close(ctl);
            return native_write(fd, buffer, length);
        }
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

int nico_kea_lease6_handoff_contract(void) {
    return runtime_capability("nico_kea_lease6_handoff_contract");
}
