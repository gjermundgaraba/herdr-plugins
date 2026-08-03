use serde_json::Value;

pub const REPORT_ID: u8 = 6;
pub const CHANNEL_RPC: u8 = 2;
pub const REPORT_SIZE: usize = 64;
pub const MAX_PAYLOAD: usize = 61;
pub const MAX_REASSEMBLED: usize = 64 * 1024;

pub fn encode_message(
    method: &str,
    params: Option<&Value>,
    id: Option<u64>,
) -> Result<Vec<[u8; REPORT_SIZE]>, serde_json::Error> {
    let mut envelope = serde_json::Map::new();
    envelope.insert("method".into(), Value::String(method.into()));
    if let Some(params) = params {
        envelope.insert("params".into(), params.clone());
    }
    if let Some(id) = id {
        envelope.insert("id".into(), Value::from(id));
    }
    let mut bytes = serde_json::to_vec(&Value::Object(envelope))?;
    bytes.extend_from_slice(b"\r\n");
    Ok(bytes
        .chunks(MAX_PAYLOAD)
        .map(|chunk| {
            let mut report = [0; REPORT_SIZE];
            report[0] = REPORT_ID;
            report[1] = CHANNEL_RPC;
            report[2] = chunk.len() as u8;
            report[3..3 + chunk.len()].copy_from_slice(chunk);
            report
        })
        .collect())
}

#[derive(Default, Debug)]
pub struct Reassembler {
    buffer: Vec<u8>,
}

impl Reassembler {
    pub fn push(&mut self, report: &[u8]) -> Vec<Result<Value, serde_json::Error>> {
        let offset = if report.get(0..2) == Some(&[REPORT_ID, CHANNEL_RPC]) {
            1
        } else if report.first() == Some(&CHANNEL_RPC) {
            0
        } else {
            return vec![];
        };
        if report.len() < offset + 2 {
            return vec![];
        }
        let length = report[offset + 1] as usize;
        if length > MAX_PAYLOAD || length + offset + 2 > report.len() {
            return vec![];
        }
        self.buffer
            .extend_from_slice(&report[offset + 2..offset + 2 + length]);
        let mut messages = Vec::new();
        while let Some(newline) = self.buffer.iter().position(|b| *b == b'\n') {
            let line: Vec<_> = self.buffer.drain(..=newline).collect();
            messages.push(serde_json::from_slice(&line[..line.len() - 1]));
        }
        if self.buffer.len() > MAX_REASSEMBLED {
            self.buffer.clear();
        }
        messages
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn accepts_report_id_variants_and_rejects_bad_reports() {
        let params = serde_json::json!({"ok":true});
        let reports = encode_message("event", Some(&params), Some(1)).unwrap();
        let mut reassembler = Reassembler::default();
        assert_eq!(
            reassembler.push(&reports[0])[0].as_ref().unwrap()["method"],
            "event"
        );
        assert_eq!(Reassembler::default().push(&reports[0][1..]).len(), 1);
        assert!(Reassembler::default()
            .push(&[REPORT_ID, CHANNEL_RPC, 62])
            .is_empty());
        let mut over = Reassembler::default();
        let mut fragment = vec![CHANNEL_RPC, 61];
        fragment.extend(std::iter::repeat(b'x').take(61));
        for _ in 0..1100 {
            over.push(&fragment);
        }
        assert!(over.buffer.len() < MAX_REASSEMBLED);
    }
}
