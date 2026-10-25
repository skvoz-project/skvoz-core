//! Continuous, conserved delivery permissions. No payload is retained here.

use super::RuntimeError;
use crate::peer_credit::Amount;
use std::{
    collections::VecDeque,
    time::{Duration, Instant},
};

/// One outstanding sequenced control. Receipts themselves are not sequenced.
#[derive(Debug, Default)]
pub(super) struct ControlWindow {
    pub sent: u64,
    pub acknowledged: u64,
    pub received: u64,
    pub receipt_sent: u64,
    pub deadline: Option<Instant>,
}

impl ControlWindow {
    pub fn begin(&mut self, now: Instant, timeout: Duration) -> Result<Option<u64>, RuntimeError> {
        if self.deadline.is_some() {
            return Ok(None);
        }
        self.sent = self
            .sent
            .checked_add(1)
            .ok_or(RuntimeError::IdentityExhausted)?;
        self.deadline = Some(now + timeout);
        Ok(Some(self.sent))
    }

    pub fn acknowledge(&mut self, sequence: u64) -> Result<(), RuntimeError> {
        if sequence > self.sent {
            return Err(RuntimeError::Protocol);
        }
        if sequence > self.acknowledged {
            self.acknowledged = sequence;
            self.deadline = None;
        }
        Ok(())
    }

    pub fn fresh(&self, sequence: u64) -> Result<bool, RuntimeError> {
        if sequence == 0
            || sequence
                > self
                    .received
                    .checked_add(1)
                    .ok_or(RuntimeError::IdentityExhausted)?
        {
            return Err(RuntimeError::Protocol);
        }
        Ok(sequence > self.received)
    }

    /// Invoke only after the complete action, including any DATA prefix barrier.
    pub fn applied(&mut self, sequence: u64) -> Result<(), RuntimeError> {
        if self.fresh(sequence)? {
            self.received = sequence;
        }
        Ok(())
    }

    pub fn receipt_dirty(&self) -> bool {
        self.received > self.receipt_sent
    }

    pub fn mark_receipt_sent(&mut self) {
        self.receipt_sent = self.received;
    }
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub(super) struct Limit {
    pub epoch: u64,
    pub allowed: Amount,
    pub applied: Amount,
}

/// Local reservations and committed publication are distinct ownership stages.
#[derive(Debug, Default)]
pub(super) struct Sender {
    pub epoch: u64,
    pub allowed: Amount,
    pub pending: Amount,
    pub sent: Amount,
    pub applied: Amount,
    pub freezing: Option<u64>,
    ends: VecDeque<Amount>,
}

impl Sender {
    pub fn room(&self) -> Amount {
        if self.freezing.is_some() {
            return Amount::default();
        }
        self.allowed
            .sub(self.sent)
            .and_then(|v| v.sub(self.pending))
            .unwrap_or_default()
    }

    pub fn grant(&mut self, limit: Limit) -> Result<(), RuntimeError> {
        if limit.epoch < self.epoch {
            return Ok(());
        }
        if (limit.applied != self.applied && !self.ends.contains(&limit.applied))
            || !self.sent.contains(limit.applied)
            || !limit.applied.contains(self.applied)
            || !limit.allowed.contains(self.sent.add(self.pending)?)
        {
            return Err(RuntimeError::Protocol);
        }
        if limit.epoch == self.epoch {
            if self.freezing.is_some() {
                return Ok(());
            }
            if !limit.allowed.contains(self.allowed) {
                return Err(RuntimeError::Protocol);
            }
        } else if self.freezing != Some(limit.epoch) || self.pending != Amount::default() {
            return Err(RuntimeError::Protocol);
        }
        self.epoch = limit.epoch;
        self.allowed = limit.allowed;
        self.applied = limit.applied;
        while self
            .ends
            .front()
            .is_some_and(|end| end.records <= limit.applied.records)
        {
            self.ends.pop_front();
        }
        self.freezing = None;
        Ok(())
    }

    pub fn apply_receipt(&mut self, applied: Amount) -> Result<(), RuntimeError> {
        // A matched older progress probe may arrive after a newer CREDIT.
        if self.applied.contains(applied) {
            return Ok(());
        }
        if !self.sent.contains(applied)
            || !applied.contains(self.applied)
            || (applied != self.applied && !self.ends.contains(&applied))
        {
            return Err(RuntimeError::Protocol);
        }
        self.applied = applied;
        while self
            .ends
            .front()
            .is_some_and(|end| end.records <= applied.records)
        {
            self.ends.pop_front();
        }
        Ok(())
    }
    pub fn reserve(&mut self, amount: Amount) -> Result<(), RuntimeError> {
        if !self.room().contains(amount) {
            return Err(RuntimeError::Protocol);
        }
        self.pending = self.pending.add(amount)?;
        Ok(())
    }

