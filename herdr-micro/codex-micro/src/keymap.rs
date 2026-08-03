use std::path::PathBuf;

use anyhow::{anyhow, bail, Context, Result};
use base64::{engine::general_purpose::STANDARD, Engine};
use serde_json::{json, Value};

use crate::{MicroDevice, DEFAULT_REQUEST_TIMEOUT};

const READ_CHUNK: usize = 512;
const WRITE_CHUNK: usize = 384;

fn keymap_chunk(value: Value) -> Result<(Vec<u8>, usize)> {
    let data = value
        .get("data")
        .and_then(Value::as_str)
        .ok_or_else(|| anyhow!("keymap response is missing data"))?;
    let total = value
        .get("total_size")
        .and_then(Value::as_u64)
        .ok_or_else(|| anyhow!("keymap response is missing total_size"))?
        .try_into()
        .map_err(|_| anyhow!("keymap is too large"))?;
    Ok((STANDARD.decode(data).context("invalid keymap data")?, total))
}

pub fn read_keymap_with<F>(mut read: F) -> Result<Vec<u8>>
where
    F: FnMut(usize) -> Result<Value>,
{
    let mut offset = 0;
    let mut body = Vec::new();
    loop {
        let (chunk, total) = keymap_chunk(read(offset)?)?;
        if chunk.is_empty() {
            bail!("empty keymap chunk");
        }
        if offset
            .checked_add(chunk.len())
            .map_or(true, |end| end > total)
        {
            bail!("invalid keymap chunk size");
        }
        offset += chunk.len();
        body.extend(chunk);
        if offset >= total {
            return Ok(body);
        }
    }
}

pub fn read_keymap(device: &MicroDevice) -> Result<Vec<u8>> {
    read_keymap_with(|offset| {
        device.request(
            "fs.readbin",
            Some(json!({ "file": "keymap.json", "offset": offset, "len": READ_CHUNK })),
            DEFAULT_REQUEST_TIMEOUT,
        )
    })
}

pub fn write_keymap_chunks_with<F>(bytes: &[u8], mut write: F) -> Result<()>
where
    F: FnMut(usize, &[u8], bool) -> Result<()>,
{
    if bytes.is_empty() {
        bail!("keymap is empty");
    }
    for (offset, chunk) in bytes.chunks(WRITE_CHUNK).enumerate() {
        let offset = offset * WRITE_CHUNK;
        write(offset, chunk, offset + chunk.len() == bytes.len())?;
    }
    Ok(())
}

pub fn write_keymap(device: &MicroDevice, bytes: &[u8]) -> Result<()> {
    write_keymap_chunks_with(bytes, |offset, data, completed| {
        device
            .request(
                "fs.writebin",
                Some(json!({
                    "file": "keymap.json",
                    "offset": offset,
                    "data": STANDARD.encode(data),
                    "append": true,
                    "completed": completed,
                })),
                DEFAULT_REQUEST_TIMEOUT,
            )
            .map(|_| ())
    })
}

pub fn update_keymap_with<B, R, W>(
    before: &[u8],
    after: &[u8],
    backup: B,
    mut read: R,
    mut write: W,
) -> Result<Option<PathBuf>>
where
    B: FnOnce(&[u8]) -> Result<PathBuf>,
    R: FnMut() -> Result<Vec<u8>>,
    W: FnMut(&[u8]) -> Result<()>,
{
    if before == after {
        return Ok(None);
    }
    let backup = backup(before)?;
    let updated = write(after).and_then(|()| {
        if read()? == after {
            Ok(())
        } else {
            bail!("keymap read-back failed")
        }
    });
    if let Err(error) = updated {
        let restored = write(before).and_then(|()| {
            if read()? == before {
                Ok(())
            } else {
                bail!("restored keymap read-back failed")
            }
        });
        return match restored {
            Ok(()) => Err(error).with_context(|| {
                format!(
                    "keymap update failed; original keymap was restored; backup: {}",
                    backup.display()
                )
            }),
            Err(recovery) => Err(anyhow!(
                "keymap update failed: {error}; automatic restore failed: {recovery}; recover from backup: {}",
                backup.display()
            )),
        };
    }
    Ok(Some(backup))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_and_writes_exact_chunks() {
        let bytes = b"abcdef".to_vec();
        let read = read_keymap_with(|offset| {
            let chunk = &bytes[offset..bytes.len().min(offset + 2)];
            Ok(json!({ "data": STANDARD.encode(chunk), "total_size": bytes.len() }))
        })
        .unwrap();
        assert_eq!(read, bytes);
        let mut chunks = Vec::new();
        write_keymap_chunks_with(&vec![1; 800], |offset, data, completed| {
            chunks.push((offset, data.len(), completed));
            Ok(())
        })
        .unwrap();
        assert_eq!(
            chunks,
            [(0, 384, false), (384, 384, false), (768, 32, true)]
        );
    }

    #[test]
    fn update_failure_restores_original_and_names_backup() {
        let backup = PathBuf::from("/tmp/keymap-backup.json");
        let mut writes = Vec::new();
        let mut reads = [b"wrong".to_vec(), b"before".to_vec()].into_iter();
        let error = update_keymap_with(
            b"before",
            b"after",
            |_| Ok(backup.clone()),
            || Ok(reads.next().unwrap()),
            |bytes| {
                writes.push(bytes.to_vec());
                Ok(())
            },
        )
        .unwrap_err();
        assert_eq!(writes, vec![b"after".to_vec(), b"before".to_vec()]);
        assert!(error.to_string().contains("/tmp/keymap-backup.json"));
        assert!(error.to_string().contains("restored"));
    }

    #[test]
    fn failed_restore_still_names_backup() {
        let backup = PathBuf::from("/tmp/keymap-backup.json");
        let mut writes = 0;
        let error = update_keymap_with(
            b"before",
            b"after",
            |_| Ok(backup.clone()),
            || Ok(b"wrong".to_vec()),
            |_| {
                writes += 1;
                if writes == 2 {
                    bail!("device disappeared")
                }
                Ok(())
            },
        )
        .unwrap_err();
        assert!(error.to_string().contains("automatic restore failed"));
        assert!(error.to_string().contains("/tmp/keymap-backup.json"));
    }
}
