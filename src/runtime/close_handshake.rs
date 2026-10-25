//! A terminal bidirectional prefix fence. No timeout releases unknown DATA.
use super::{RuntimeError, protocol::Control};
use crate::peer_credit::Amount;
use std::time::Instant;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) struct Prefix {
    pub sequence: u64,
    pub amount: Amount,
}
impl Prefix {
    pub fn from_control(c: &Control) -> Result<Self, RuntimeError> {
        if c.records != c.watermark {
            return Err(RuntimeError::Protocol);
        }
        Ok(Self {
            sequence: c.watermark,
            amount: Amount {
                bytes: c.applied_bytes,
                records: c.records,
            },
        })
    }
    pub fn reached(self, sequence: u64, applied: Amount) -> Result<bool, RuntimeError> {
        if sequence > self.sequence || !self.amount.contains(applied) {
            return Err(RuntimeError::Protocol);
        }
        if sequence == self.sequence {
            if applied != self.amount {
                return Err(RuntimeError::Protocol);
            }
            return Ok(true);
        }
        Ok(false)
    }
}

#[derive(Clone, Copy, Debug)]
enum Phase {
    Initiator {
        sent: bool,
        remote: Option<Prefix>,
        done: bool,
    },
    Responder {
        remote: Prefix,
        acknowledged: bool,
        done: bool,
    },
}
#[derive(Clone, Copy, Debug)]
pub(super) struct Closing {
    pub nonce: u128,
    pub local: Prefix,
    pub deadline: Instant,
    phase: Phase,
}
impl Closing {
    pub fn initiate(nonce: u128, local: Prefix, deadline: Instant) -> Self {
        Self {
            nonce,
            local,
            deadline,
            phase: Phase::Initiator {
                sent: false,
                remote: None,
                done: false,
            },
        }
    }
    pub fn respond(nonce: u128, local: Prefix, remote: Prefix, deadline: Instant) -> Self {
        Self {
            nonce,
            local,
            deadline,
            phase: Phase::Responder {
                remote,
                acknowledged: false,
                done: false,
            },
        }
    }
    pub fn initiating(&self) -> bool {
        matches!(self.phase, Phase::Initiator { .. })
    }
    pub fn remote(&self) -> Option<Prefix> {
        match self.phase {
            Phase::Initiator { remote, .. } => remote,
            Phase::Responder { remote, .. } => Some(remote),
        }
    }
    /// ACK proves our committed prefix was applied by the remote Core. Its
    /// reverse prefix is retained until locally applied before publishing DONE.
    pub fn acknowledged(
        &mut self,
        nonce: u128,
        remote_prefix: Prefix,
    ) -> Result<Amount, RuntimeError> {
        if nonce != self.nonce {
            return Err(RuntimeError::Protocol);
        }
        let Phase::Initiator {
            sent: true, remote, ..
        } = &mut self.phase
        else {
            return Err(RuntimeError::Protocol);
        };
        if remote.is_some_and(|old| old != remote_prefix) {
            return Err(RuntimeError::Protocol);
        }
        *remote = Some(remote_prefix);
        Ok(self.local.amount)
    }
    /// Responder releases its own outgoing debt only after final DONE.
    pub fn done(&mut self, nonce: u128) -> Result<Amount, RuntimeError> {
        if nonce != self.nonce {
            return Err(RuntimeError::Protocol);
        }
        let Phase::Responder {
            acknowledged: true,
            done,
            ..
        } = &mut self.phase
        else {
            return Err(RuntimeError::Protocol);
        };
        *done = true;
        Ok(self.local.amount)
    }
    pub fn complete(&self) -> bool {
        matches!(
            self.phase,
            Phase::Initiator { done: true, .. } | Phase::Responder { done: true, .. }
        )
    }
    /// At most one CLOSE, ACK and DONE; no automatic terminal retransmission.
    pub fn output(
        &mut self,
        sequence: u64,
        applied: Amount,
        owner_drained: bool,
    ) -> Result<Option<u8>, RuntimeError> {
        match &mut self.phase {
            Phase::Initiator { sent, .. } if !*sent => {
                *sent = true;
                Ok(Some(12))
            }
            Phase::Initiator {
                remote: Some(remote),
                done,
                ..
            } if !*done && owner_drained => {
                if remote.reached(sequence, applied)? {
                    *done = true;
                    Ok(Some(14))
                } else {
                    Ok(None)
                }
            }
            Phase::Responder {
                remote,
                acknowledged,
                ..
            } if !*acknowledged && owner_drained => {
                if remote.reached(sequence, applied)? {
                    *acknowledged = true;
                    Ok(Some(13))
                } else {
                    Ok(None)
                }
            }
            _ => Ok(None),
        }
    }
}

#[cfg(test)]
#[path = "../tests/unit/runtime/close_handshake.rs"]
mod tests;