    /// Only a frame proven never passed to publication can be cancelled locally.
    #[cfg(test)]
    pub fn cancel_pending(&mut self, amount: Amount) -> Result<(), RuntimeError> {
        self.pending = self.pending.sub(amount)?;
        Ok(())
    }

    pub fn commit(&mut self, amount: Amount) -> Result<(), RuntimeError> {
        let pending = self.pending.sub(amount)?;
        let sent = self.sent.add(amount)?;
        if !self.allowed.contains(sent.add(pending)?) {
            return Err(RuntimeError::Protocol);
        }
        if amount.records != 1 {
            return Err(RuntimeError::Protocol);
        }
        self.pending = pending;
        self.sent = sent;
        self.ends.push_back(sent);
        Ok(())
    }

    pub fn freeze(&mut self, next_epoch: u64) -> Result<(), RuntimeError> {
        if self.epoch.checked_add(1) != Some(next_epoch)
            || self.freezing.is_some_and(|old| old != next_epoch)
        {
            return Err(RuntimeError::Protocol);
        }
        self.freezing = Some(next_epoch);
        Ok(())
    }

    pub fn frozen_prefix(&self) -> Option<Amount> {
        (self.freezing.is_some() && self.pending == Amount::default()).then_some(self.sent)
    }

    pub fn outstanding(&self) -> Amount {
        self.sent
            .sub(self.applied)
            .expect("validated delivery receipt")
    }
}

#[derive(Debug, Default)]
pub(super) struct Recipient {
    pub epoch: u64,
    pub allowed: Amount,
    pub applied: Amount,
    pub freezing: Option<u64>,
    pub frozen: Option<Amount>,
}

impl Recipient {
    pub fn snapshot(&self) -> Limit {
        Limit {
            epoch: self.epoch,
            allowed: self.allowed,
            applied: self.applied,
        }
    }

    pub fn obligation(&self) -> Amount {
        self.allowed
            .sub(self.applied)
            .expect("conserved delivery grant")
    }

    /// The caller reserves this increase in the shared bytes/record pool first.
    pub fn extend(&mut self, amount: Amount) -> Result<(), RuntimeError> {
        if self.freezing.is_some() {
            return Err(RuntimeError::Protocol);
        }
        self.allowed = self.allowed.add(amount)?;
        Ok(())
    }

    pub fn apply(&mut self, amount: Amount) -> Result<(), RuntimeError> {
        let applied = self.applied.add(amount)?;
        if !self.allowed.contains(applied)
            || self.frozen.is_some_and(|prefix| !prefix.contains(applied))
        {
            return Err(RuntimeError::Protocol);
        }
        self.applied = applied;
        Ok(())
    }

    pub fn freeze(&mut self) -> Result<u64, RuntimeError> {
        let next = self
            .epoch
            .checked_add(1)
            .ok_or(RuntimeError::IdentityExhausted)?;
        if self.freezing.is_some_and(|old| old != next) {
            return Err(RuntimeError::Protocol);
        }
        self.freezing = Some(next);
        Ok(next)
    }

    /// CONTROL may overtake DATA; this alone never releases the unused grant.
    pub fn observe_frozen(&mut self, epoch: u64, prefix: Amount) -> Result<(), RuntimeError> {
        if self.freezing != Some(epoch)
            || !self.allowed.contains(prefix)
            || !prefix.contains(self.applied)
            || self.frozen.is_some_and(|old| old != prefix)
        {
            return Err(RuntimeError::Protocol);
        }
        self.frozen = Some(prefix);
        Ok(())
    }

    pub fn reclaim(&mut self) -> Result<Option<Amount>, RuntimeError> {
        if self.frozen != Some(self.applied) {
            return Ok(None);
        }
        let epoch = self.freezing.ok_or(RuntimeError::Protocol)?;
        let released = self.obligation();
        self.allowed = self.applied;
        self.epoch = epoch;
        self.frozen = None;
        self.freezing = None;
        Ok(Some(released))
    }
}

#[cfg(test)]
#[path = "../tests/unit/runtime/delivery_credit.rs"]
mod tests;
