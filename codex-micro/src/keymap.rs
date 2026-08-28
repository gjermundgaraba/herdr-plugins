use anyhow::{Context, Result, anyhow, bail};
use base64::{Engine, engine::general_purpose::STANDARD};
use serde_json::{Value, json};
use std::time::Instant;

const READ_CHUNK: usize = 512;
const WRITE_CHUNK: usize = 384;
// ponytail: Codex keymaps are much smaller; raise this only if firmware grows past it.
pub(crate) const MAX_KEYMAP_SIZE: usize = 8 * 1024 * 1024;

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

fn check_deadline(deadline: Instant, operation: &str) -> Result<()> {
    if Instant::now() >= deadline {
        bail!("keymap {operation} timed out");
    }
    Ok(())
}

fn read_keymap_with<F>(mut read: F, deadline: Instant) -> Result<Vec<u8>>
where
    F: FnMut(usize) -> Result<Value>,
{
    let mut offset = 0;
    let mut expected_total = None;
    let mut body = Vec::new();
    loop {
        check_deadline(deadline, "read")?;
        let (chunk, total) = keymap_chunk(read(offset)?)?;
        if total > MAX_KEYMAP_SIZE {
            bail!("keymap exceeds {MAX_KEYMAP_SIZE} bytes");
        }
        match expected_total {
            None => {
                body.try_reserve_exact(total)
                    .context("cannot allocate keymap buffer")?;
                expected_total = Some(total);
            }
            Some(expected) if expected != total => bail!("keymap size changed while reading"),
            Some(_) => {}
        }
        if chunk.is_empty() {
            bail!("empty keymap chunk");
        }
        if chunk.len() > total - offset {
            bail!("invalid keymap chunk size");
        }
        offset += chunk.len();
        body.extend(chunk);
        if offset >= total {
            return Ok(body);
        }
    }
}

pub(crate) fn read_keymap_until(
    request: impl Fn(&str, Option<Value>) -> Result<Value>,
    deadline: Instant,
) -> Result<Vec<u8>> {
    read_keymap_with(
        |offset| {
            request(
                "fs.readbin",
                Some(json!({ "file": "keymap.json", "offset": offset, "len": READ_CHUNK })),
            )
        },
        deadline,
    )
}

fn write_keymap_chunks_with<F>(bytes: &[u8], deadline: Instant, mut write: F) -> Result<()>
where
    F: FnMut(usize, &[u8], bool) -> Result<()>,
{
    if bytes.is_empty() {
        bail!("keymap is empty");
    }
    for (offset, chunk) in bytes.chunks(WRITE_CHUNK).enumerate() {
        let offset = offset * WRITE_CHUNK;
        check_deadline(deadline, "write")?;
        write(offset, chunk, offset + chunk.len() == bytes.len())?;
    }
    Ok(())
}

pub(crate) fn write_keymap_until(
    request: impl Fn(&str, Option<Value>) -> Result<Value>,
    bytes: &[u8],
    deadline: Instant,
) -> Result<()> {
    write_keymap_chunks_with(bytes, deadline, |offset, data, completed| {
        request(
            "fs.writebin",
            Some(json!({
                "file": "keymap.json",
                "offset": offset,
                "data": STANDARD.encode(data),
                "append": true,
                "completed": completed,
            })),
        )
        .map(|_| ())
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn far_deadline() -> Instant {
        Instant::now() + std::time::Duration::from_secs(3600)
    }

    #[test]
    fn rejects_oversized_or_changing_keymaps() {
        let oversized = read_keymap_with(
            |_| {
                Ok(json!({
                    "data": STANDARD.encode(b"a"),
                    "total_size": MAX_KEYMAP_SIZE + 1
                }))
            },
            far_deadline(),
        )
        .unwrap_err();
        assert!(oversized.to_string().contains("exceeds"));

        let changing = read_keymap_with(
            |offset| {
                Ok(json!({
                    "data": STANDARD.encode(b"a"),
                    "total_size": if offset == 0 { 2 } else { 3 }
                }))
            },
            far_deadline(),
        )
        .unwrap_err();
        assert!(changing.to_string().contains("size changed"));
    }

    #[test]
    fn reads_and_writes_exact_chunks() {
        let bytes = b"abcdef".to_vec();
        let read = read_keymap_with(
            |offset| {
                let chunk = &bytes[offset..bytes.len().min(offset + 2)];
                Ok(json!({ "data": STANDARD.encode(chunk), "total_size": bytes.len() }))
            },
            far_deadline(),
        )
        .unwrap();
        assert_eq!(read, bytes);
        let mut chunks = Vec::new();
        write_keymap_chunks_with(&vec![1; 800], far_deadline(), |offset, data, completed| {
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
    fn expired_deadline_stops_before_any_request() {
        let deadline = Instant::now();
        let requests = std::cell::Cell::new(0);
        let count = |_: &str, _: Option<Value>| {
            requests.set(requests.get() + 1);
            Ok(Value::Null)
        };

        assert_eq!(
            read_keymap_until(count, deadline).unwrap_err().to_string(),
            "keymap read timed out"
        );
        assert_eq!(
            write_keymap_until(count, &[0; 1], deadline)
                .unwrap_err()
                .to_string(),
            "keymap write timed out"
        );
        assert_eq!(requests.get(), 0);
    }
}
