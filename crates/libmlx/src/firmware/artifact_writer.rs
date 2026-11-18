// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

use std::path::Path;

use tokio::io::AsyncWriteExt;

use super::error::{FirmwareError, FirmwareResult};

// Administrative ceiling per staged artifact. Hardware capacity qualification
// remains separate from this 16 GiB limit.
pub(super) const MAX_ARTIFACT_BYTES: u64 = 16 * 1024 * 1024 * 1024;

/// `ArtifactWriter` bounds staged bytes without owning the preparation directory.
pub(super) struct ArtifactWriter {
    file: tokio::fs::File,
    max_bytes: u64,
    written_bytes: u64,
}

impl ArtifactWriter {
    /// Reject an oversized advertised body before creating or truncating a file.
    pub(super) async fn create(
        path: &Path,
        max_bytes: u64,
        advertised_length: Option<u64>,
    ) -> FirmwareResult<Self> {
        if advertised_length.is_some_and(|length| length > max_bytes) {
            return Err(byte_limit_exceeded());
        }
        Ok(Self {
            file: tokio::fs::File::create(path).await?,
            max_bytes,
            written_bytes: 0,
        })
    }

    /// Reject a chunk in full when it would exceed the cumulative byte limit.
    pub(super) async fn write_chunk(&mut self, chunk: &[u8]) -> FirmwareResult<()> {
        let written_bytes = self
            .written_bytes
            .checked_add(chunk.len() as u64)
            .filter(|length| *length <= self.max_bytes)
            .ok_or_else(byte_limit_exceeded)?;
        self.file.write_all(chunk).await?;
        self.written_bytes = written_bytes;
        Ok(())
    }

    /// Settle buffered Tokio writes before the caller removes or reuses the file.
    pub(super) async fn finish(mut self) -> FirmwareResult<()> {
        self.file.flush().await?;
        Ok(())
    }
}

/// Report a size failure without copying a source address into the diagnostic.
fn byte_limit_exceeded() -> FirmwareError {
    FirmwareError::HttpError("firmware artifact exceeds the byte limit".into())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Reject the entire overflowing chunk while settling the earlier prefix.
    /// A partial write up to the byte limit would violate this contract.
    #[tokio::test]
    async fn overflow_does_not_write_any_part_of_the_rejected_chunk() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("image.bin");
        let mut writer = ArtifactWriter::create(&path, 8, None).await.unwrap();
        writer.write_chunk(b"first").await.unwrap();
        assert!(writer.write_chunk(b"too large").await.is_err());
        writer.finish().await.unwrap();
        assert_eq!(tokio::fs::read(path).await.unwrap(), b"first");
    }
}
