//! Versioned runtime envelopes. Stream frames retain their own checked codec.

use super::RuntimeError;

pub(super) const DATA_HEADER: usize = 48;
pub(super) const CONTROL_BYTES: usize = 200;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) struct Control {
    pub kind: u8,
    pub sender: u128,
    pub recipient: u128,
    pub hello: u128,
    pub token: u128,
    pub watermark: u64,
    pub serial: u64,
    pub ack: u64,
    pub epoch: u64,
    pub records: u64,
    pub applied_bytes: u64,
    pub applied_records: u64,
    pub body_len: u8,
    pub body: [u8; 64],
}

impl Default for Control {
    fn default() -> Self {
        Self {
            kind: 0,
            sender: 0,
            recipient: 0,
            hello: 0,
            token: 0,
            watermark: 0,
            serial: 0,
            ack: 0,
            epoch: 0,
            records: 0,
            applied_bytes: 0,
            applied_records: 0,
            body_len: 0,
            body: [0; 64],
        }
    }
}

impl Control {
    pub fn encode(self) -> Vec<u8> {
        let mut bytes = Vec::with_capacity(CONTROL_BYTES);
        bytes.extend_from_slice(b"SKC3");
        bytes.extend_from_slice(&[self.kind, 0, 0, 0]);
        for value in [self.sender, self.recipient, self.hello, self.token] {
            bytes.extend_from_slice(&value.to_be_bytes());
        }
        for value in [
            self.watermark,
            self.serial,
            self.ack,
            self.epoch,
            self.records,
            self.applied_bytes,
            self.applied_records,
        ] {
            bytes.extend_from_slice(&value.to_be_bytes());
        }
        bytes.push(self.body_len);
        bytes.extend_from_slice(&[0; 7]);
        bytes.extend_from_slice(&self.body);
        bytes
    }

    pub fn decode(bytes: &[u8]) -> Result<Self, RuntimeError> {
        if bytes.len() != CONTROL_BYTES
            || &bytes[..4] != b"SKC3"
            || bytes[5..8] != [0, 0, 0]
            || !(1..=14).contains(&bytes[4])
            || bytes[128] > 64
            || bytes[129..136] != [0; 7]
            || bytes[136 + bytes[128] as usize..200]
                .iter()
                .any(|b| *b != 0)
        {
            return Err(RuntimeError::Protocol);
        }
        let wide = |offset| u128::from_be_bytes(bytes[offset..offset + 16].try_into().unwrap());
        let count = |offset| u64::from_be_bytes(bytes[offset..offset + 8].try_into().unwrap());
        Ok(Self {
            kind: bytes[4],
            sender: wide(8),
            recipient: wide(24),
            hello: wide(40),
            token: wide(56),
            watermark: count(72),
            serial: count(80),
            ack: count(88),
            epoch: count(96),
            records: count(104),
            applied_bytes: count(112),
            applied_records: count(120),
            body_len: bytes[128],
            body: bytes[136..200].try_into().unwrap(),
        })
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) struct DataHeader {
    pub token: u128,
    pub sequence: u64,
    pub control_ack: u64,
    pub epoch: u64,
}

impl DataHeader {
    pub fn encode(self, body_len: usize, bytes: &mut Vec<u8>) -> Result<(), RuntimeError> {
        let length = u32::try_from(body_len).map_err(|_| RuntimeError::Protocol)?;
        if self.sequence == 0 {
            return Err(RuntimeError::Protocol);
        }
        bytes.extend_from_slice(b"SKD3");
        bytes.extend_from_slice(&self.token.to_be_bytes());
        bytes.extend_from_slice(&self.sequence.to_be_bytes());
        bytes.extend_from_slice(&self.control_ack.to_be_bytes());
        bytes.extend_from_slice(&self.epoch.to_be_bytes());
        bytes.extend_from_slice(&length.to_be_bytes());
        Ok(())
    }

    pub fn decode(bytes: &[u8]) -> Result<(Self, &[u8]), RuntimeError> {
        if bytes.len() < DATA_HEADER || &bytes[..4] != b"SKD3" {
            return Err(RuntimeError::Protocol);
        }
        let count = |offset| u64::from_be_bytes(bytes[offset..offset + 8].try_into().unwrap());
        let length = u32::from_be_bytes(bytes[44..48].try_into().unwrap()) as usize;
        if length != bytes.len() - DATA_HEADER || count(20) == 0 {
            return Err(RuntimeError::Protocol);
        }
        Ok((
            Self {
                token: u128::from_be_bytes(bytes[4..20].try_into().unwrap()),
                sequence: count(20),
                control_ack: count(28),
                epoch: count(36),
            },
            &bytes[DATA_HEADER..],
        ))
    }
}

#[cfg(test)]
#[path = "../tests/unit/runtime/protocol.rs"]
mod tests;
