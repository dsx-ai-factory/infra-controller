// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0
use std::fs::{File, OpenOptions};
use std::net::Ipv6Addr;
use std::os::unix::fs::FileExt;
use std::path::Path;
use std::time::{Duration, Instant};

/// Controls only the test child's native memfile append, never production code.
pub(crate) struct LeaseIoFault {
    file: File,
}

impl LeaseIoFault {
    pub(crate) fn new(path: &Path) -> std::io::Result<Self> {
        let file = OpenOptions::new()
            .read(true)
            .write(true)
            .create_new(true)
            .open(path)?;
        file.set_len(528)?;
        Ok(Self { file })
    }

    // Match the new owner's CSV row; earlier retirement/deletion writes pass.
    // This tests the persistence contract without depending on the write count.
    pub(crate) fn arm(&self, address: Ipv6Addr, duid: &[u8], pause: bool) -> std::io::Result<()> {
        self.release()?;
        let duid = duid
            .iter()
            .map(|byte| format!("{byte:02x}"))
            .collect::<Vec<_>>()
            .join(":");
        let prefix = format!("{address},{duid},");
        assert!(prefix.len() < 512);
        let mut bytes = [0; 512];
        bytes[..prefix.len()].copy_from_slice(prefix.as_bytes());
        self.file.write_all_at(&bytes, 16)?;
        self.file.write_all_at(&0u64.to_ne_bytes(), 8)?;
        self.file
            .write_all_at(&(if pause { 2u64 } else { 1 }).to_ne_bytes(), 0)
    }

    pub(crate) fn wait_for_hit(&self, minimum: u64) -> std::io::Result<()> {
        let deadline = Instant::now() + Duration::from_secs(5);
        loop {
            let mut bytes = [0; 8];
            self.file.read_exact_at(&mut bytes, 8)?;
            if u64::from_ne_bytes(bytes) >= minimum {
                return Ok(());
            }
            if Instant::now() >= deadline {
                return Err(std::io::Error::new(
                    std::io::ErrorKind::TimedOut,
                    "native memfile write did not reach fault barrier",
                ));
            }
            std::thread::sleep(Duration::from_millis(10));
        }
    }

    pub(crate) fn release(&self) -> std::io::Result<()> {
        self.file.write_all_at(&0u64.to_ne_bytes(), 0)
    }
}
