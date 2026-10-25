//! Bounded cold DTO serialization. Payload bytes never use this codec.
use crate::abi::MAX_DTO_BYTES;
use std::io::{self, Write};
struct Capped(Vec<u8>);
impl Write for Capped {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        if bytes.len() > MAX_DTO_BYTES - self.0.len() {
            return Err(io::Error::other("Core DTO exceeds ABI bound"));
        }
        let length = self.0.len() + bytes.len();
        if length > self.0.capacity() {
            let capacity = length
                .max(self.0.capacity().saturating_mul(2))
                .min(MAX_DTO_BYTES);
            self.0.reserve_exact(capacity - self.0.len());
        }
        self.0.extend_from_slice(bytes);
        Ok(bytes.len())
    }
    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}
pub fn encode(value: &impl serde::Serialize) -> Result<Vec<u8>, serde_json::Error> {
    let mut output = Capped(Vec::new());
    serde_json::to_writer(&mut output, value)?;
    Ok(output.0)
}
#[cfg(test)]
#[path = "tests/unit/codec.rs"]
mod tests;
