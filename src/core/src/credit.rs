//! Conserved aggregate DATA permissions; callers own authenticated wire order.
use crate::{Frame, ProtocolError};

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub(crate) struct Amount {
    pub bytes: u64,
    pub records: u64,
}
impl Amount {
    pub fn data(bytes: usize) -> Self {
        Self {
            bytes: bytes as u64,
            records: 1,
        }
    }
    pub fn contains(self, other: Self) -> bool {
        self.bytes >= other.bytes && self.records >= other.records
    }
    pub fn add(self, other: Self) -> Result<Self, ProtocolError> {
        Ok(Self {
            bytes: self
                .bytes
                .checked_add(other.bytes)
                .ok_or(ProtocolError::InvalidCredit)?,
            records: self
                .records
                .checked_add(other.records)
                .ok_or(ProtocolError::InvalidCredit)?,
        })
    }
    pub fn sub(self, other: Self) -> Result<Self, ProtocolError> {
        Ok(Self {
            bytes: self
                .bytes
                .checked_sub(other.bytes)
                .ok_or(ProtocolError::InvalidCredit)?,
            records: self
                .records
                .checked_sub(other.records)
                .ok_or(ProtocolError::InvalidCredit)?,
        })
    }
}

#[derive(Debug, Default)]
pub(crate) struct SendCredit {
    pub epoch: u64,
    pub dispatched: Amount,
    pub pending: Amount,
    pub consumed: Amount,
    pub allowed: Amount,
    pub freezing: Option<u64>,
    pub frozen: bool,
}
impl SendCredit {
    pub fn room(&self) -> Amount {
        if self.freezing.is_some() {
            return Amount::default();
        }
        self.allowed
            .sub(self.dispatched)
            .and_then(|a| a.sub(self.pending))
            .unwrap_or_default()
    }
    pub fn reserve(&mut self, amount: Amount) -> Result<(), ProtocolError> {
        if !self.room().contains(amount) {
            return Err(ProtocolError::InvalidCredit);
        }
        self.pending = self.pending.add(amount)?;
        Ok(())
    }
    pub fn release_pending(&mut self, amount: Amount) -> Result<(), ProtocolError> {
        self.pending = self.pending.sub(amount)?;
        Ok(())
    }
    pub fn dispatch(&mut self, amount: Amount) -> Result<(), ProtocolError> {
        let pending = self.pending.sub(amount)?;
        let dispatched = self.dispatched.add(amount)?;
        if !self.allowed.contains(dispatched.add(pending)?) {
            return Err(ProtocolError::InvalidCredit);
        }
        self.pending = pending;
        self.dispatched = dispatched;
        Ok(())
    }
    pub fn grant(
        &mut self,
        epoch: u64,
        consumed: Amount,
        allowed: Amount,
    ) -> Result<(), ProtocolError> {
        if epoch < self.epoch {
            return Ok(());
        }
        if !consumed.contains(self.consumed)
            || !self.dispatched.contains(consumed)
            || !allowed.contains(consumed)
            || !allowed.contains(self.dispatched.add(self.pending)?)
        {
            return Err(ProtocolError::InvalidCredit);
        }
        if epoch == self.epoch {
            if !consumed.contains(self.consumed) || !allowed.contains(self.allowed) {
                // Ordered transport makes a partial regression a protocol error.
                if self.consumed.contains(consumed) && self.allowed.contains(allowed) {
                    return Ok(());
                }
                return Err(ProtocolError::InvalidCredit);
            }
            if self.frozen {
                return Err(ProtocolError::InvalidCredit);
            }
        } else if self.freezing != Some(epoch) || !self.frozen {
            return Err(ProtocolError::InvalidCredit);
        }
        self.epoch = epoch;
        self.consumed = consumed;
        self.allowed = allowed;
        self.freezing = None;
        self.frozen = false;
        Ok(())
    }
    pub fn freeze(&mut self, epoch: u64) -> Result<(), ProtocolError> {
        if epoch <= self.epoch {
            return Ok(());
        }
        if self.epoch.checked_add(1) != Some(epoch) || self.freezing.is_some_and(|old| old != epoch)
        {
            return Err(ProtocolError::InvalidCredit);
        }
        self.freezing = Some(epoch);
        Ok(())
    }
    /// Extract only after every pending DATA frame was extracted in wire order.
    pub fn frozen_frame(&mut self) -> Option<Frame> {
        let frame = self.peek_frozen_frame()?;
        self.frozen = true;
        Some(frame)
    }
    pub fn peek_frozen_frame(&self) -> Option<Frame> {
        let epoch = self.freezing?;
        if self.pending != Amount::default() || self.frozen {
            return None;
        }
        Some(Frame::PeerFrozen {
            epoch,
            bytes: self.dispatched.bytes,
            records: self.dispatched.records,
        })
    }
}

#[derive(Debug, Default)]
pub(crate) struct ReceiveCredit {
    pub epoch: u64,
    pub received: Amount,
    pub consumed: Amount,
    pub allowed: Amount,
    pub freezing: Option<u64>,
}
impl ReceiveCredit {
    pub fn promise(&self) -> Amount {
        self.allowed
            .sub(self.consumed)
            .expect("receive credit conservation")
    }
    pub fn outstanding(&self) -> Amount {
        self.received
            .sub(self.consumed)
            .expect("receive consumption conservation")
    }
    pub fn receive(&mut self, amount: Amount) -> Result<(), ProtocolError> {
        let next = self.received.add(amount)?;
        if !self.allowed.contains(next) {
            return Err(ProtocolError::ReceiveWindowExceeded);
        }
        self.received = next;
        Ok(())
    }
    pub fn consume(&mut self, amount: Amount) -> Result<(), ProtocolError> {
        let next = self.consumed.add(amount)?;
        if !self.received.contains(next) {
            return Err(ProtocolError::InvalidCredit);
        }
        self.consumed = next;
        Ok(())
    }
    pub fn grant(&mut self, headroom: Amount) -> Result<(), ProtocolError> {
        if self.freezing.is_some() {
            return Err(ProtocolError::InvalidCredit);
        }
        let next = self.consumed.add(headroom)?;
        self.allowed.bytes = self.allowed.bytes.max(next.bytes);
        self.allowed.records = self.allowed.records.max(next.records);
        Ok(())
    }
    pub fn frame(&self, probe: u64) -> Frame {
        Frame::PeerGrant {
            epoch: self.epoch,
            consumed_bytes: self.consumed.bytes,
            limit_bytes: self.allowed.bytes,
            consumed_records: self.consumed.records,
            limit_records: self.allowed.records,
            probe,
        }
    }
    pub fn freeze(&mut self) -> Result<Frame, ProtocolError> {
        if self.freezing.is_some() {
            return Err(ProtocolError::InvalidCredit);
        }
        let epoch = self
            .epoch
            .checked_add(1)
            .ok_or(ProtocolError::InvalidCredit)?;
        self.freezing = Some(epoch);
        Ok(Frame::PeerFreeze { epoch })
    }
    /// The authenticated lane must have delivered every DATA before this ACK.
    pub fn frozen(
        &mut self,
        epoch: u64,
        dispatched: Amount,
        headroom: Amount,
    ) -> Result<(), ProtocolError> {
        if self.freezing != Some(epoch) || dispatched != self.received {
            return Err(ProtocolError::InvalidCredit);
        }
        let allowed = self.received.add(headroom)?;
        self.epoch = epoch;
        self.allowed = allowed;
        self.freezing = None;
        Ok(())
    }
}

#[cfg(test)]
#[path = "credit_tests.rs"]
mod tests;
