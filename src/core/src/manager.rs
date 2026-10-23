//! Multi-peer ownership, admission and active-work scheduling without I/O.
use crate::credit::{Amount, ReceiveCredit, SendCredit};
use crate::{
    CloseReason, Error, Event, Frame, MAX_BATCH_EVENTS, PeerLimits, SendOutcome, Snapshot, State,
    Stream,
};
use std::collections::{BTreeMap, BTreeSet, LinkedList, VecDeque};

pub use skvoz_core_contracts::{PeerId, StreamKey};

#[cfg(test)]
#[path = "manager_tests.rs"]
mod credit_integration_tests;
#[derive(Debug)]
pub struct ManagedEvent {
    pub key: StreamKey,
    pub event: Event,
}
#[derive(Debug)]
pub struct RoutedFrame {
    pub key: StreamKey,
    pub frame: Frame,
}

pub use skvoz_core_contracts::{Aggregate, ManagerConfig, ManagerError, Resources};
const SCHEDULER_QUANTUM: usize = 16 * 1024;
// Fixed framing, parsing and allocation work exists even for a one-byte DATA.
const SCHEDULER_RECORD_BYTES: usize = 1024;

pub use skvoz_core_contracts::FlowObservation;

struct Entry {
    deficit: usize,
    backlog_since: Option<u64>,
    announced: bool,
    stream: Stream,
    receive_ends: LinkedList<u64>,
    received: u64,
    consumed: u64,
    consumed_records: u64,
    growth_consumed: u64,
    growth_records: u64,
    growth_started: u64,
    last_consumption: u64,
    pending_records: usize,
    retained_send_records: usize,
    deadline: Option<u64>,
}
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct CreditRequest {
    requester: u64,
    blocked: u8,
    consumed: Amount,
}
#[derive(Clone, Copy)]
struct Pressure {
    requester: u64,
    blocked: u8,
    deadline: u64,
}
struct Peer {
    deficit: usize,
    tx: SendCredit,
    rx: ReceiveCredit,
    target: Amount,
    reserved_receive: Amount,
    requested: Amount,
    growth_consumed: u64,
    probe: u64,
    controls: VecDeque<Frame>,
    freeze_deadline: Option<u64>,
    last_progress: u64,
    last_request: Option<CreditRequest>,
    pressure: Option<Pressure>,
    retained_send_records: usize,
    output_enabled: bool,
    tx_probe: Option<u64>,
    rtt_ms: u64,
    ack_ms: u64,
    ack_bytes: u64,
    consumed_bytes_per_ms: u64,
    local_bit: u64,
    local_sequence: u64,
    remote_sequence: u64,
    count: usize,
    pending_send: usize,
    output: VecDeque<u64>,
    output_set: BTreeSet<u64>,
    events: VecDeque<u64>,
    event_set: BTreeSet<u64>,
    rejection: Option<RoutedFrame>,
    reject_next: bool,
    failed: bool,
}
/// A single owner drives this value. Returned buffers transfer to its caller.
pub struct Manager {
    config: ManagerConfig,
    peers: BTreeMap<PeerId, Peer>,
    streams: BTreeMap<StreamKey, Entry>,
    output: VecDeque<PeerId>,
    output_set: BTreeSet<PeerId>,
    events: VecDeque<PeerId>,
    event_set: BTreeSet<PeerId>,
    deadlines: BTreeSet<(u64, StreamKey)>,
    pressure_deadlines: BTreeSet<(u64, PeerId)>,
    unconsumed: BTreeSet<StreamKey>,
    budget_waiters: BTreeSet<StreamKey>,
    pending_send: usize,
    pending_records: usize,
    receive_promises: Amount,
    retained_send_records: usize,
    minimum_send_records: usize,
    failed_peers: BTreeSet<PeerId>,
    credit_cursor: Option<PeerId>,
    wake_cursor: Option<StreamKey>,
    now: u64,
    failed: bool,
}
impl Manager {
    pub fn new(config: ManagerConfig) -> Result<Self, ManagerError> {
        config.stream.validate()?;
        if config.max_peers == 0
            || config.max_streams == 0
            || config.max_streams_per_peer == 0
            || config.receive_budget == 0
            || config.receive_budget_per_peer == 0
            || config.send_budget == 0
            || config.send_budget_per_peer == 0
            || config.metadata_backing().is_none()
        {
            return Err(ManagerError::InvalidConfig);
        }
        Ok(Self {
            config,
            peers: BTreeMap::new(),
            streams: BTreeMap::new(),
            output: VecDeque::new(),
            output_set: BTreeSet::new(),
            events: VecDeque::new(),
            event_set: BTreeSet::new(),
            deadlines: BTreeSet::new(),
            pressure_deadlines: BTreeSet::new(),
            unconsumed: BTreeSet::new(),
            budget_waiters: BTreeSet::new(),
            pending_send: 0,
            pending_records: 0,
            receive_promises: Amount::default(),
            retained_send_records: 0,
            minimum_send_records: 0,
            failed_peers: BTreeSet::new(),
            credit_cursor: None,
            wake_cursor: None,
            now: 0,
            failed: false,
        })
    }
    pub fn config(&self) -> ManagerConfig {
        self.config
    }
    /// The host must bind this registration to an authenticated peer session.
    pub fn register_peer(
        &mut self,
        id: PeerId,
        local_origin_bit: bool,
    ) -> Result<(), ManagerError> {
        if self.failed {
            return Err(ManagerError::TransportLost);
        }
        if self.peers.contains_key(&id) || self.peers.len() >= self.config.max_peers {
            return Err(ManagerError::Admission);
        }
        let target = self.minimum_credit();
        let total = self.receive_promises.add(target).map_err(Error::Protocol)?;
        if total.bytes > self.config.receive_budget as u64
            || total.records > self.config.receive_record_budget() as u64
        {
            return Err(ManagerError::Admission);
        }
        let mut rx = ReceiveCredit::default();
        rx.grant(target).map_err(Error::Protocol)?;
        let initial = rx.frame(0);
        self.receive_promises = total;
        self.minimum_send_records += 64;
        self.peers.insert(
            id,
            Peer {
                deficit: SCHEDULER_QUANTUM,
                tx: SendCredit::default(),
                rx,
                target,
                reserved_receive: target,
                requested: target,
                growth_consumed: 0,
                probe: 0,
                controls: VecDeque::from([initial]),
                freeze_deadline: None,
                last_progress: self.now,
                last_request: None,
                pressure: None,
                retained_send_records: 0,
                output_enabled: true,
                tx_probe: None,
                rtt_ms: 0,
                ack_ms: self.now,
                ack_bytes: 0,
                consumed_bytes_per_ms: 0,
                local_bit: u64::from(local_origin_bit),
                local_sequence: 0,
                remote_sequence: 0,
                count: 0,
                pending_send: 0,
                output: VecDeque::new(),
                output_set: BTreeSet::new(),
                events: VecDeque::new(),
                event_set: BTreeSet::new(),
                rejection: None,
                reject_next: true,
                failed: false,
            },
        );
        self.ready_peer(id, true);
        Ok(())
    }
    /// Retain output until the host has proved the authenticated peer lane ready.
    pub fn set_peer_output_enabled(
        &mut self,
        id: PeerId,
        enabled: bool,
    ) -> Result<(), ManagerError> {
        let p = self.peers.get_mut(&id).ok_or(ManagerError::UnknownPeer)?;
        p.output_enabled = enabled;
        if enabled {
            if !p.output.is_empty()
                || !p.controls.is_empty()
                || p.rejection.is_some()
                || (p.tx.freezing.is_some() && !p.tx.frozen && p.tx.pending == Amount::default())
            {
                self.ready_peer(id, true);
            }
        } else {
            self.output.retain(|peer| *peer != id);
            self.output_set.remove(&id);
        }
        Ok(())
    }
    pub fn remove_peer(&mut self, id: PeerId) -> Result<(), ManagerError> {
        let peer = self.peers.get(&id).ok_or(ManagerError::UnknownPeer)?;
        if peer.count != 0 || peer.rejection.is_some() || !peer.failed {
            return Err(ManagerError::PeerBusy);
        }
        self.peers.remove(&id);
        self.failed_peers.remove(&id);
        self.output.retain(|p| *p != id);
        self.output_set.remove(&id);
        self.events.retain(|p| *p != id);
        self.event_set.remove(&id);
        Ok(())
    }
    fn minimum_credit(&self) -> Amount {
        Amount {
            bytes: (self.config.stream.receive_window as usize)
                .min(65536)
                .min((self.config.receive_budget / self.config.max_peers / 8).max(1))
                .min(self.config.receive_budget_per_peer) as u64,
            records: 64.min(self.config.receive_records_per_peer()) as u64,
        }
    }
    fn queue_control(&mut self, peer: PeerId, frame: Frame) {
        let p = self.peers.get_mut(&peer).unwrap();
        let kind = std::mem::discriminant(&frame);
        if let Some(old) = p
            .controls
            .iter_mut()
            .find(|old| std::mem::discriminant(*old) == kind)
        {
            *old = frame;
        } else {
            assert!(p.controls.len() < 4, "bounded peer control slots");
            p.controls.push_back(frame);
        }
        self.ready_peer(peer, true);
    }
    fn release_received(&mut self, peer: PeerId, amount: Amount) -> Result<(), ManagerError> {
        let minimum = self.minimum_credit();
        let p = self.peers.get_mut(&peer).unwrap();
        let before = p.reserved_receive;
        p.rx.consume(amount).map_err(Error::Protocol)?;
        if amount.bytes != 0 {
            p.last_progress = self.now;
        }
        if !p.failed && p.rx.freezing.is_none() {
            p.rx.grant(p.target).map_err(Error::Protocol)?;
        }
        let promise = p.rx.promise();
        p.reserved_receive = Amount {
            bytes: promise.bytes.max(minimum.bytes),
            records: promise.records.max(minimum.records),
        };
        self.receive_promises = self
            .receive_promises
            .sub(before)
            .and_then(|a| a.add(p.reserved_receive))
            .map_err(Error::Protocol)?;
        if !p.failed && p.rx.freezing.is_none() {
            let frame = p.rx.frame(p.probe);
            self.queue_control(peer, frame);
        }
        Ok(())
    }
    fn request_credit(&mut self, key: StreamKey, blocked: u8) {
        let peer = key.peer;
        let p = self.peers.get_mut(&peer).unwrap();
        let request = CreditRequest {
            requester: key.stream_id,
            blocked,
            consumed: p.tx.consumed,
        };
        if p.tx.freezing.is_some() || p.last_request == Some(request) {
            return;
        }
        p.last_request = Some(request);
        let current = p.tx.allowed.bytes - p.tx.consumed.bytes;
        let bdp = p
            .consumed_bytes_per_ms
            .saturating_mul(p.rtt_ms)
            .saturating_mul(2);
        let bytes = current
            .saturating_mul(2)
            .max(bdp)
            .max(1048576)
            .min(crate::MAX_RECEIVE_WINDOW as u64) as u32;
        let records = (p.tx.allowed.records - p.tx.consumed.records)
            .saturating_mul(2)
            .max(64)
            .min(self.config.receive_records_per_peer() as u64) as u32;
        let probe = self.now.saturating_add(1);
        p.tx_probe = Some(probe);
        self.queue_control(
            peer,
            Frame::PeerRequest {
                bytes: bytes.max(1),
                records: records.max(1),
                probe,
                requester_stream_id: key.stream_id,
                blocked,
            },
        );
    }
    fn pressure_interval(&self, peer: PeerId) -> u64 {
        self.peers[&peer].rtt_ms.saturating_mul(4).max(1000)
    }
    fn set_pressure(
        &mut self,
        peer: PeerId,
        requester: u64,
        mut blocked: u8,
    ) -> Result<(), ManagerError> {
        if blocked == 0 {
            return Ok(());
        }
        let interval = self.pressure_interval(peer);
        let mut deadline = self.now.checked_add(interval).ok_or(Error::InvalidTime)?;
        let p = self.peers.get_mut(&peer).unwrap();
        if let Some(old) = p.pressure.take() {
            deadline = deadline.min(old.deadline);
            blocked |= old.blocked;
            self.pressure_deadlines.remove(&(old.deadline, peer));
        }
        p.pressure = Some(Pressure {
            requester,
            blocked,
            deadline,
        });
        self.pressure_deadlines.insert((deadline, peer));
        Ok(())
    }
    fn apply_pressure(&mut self, peer: PeerId) -> Result<(), ManagerError> {
        let Some(pressure) = self.peers[&peer].pressure else {
            return Ok(());
        };
        let requester = StreamKey {
            peer,
            stream_id: pressure.requester,
        };
        let p = &self.peers[&peer];
        let available = p.rx.allowed.sub(p.rx.received).map_err(Error::Protocol)?;
        let byte_deficit = pressure.blocked & 1 != 0 && available.bytes == 0;
        let record_deficit = pressure.blocked & 2 != 0 && available.records == 0;
        if !self
            .streams
            .get(&requester)
            .is_some_and(|e| matches!(e.stream.state(), State::Open | State::HalfClosedLocal))
            || (!byte_deficit && !record_deficit)
            || p.failed
            || p.rx.freezing.is_some()
        {
            self.peers.get_mut(&peer).unwrap().pressure = None;
            return Ok(());
        }
        let interval = self.pressure_interval(peer);
        // This index contains only actually received, unconsumed DATA. Idle
        // streams are never scanned on the normal drive/consumption hot path.
        let victim = self
            .unconsumed
            .range(
                StreamKey { peer, stream_id: 0 }..=StreamKey {
                    peer,
                    stream_id: u64::MAX,
                },
            )
            .filter(|key| **key != requester)
            .filter_map(|key| {
                let e = &self.streams[key];
                (self.now.saturating_sub(e.last_consumption) >= interval).then_some((
                    if record_deficit {
                        e.receive_ends.len() as u64
                    } else {
                        e.received - e.consumed
                    },
                    if byte_deficit {
                        e.received - e.consumed
                    } else {
                        e.receive_ends.len() as u64
                    },
                    *key,
                ))
            })
            .max()
            .map(|(_, _, key)| key);
        if let Some(key) = victim {
            self.close(key, CloseReason::ResourcePressure)?;
            self.peers.get_mut(&peer).unwrap().pressure = None;
        } else {
            self.peers.get_mut(&peer).unwrap().pressure = None;
            self.set_pressure(peer, pressure.requester, pressure.blocked)?;
        }
        Ok(())
    }
    fn receive_control(&mut self, peer: PeerId, frame: &Frame) -> Result<(), ManagerError> {
        match *frame {
            Frame::PeerGrant {
                epoch,
                consumed_bytes,
                limit_bytes,
                consumed_records,
                limit_records,
                probe,
            } => {
                let p = self.peers.get_mut(&peer).unwrap();
                if limit_bytes
                    .checked_sub(consumed_bytes)
                    .is_none_or(|n| n > crate::MAX_RECEIVE_WINDOW as u64)
                    || limit_records
                        .checked_sub(consumed_records)
                        .is_none_or(|n| n > 65536)
                    || (probe != 0 && probe - 1 > self.now)
                {
                    return Err(Error::Protocol(crate::ProtocolError::InvalidCredit).into());
                }
                let previous_epoch = p.tx.epoch;
                let previous_allowed = p.tx.allowed;
                p.tx.grant(
                    epoch,
                    Amount {
                        bytes: consumed_bytes,
                        records: consumed_records,
                    },
                    Amount {
                        bytes: limit_bytes,
                        records: limit_records,
                    },
                )
                .map_err(Error::Protocol)?;
                if p.tx.epoch != previous_epoch || p.tx.allowed != previous_allowed {
                    p.last_request = None;
                }
                if p.tx_probe == Some(probe) {
                    let sample = self.now.saturating_sub(probe.saturating_sub(1)).max(1);
                    p.rtt_ms = if p.rtt_ms == 0 {
                        sample
                    } else {
                        (p.rtt_ms.saturating_mul(3) + sample) / 4
                    };
                    p.tx_probe = None;
                }
                if self.now > p.ack_ms && consumed_bytes > p.ack_bytes {
                    let sample = (consumed_bytes - p.ack_bytes) / (self.now - p.ack_ms);
                    p.consumed_bytes_per_ms = if p.consumed_bytes_per_ms == 0 {
                        sample
                    } else {
                        (p.consumed_bytes_per_ms.saturating_mul(3) + sample) / 4
                    };
                    p.ack_ms = self.now;
                    p.ack_bytes = consumed_bytes;
                }
                self.wake_budget_waiters();
            }
            Frame::PeerRequest {
                bytes,
                records,
                probe,
                requester_stream_id,
                blocked,
            } => {
                if bytes == 0
                    || records == 0
                    || bytes > crate::MAX_RECEIVE_WINDOW
                    || requester_stream_id < 2
                    || blocked > 3
                {
                    return Err(Error::Protocol(crate::ProtocolError::InvalidCredit).into());
                }
                let key = StreamKey {
                    peer,
                    stream_id: requester_stream_id,
                };
                if self
                    .streams
                    .get(&key)
                    .is_none_or(|e| e.stream.state() == State::Closed)
                {
                    let p = &self.peers[&peer];
                    let sequence = if requester_stream_id & 1 == p.local_bit {
                        p.local_sequence
                    } else {
                        p.remote_sequence
                    };
                    if requester_stream_id >> 1 <= sequence {
                        return Ok(());
                    }
                    return Err(ManagerError::InvalidOrigin);
                }
                let p = self.peers.get_mut(&peer).unwrap();
                p.requested = Amount {
                    bytes: (bytes as u64).min(self.config.receive_budget_per_peer as u64),
                    records: (records as u64).min(self.config.receive_records_per_peer() as u64),
                };
                p.probe = probe;
                self.set_pressure(peer, requester_stream_id, blocked)?;
                self.grow_credit();
                let p = &self.peers[&peer];
                if p.rx.freezing.is_none() {
                    self.queue_control(peer, p.rx.frame(probe));
                }
            }
            Frame::PeerFreeze { epoch } => {
                self.peers
                    .get_mut(&peer)
                    .unwrap()
                    .tx
                    .freeze(epoch)
                    .map_err(Error::Protocol)?;
                self.ready_peer(peer, true);
            }
            Frame::PeerFrozen {
                epoch,
                bytes,
                records,
            } => {
                let minimum = self.minimum_credit();
                let fair = self.fair_target();
                let p = self.peers.get_mut(&peer).unwrap();
                if epoch <= p.rx.epoch {
                    return Ok(());
                }
                let before = p.reserved_receive;
                let outstanding = p.rx.outstanding();
                let spare = before.sub(outstanding).map_err(Error::Protocol)?;
                // Reclaim only excess from an actively consuming donor. Keeping
                // its contention share avoids repeated freeze/regrant cycles;
                // freely available backing has no such per-peer ceiling.
                let retained = if p.requested.bytes > minimum.bytes
                    && self.now.saturating_sub(p.last_progress)
                        <= self.config.stream.open_timeout_ms.min(1000)
                {
                    Amount {
                        bytes: fair.bytes.min(p.requested.bytes),
                        records: fair.records.min(p.requested.records),
                    }
                } else {
                    minimum
                };
                let headroom = Amount {
                    bytes: retained
                        .bytes
                        .saturating_sub(outstanding.bytes)
                        .min(spare.bytes),
                    records: retained
                        .records
                        .saturating_sub(outstanding.records)
                        .min(spare.records),
                };
                p.rx.frozen(epoch, Amount { bytes, records }, headroom)
                    .map_err(Error::Protocol)?;
                p.target = p.rx.promise();
                p.reserved_receive = p.target;
                p.growth_consumed = p.rx.consumed.bytes;
                p.freeze_deadline = None;
                self.receive_promises = self
                    .receive_promises
                    .sub(before)
                    .and_then(|a| a.add(p.target))
                    .map_err(Error::Protocol)?;
                let frame = p.rx.frame(p.probe);
                self.queue_control(peer, frame);
                self.grow_credit_with_reclaimed_pool(true);
            }
            _ => return Err(Error::Protocol(crate::ProtocolError::UnexpectedFrame).into()),
        }
        Ok(())
    }
    fn fair_target(&self) -> Amount {
        let minimum = self.minimum_credit();
        let active = self
            .peers
            .values()
            .filter(|p| {
                !p.failed
                    && p.requested.bytes > minimum.bytes
                    && self.now.saturating_sub(p.last_progress)
                        <= self.config.stream.open_timeout_ms.min(1000)
            })
            .count()
            .max(1);
        let idle = self.config.max_peers.saturating_sub(active);
        Amount {
            bytes: (self.config.receive_budget as u64).saturating_sub(minimum.bytes * idle as u64)
                / active as u64,
            records: (self.config.receive_record_budget() as u64)
                .saturating_sub(minimum.records * idle as u64)
                / active as u64,
        }
    }
    fn grow_credit(&mut self) {
        self.grow_credit_with_reclaimed_pool(false);
    }
    fn grow_credit_with_reclaimed_pool(&mut self, reclaimed: bool) {
        let minimum = self.minimum_credit();
        let mut peers: Vec<_> = self.peers.keys().copied().collect();
        if let Some(cursor) = self.credit_cursor {
            peers.sort_by_key(|id| (*id <= cursor, *id));
        }
        let eligible = |p: &Peer| {
            !p.failed && p.rx.freezing.is_none() && p.rx.consumed.bytes != p.growth_consumed
        };
        let mut byte_requests = self
            .peers
            .values()
            .filter(|p| eligible(p) && p.requested.bytes > p.target.bytes)
            .count();
        let mut record_requests = self
            .peers
            .values()
            .filter(|p| eligible(p) && p.requested.records > p.target.records)
            .count();
        for peer in peers {
            let p = &self.peers[&peer];
            if p.failed
                || p.rx.freezing.is_some()
                || p.rx.consumed.bytes == p.growth_consumed
                || (p.requested.bytes <= p.target.bytes && p.requested.records <= p.target.records)
            {
                continue;
            }
            let unregistered = self.config.max_peers - self.peers.len();
            let byte_free = (self.config.receive_budget as u64 - self.receive_promises.bytes)
                .saturating_sub(minimum.bytes * unregistered as u64);
            let record_free = (self.config.receive_record_budget() as u64
                - self.receive_promises.records)
                .saturating_sub(minimum.records * unregistered as u64);
            // Only competing unmet requests share newly free backing. Fulfilled
            // low-demand peers impose no window ceiling on a bulk consumer.
            let byte_share = byte_free / byte_requests.max(1) as u64;
            let record_share = record_free / record_requests.max(1) as u64;
            byte_requests =
                byte_requests.saturating_sub(usize::from(p.requested.bytes > p.target.bytes));
            record_requests =
                record_requests.saturating_sub(usize::from(p.requested.records > p.target.records));
            let bytes = p
                .requested
                .bytes
                .min(if reclaimed {
                    u64::MAX
                } else {
                    p.target.bytes.saturating_mul(4)
                })
                .saturating_sub(p.target.bytes)
                .min(byte_share);
            let records = p
                .requested
                .records
                .min(if reclaimed {
                    u64::MAX
                } else {
                    p.target.records.saturating_mul(4)
                })
                .saturating_sub(p.target.records)
                .min(record_share);
            if bytes == 0 && records == 0 {
                self.reclaim_credit(peer);
                continue;
            }
            let p = self.peers.get_mut(&peer).unwrap();
            let before = p.reserved_receive;
            p.target = p
                .target
                .add(Amount { bytes, records })
                .expect("bounded growth");
            if p.rx.grant(p.target).is_err() {
                self.peer_lost(peer);
                continue;
            }
            p.growth_consumed = p.rx.consumed.bytes;
            p.reserved_receive = p.rx.promise();
            self.receive_promises = self
                .receive_promises
                .sub(before)
                .and_then(|a| a.add(p.reserved_receive))
                .expect("aggregate growth conservation");
            let frame = p.rx.frame(p.probe);
            self.queue_control(peer, frame);
            self.credit_cursor = Some(peer);
        }
    }
    fn reclaim_credit(&mut self, requester: PeerId) {
        let minimum = self.minimum_credit();
        let fair = self.fair_target();
        if self.peers.values().any(|p| p.rx.freezing.is_some()) {
            return;
        }
        let byte_deficit =
            self.peers[&requester].requested.bytes > self.peers[&requester].target.bytes;
        let record_deficit =
            self.peers[&requester].requested.records > self.peers[&requester].target.records;
        let candidate = self
            .peers
            .iter()
            .filter(|(id, p)| **id != requester && !p.failed && p.rx.freezing.is_none())
            .filter(|(_, p)| {
                let idle = self.now.saturating_sub(p.last_progress)
                    > self.config.stream.open_timeout_ms.min(1000);
                (byte_deficit
                    && (p.target.bytes > fair.bytes || idle)
                    && p.rx
                        .promise()
                        .bytes
                        .saturating_sub(p.rx.outstanding().bytes)
                        > minimum.bytes)
                    || (record_deficit
                        && (p.target.records > fair.records || idle)
                        && p.rx
                            .promise()
                            .records
                            .saturating_sub(p.rx.outstanding().records)
                            > minimum.records)
            })
            .min_by_key(|(_, p)| p.last_progress)
            .map(|(id, _)| *id);
        if let Some(peer) = candidate {
            let p = self.peers.get_mut(&peer).unwrap();
            let frame = match p.rx.freeze() {
                Ok(frame) => frame,
                Err(_) => {
                    self.peer_lost(peer);
                    return;
                }
            };
            p.freeze_deadline = self.now.checked_add(self.config.stream.open_timeout_ms);
            if p.freeze_deadline.is_none() {
                self.peer_lost(peer);
                return;
            }
            self.queue_control(peer, frame);
        }
    }
    fn available_admission(&self, id: PeerId) -> Result<(), ManagerError> {
        if self.failed {
            return Err(ManagerError::TransportLost);
        }
        let p = self.peers.get(&id).ok_or(ManagerError::UnknownPeer)?;
        if p.failed {
            return Err(ManagerError::TransportLost);
        }
        if self.streams.len() >= self.config.max_streams
            || p.count >= self.config.max_streams_per_peer
        {
            return Err(ManagerError::Admission);
        }
        Ok(())
    }
    pub fn open(
        &mut self,
        peer: PeerId,
        metadata: &[u8],
        now: u64,
    ) -> Result<StreamKey, ManagerError> {
        self.tick(now)?;
        self.available_admission(peer)?;
        let p = self.peers.get(&peer).unwrap();
        let seq = p
            .local_sequence
            .checked_add(1)
            .ok_or(ManagerError::IdentityExhausted)?;
        let id = seq
            .checked_mul(2)
            .and_then(|n| n.checked_add(p.local_bit))
            .ok_or(ManagerError::IdentityExhausted)?;
        let mut stream = Stream::new(self.config.stream)?;
        stream.initial_receive_window(self.config.stream.receive_window.min(65536));
        stream.open(metadata, now)?;
        let key = StreamKey {
            peer,
            stream_id: id,
        };
        self.insert(key, stream, now)?;
        self.peers.get_mut(&peer).unwrap().local_sequence = seq;
        Ok(key)
    }
    fn insert(&mut self, key: StreamKey, stream: Stream, now: u64) -> Result<(), ManagerError> {
        let deadline = now
            .checked_add(self.config.stream.open_timeout_ms)
            .ok_or(Error::InvalidTime)?;
        self.deadlines.insert((deadline, key));
        self.streams.insert(
            key,
            Entry {
                deficit: SCHEDULER_QUANTUM,
                backlog_since: None,
                announced: !stream.has_pending_open(),
                stream,
                receive_ends: LinkedList::new(),
                received: 0,
                consumed: 0,
                consumed_records: 0,
                growth_consumed: 0,
                growth_records: 0,
                growth_started: now,
                last_consumption: now,
                pending_records: 0,
                retained_send_records: 0,
                deadline: Some(deadline),
            },
        );
        self.peers.get_mut(&key.peer).unwrap().count += 1;
        self.refresh(key, 0);
        Ok(())
    }
    /// Unknown non-OPEN packets are ignored. Excess OPEN receives at most one
    /// pending REJECT per peer; additional overload requests time out remotely.
    pub fn receive(&mut self, key: StreamKey, frame: &Frame, now: u64) -> Result<(), ManagerError> {
        self.tick(now)?;
        if self.failed {
            return Err(ManagerError::TransportLost);
        }
        let p = self.peers.get(&key.peer).ok_or(ManagerError::UnknownPeer)?;
        if p.failed {
            return Err(ManagerError::TransportLost);
        }
        let local_bit = p.local_bit;
        let remote_sequence = p.remote_sequence;
        if frame.is_peer_control() {
            if key.stream_id != 0 {
                return Err(Error::Protocol(crate::ProtocolError::UnexpectedFrame).into());
            }
            return self.receive_control(key.peer, frame);
        }
        if key.stream_id == 0 {
            return Err(ManagerError::InvalidOrigin);
        }
        if let Frame::Data { bytes, .. } = frame {
            if bytes.is_empty() || bytes.len() > self.config.stream.max_frame as usize {
                return Err(Error::Protocol(crate::ProtocolError::InvalidDataSize).into());
            }
            self.peers
                .get_mut(&key.peer)
                .unwrap()
                .rx
                .receive(Amount::data(bytes.len()))
                .map_err(Error::Protocol)?;
            if !self.streams.contains_key(&key) {
                self.release_received(key.peer, Amount::data(bytes.len()))?;
                return Ok(());
            }
        }
        if !self.streams.contains_key(&key) {
            if !matches!(frame, Frame::Open { .. }) {
                return Ok(());
            }
            let seq = key.stream_id >> 1;
            if seq == 0 || key.stream_id & 1 == local_bit {
                return Err(ManagerError::InvalidOrigin);
            }
            if seq <= remote_sequence {
                return Ok(());
            }
            self.peers.get_mut(&key.peer).unwrap().remote_sequence = seq;
            if self.available_admission(key.peer).is_err() {
                let p = self.peers.get_mut(&key.peer).unwrap();
                if p.rejection.is_none() {
                    p.rejection = Some(RoutedFrame {
                        key,
                        frame: Frame::Reject {
                            reason: Box::new([]),
                        },
                    });
                }
                self.ready_peer(key.peer, true);
                return Ok(());
            }
            let mut stream = Stream::new(self.config.stream)?;
            stream.initial_receive_window(self.config.stream.receive_window.min(65536));
            self.insert(key, stream, now)?;
        }
        let before = self.streams[&key].stream.snapshot().pending_send_bytes;
        let result = self
            .streams
            .get_mut(&key)
            .unwrap()
            .stream
            .receive(frame, now);
        if let Frame::Data { offset, bytes } = frame {
            if result.is_ok() && self.streams[&key].stream.state() != State::Closed {
                let end = offset
                    .checked_add(bytes.len() as u64)
                    .ok_or(Error::OffsetExhausted)?;
                let e = self.streams.get_mut(&key).unwrap();
                if e.received == e.consumed {
                    e.last_consumption = now;
                    if e.growth_consumed == e.consumed && e.growth_records == e.consumed_records {
                        e.growth_started = now;
                    }
                }
                e.received = end;
                e.receive_ends.push_back(end);
                self.unconsumed.insert(key);
            } else {
                self.release_received(key.peer, Amount::data(bytes.len()))?;
            }
        }
        self.refresh(key, before);
        if matches!(
            frame,
            Frame::WindowUpdate { .. } | Frame::WindowGrant { .. }
        ) {
            // A partial byte ACK can free the per-stream occupancy gate while
            // retaining its end-offset node and all pending frame counts.
            self.wake_budget_waiters();
        }
        result.map_err(Into::into)
    }
    fn operation(
        &mut self,
        key: StreamKey,
        f: impl FnOnce(&mut Stream) -> Result<(), Error>,
    ) -> Result<(), ManagerError> {
        let e = self
            .streams
            .get_mut(&key)
            .ok_or(ManagerError::UnknownStream)?;
        let before = e.stream.snapshot().pending_send_bytes;
        let result = f(&mut e.stream);
        self.refresh(key, before);
        result.map_err(Into::into)
    }
    pub fn accept(&mut self, key: StreamKey, metadata: &[u8]) -> Result<(), ManagerError> {
        self.operation(key, |s| s.accept(metadata))
    }
    pub fn reject(&mut self, key: StreamKey, reason: &[u8]) -> Result<(), ManagerError> {
        self.operation(key, |s| s.reject(reason))
    }
    pub fn finish(&mut self, key: StreamKey) -> Result<(), ManagerError> {
        self.operation(key, Stream::finish)
    }
    pub fn consume_through(&mut self, key: StreamKey, offset: u64) -> Result<(), ManagerError> {
        let e = self
            .streams
            .get_mut(&key)
            .ok_or(ManagerError::UnknownStream)?;
        let before = e.stream.snapshot().pending_send_bytes;
        e.stream.consume_through(offset)?;
        let bytes = offset.saturating_sub(e.consumed);
        let mut records = 0;
        while e.receive_ends.front().is_some_and(|end| *end <= offset) {
            e.receive_ends.pop_front();
            records += 1;
        }
        if bytes != 0 && e.consumed == e.growth_consumed && e.consumed_records == e.growth_records {
            // Measure this stream's processing, not time waiting for its first
            // native/owner turn. Subsequent slow consumption still bounds growth.
            e.growth_started = self.now;
        }
        e.consumed = e.consumed.max(offset);
        e.consumed_records = e
            .consumed_records
            .checked_add(records)
            .ok_or(Error::OffsetExhausted)?;
        if bytes != 0 {
            e.last_consumption = self.now;
        }
        if e.received == e.consumed {
            self.unconsumed.remove(&key);
        }
        self.release_received(key.peer, Amount { bytes, records })?;
        self.grow_credit();
        if let Some(e) = self.streams.get_mut(&key)
            && matches!(
                e.stream.state(),
                State::Open | State::HalfClosedLocal | State::HalfClosedRemote | State::Draining
            )
        {
            let p = &self.peers[&key.peer];
            let current = e.stream.receive_window();
            let epoch_bytes = e.consumed - e.growth_consumed;
            let epoch_records = e.consumed_records - e.growth_records;
            if epoch_bytes >= (current as u64 / 2).max(1)
                || epoch_records >= (e.stream.receive_record_window() as u64 / 2).max(1)
            {
                // Only this stream's actual consumption can grow its flight.
                // A drained native queue is also proof of consumer progress.
                // Shared owner scheduling can exceed a low transport RTT even
                // when this stream leaves no work waiting for its consumer.
                let fast = e.consumed == e.received
                    || p.rtt_ms == 0
                    || self.now.saturating_sub(e.growth_started) <= p.rtt_ms.saturating_mul(2);
                if fast {
                    let ceiling = p
                        .target
                        .bytes
                        .saturating_sub(p.target.bytes / 8)
                        .min(self.config.stream.receive_window as u64)
                        as u32;
                    let window = current
                        .saturating_mul(2)
                        .max(256 * 1024)
                        .min(ceiling)
                        .max(current);
                    if window > current {
                        e.stream.grant_receive_window(window, p.probe)?;
                    }
                }
                e.growth_consumed = e.consumed;
                e.growth_records = e.consumed_records;
                e.growth_started = self.now;
            }
        }
        self.refresh(key, before);
        Ok(())
    }
    pub fn close(&mut self, key: StreamKey, reason: CloseReason) -> Result<(), ManagerError> {
        self.operation(key, |s| s.close(reason))
    }
    pub fn send(&mut self, key: StreamKey, bytes: &[u8]) -> Result<SendOutcome, ManagerError> {
        if self.failed {
            return Err(ManagerError::TransportLost);
        }
        let p = self.peers.get(&key.peer).ok_or(ManagerError::UnknownPeer)?;
        if p.failed {
            return Err(ManagerError::TransportLost);
        }
        let e = self
            .streams
            .get_mut(&key)
            .ok_or(ManagerError::UnknownStream)?;
        let before = e.stream.snapshot().pending_send_bytes;
        let credit = p.tx.room();
        let flight = p.tx.allowed.sub(p.tx.consumed).map_err(Error::Protocol)?;
        let byte_ceiling = flight.bytes.saturating_sub(flight.bytes / 8).max(1);
        let record_ceiling = flight.records.saturating_sub(flight.records / 8).max(1);
        let own_byte_room = byte_ceiling.saturating_sub(e.stream.send_unacknowledged_bytes());
        let own_records_full = e.stream.retained_send_records() as u64 >= record_ceiling;
        let room = (self.config.send_budget - self.pending_send)
            .min(self.config.send_budget_per_peer - p.pending_send)
            .min(credit.bytes as usize)
            .min(own_byte_room as usize);
        let own_minimum = 64usize.saturating_sub(p.retained_send_records);
        let spare_records = self
            .config
            .send_record_budget()
            .saturating_sub(self.retained_send_records)
            .saturating_sub(self.minimum_send_records - own_minimum)
            .saturating_sub((self.config.max_peers - self.peers.len()).saturating_mul(64));
        let room = if credit.records == 0
            || spare_records == 0
            || p.retained_send_records >= self.config.send_records_per_peer()
            || own_records_full
        {
            0
        } else {
            room
        };
        if room == 0
            && !bytes.is_empty()
            && matches!(e.stream.state(), State::Open | State::HalfClosedRemote)
        {
            e.stream.budget_blocked();
            self.budget_waiters.insert(key);
            if credit.bytes == 0 || credit.records == 0 {
                self.request_credit(
                    key,
                    u8::from(credit.bytes == 0) | (u8::from(credit.records == 0) << 1),
                );
            } else if own_byte_room == 0 || own_records_full {
                self.request_credit(key, 0);
            }
            return Ok(SendOutcome::WouldBlock);
        }
        let result = e.stream.send(&bytes[..bytes.len().min(room)]);
        if let Ok(SendOutcome::Accepted(count)) = result
            && count != 0
        {
            self.peers
                .get_mut(&key.peer)
                .unwrap()
                .tx
                .reserve(Amount::data(count))
                .map_err(Error::Protocol)?;
            self.pending_records += 1;
            self.streams.get_mut(&key).unwrap().pending_records += 1;
        }
        if matches!(result, Ok(SendOutcome::WouldBlock))
            && self.streams[&key].stream.credit_blocked()
        {
            self.request_credit(key, 0);
        }
        self.refresh(key, before);
        result.map_err(Into::into)
    }
    fn ready_peer(&mut self, id: PeerId, output: bool) {
        if output && !self.peers[&id].output_enabled {
            return;
        }
        let (q, set) = if output {
            (&mut self.output, &mut self.output_set)
        } else {
            (&mut self.events, &mut self.event_set)
        };
        if set.insert(id) {
            q.push_back(id);
        }
    }
    fn refresh(&mut self, key: StreamKey, before: usize) {
        let e = self.streams.get_mut(&key).unwrap();
        let snapshot = e.stream.snapshot();
        let retained_before = e.retained_send_records;
        let retained_after = e.stream.retained_send_records();
        e.retained_send_records = retained_after;
        let after = snapshot.pending_send_bytes;
        if after == 0 {
            e.backlog_since = None;
        } else if e.backlog_since.is_none() {
            e.backlog_since = Some(self.now);
        }
        if snapshot.state == State::Closed {
            self.unconsumed.remove(&key);
            let amount = Amount {
                bytes: e.received - e.consumed,
                records: e.receive_ends.len() as u64,
            };
            e.consumed = e.received;
            e.receive_ends.clear();
            if amount != Amount::default() {
                self.release_received(key.peer, amount)
                    .expect("closed receive conservation");
            }
            let cancelled = before - after;
            if cancelled != 0 {
                // Every DATA node has one record; closed stream removed all its nodes.
                let e = self.streams.get_mut(&key).unwrap();
                let records = e.pending_records as u64;
                e.pending_records = 0;
                self.peers
                    .get_mut(&key.peer)
                    .unwrap()
                    .tx
                    .release_pending(Amount {
                        bytes: cancelled as u64,
                        records,
                    })
                    .expect("cancelled send conservation");
                self.pending_records -= records as usize;
            }
        }
        let e = self.streams.get_mut(&key).unwrap();
        let opening = matches!(e.stream.state(), State::Idle | State::Opening(_));
        if !opening && let Some(d) = e.deadline.take() {
            self.deadlines.remove(&(d, key));
        }
        let output = e.stream.has_frames();
        let events = e.stream.has_events();
        let p = self.peers.get_mut(&key.peer).unwrap();
        let previous_minimum = if p.failed {
            0
        } else {
            64usize.saturating_sub(p.retained_send_records)
        };
        p.retained_send_records = p.retained_send_records - retained_before + retained_after;
        self.retained_send_records = self.retained_send_records - retained_before + retained_after;
        let next_minimum = if p.failed {
            0
        } else {
            64usize.saturating_sub(p.retained_send_records)
        };
        self.minimum_send_records = self.minimum_send_records - previous_minimum + next_minimum;
        p.pending_send = p.pending_send - before + after;
        self.pending_send = self.pending_send - before + after;
        if output && p.output_set.insert(key.stream_id) {
            p.output.push_back(key.stream_id);
        }
        if events && p.event_set.insert(key.stream_id) {
            p.events.push_back(key.stream_id);
        }
        if output {
            self.ready_peer(key.peer, true);
        }
        if events {
            self.ready_peer(key.peer, false);
        }
        if after < before || retained_after < retained_before {
            self.wake_budget_waiters();
        }
    }
    fn wake_budget_waiters(&mut self) {
        if self.pending_send >= self.config.send_budget {
            return;
        }
        let mut keys: Vec<_> = match self.wake_cursor {
            Some(cursor) => self
                .budget_waiters
                .range((
                    std::ops::Bound::Excluded(cursor),
                    std::ops::Bound::Unbounded,
                ))
                .take(32)
                .copied()
                .collect(),
            None => Vec::new(),
        };
        if keys.len() < 32 {
            keys.extend(self.budget_waiters.iter().take(32 - keys.len()).copied());
        }
        self.wake_cursor = keys.last().copied();
        for key in keys {
            if self.peers[&key.peer].pending_send >= self.config.send_budget_per_peer {
                continue;
            }
            self.budget_waiters.remove(&key);
            if let Some(e) = self.streams.get_mut(&key) {
                e.stream.budget_writable();
                let p = self.peers.get_mut(&key.peer).unwrap();
                if e.stream.has_events() && p.event_set.insert(key.stream_id) {
                    p.events.push_back(key.stream_id);
                }
                if e.stream.has_events() {
                    self.ready_peer(key.peer, false);
                }
            }
        }
    }
    fn reap(&mut self, key: StreamKey) {
        if self.streams.get(&key).is_some_and(|e| {
            e.stream.state() == State::Closed && !e.stream.has_frames() && !e.stream.has_events()
        }) {
            let e = self.streams.remove(&key).unwrap();
            if let Some(d) = e.deadline {
                self.deadlines.remove(&(d, key));
            }
            let p = self.peers.get_mut(&key.peer).unwrap();
            p.count -= 1;
            p.output.retain(|id| *id != key.stream_id);
            p.output_set.remove(&key.stream_id);
            p.events.retain(|id| *id != key.stream_id);
            p.event_set.remove(&key.stream_id);
            self.budget_waiters.remove(&key);
        }
    }
    /// Byte deficit scheduling between peers, then their ready streams.
    /// Caller transmits in returned order and bounds retained frame batches.
    pub fn poll_frames(&mut self, max: usize) -> Vec<RoutedFrame> {
        self.poll_frames_with_budget(max, |_, _, _| true)
    }
    /// Framed demand already retained in ready output. Does not extract,
    /// clone payloads, dispatch credit or alter stream/peer rotation.
    pub fn transport_demand(&self, peer: PeerId, overhead: usize, maximum: usize) -> usize {
        let Some(p) = self.peers.get(&peer) else {
            return 0;
        };
        let weight = |id, frame: &Frame| {
            crate::wire::encoded_size(id, frame)
                .unwrap_or(maximum)
                .saturating_add(overhead)
        };
        let mut bytes = 0usize;
        for frame in &p.controls {
            bytes = bytes.saturating_add(weight(0, frame));
            if bytes >= maximum {
                return maximum;
            }
        }
        if let Some(frame) = p.tx.peek_frozen_frame() {
            bytes = bytes.saturating_add(weight(0, &frame));
        }
        if let Some(frame) = &p.rejection {
            bytes = bytes.saturating_add(weight(frame.key.stream_id, &frame.frame));
        }
        for &id in &p.output {
            if bytes >= maximum {
                break;
            }
            if let Some(entry) = self.streams.get(&StreamKey {
                peer,
                stream_id: id,
            }) {
                bytes = bytes.saturating_add(entry.stream.transport_demand(
                    id,
                    overhead,
                    maximum - bytes,
                ));
            }
        }
        bytes.min(maximum)
    }
    pub fn next_transport_weight(&self, peer: PeerId, overhead: usize) -> Option<usize> {
        let p = self.peers.get(&peer)?;
        let weight = |id, frame: &Frame| {
            crate::wire::encoded_size(id, frame)
                .ok()?
                .checked_add(overhead)
        };
        if let Some(frame) = p.controls.front() {
            return weight(0, frame);
        }
        if let Some(frame) = p.tx.peek_frozen_frame() {
            return weight(0, &frame);
        }
        let rejected = p
            .rejection
            .as_ref()
            .and_then(|frame| weight(frame.key.stream_id, &frame.frame));
        if p.rejection.is_some() && (p.output.is_empty() || p.reject_next) {
            return rejected;
        }
        p.output
            .iter()
            .filter_map(|&id| {
                let key = StreamKey {
                    peer,
                    stream_id: id,
                };
                if self.open_output_blocked(key) {
                    return None;
                }
                let entry = self.streams.get(&key)?;
                let (size, _) = entry.stream.next_frame_size_for(id)?;
                size.checked_add(overhead)
            })
            .chain(rejected)
            .min()
    }
    #[cfg(feature = "nats")]
    pub(crate) fn stream_transport_weight(&self, key: StreamKey, overhead: usize) -> Option<usize> {
        if self.open_output_blocked(key) {
            return None;
        }
        let stream = &self.streams.get(&key)?.stream;
        if stream.priority_control_ready() {
            return None;
        }
        stream
            .next_frame_size_for(key.stream_id)?
            .0
            .checked_add(overhead)
    }
    fn open_output_blocked(&self, key: StreamKey) -> bool {
        self.streams
            .get(&key)
            .is_some_and(|entry| entry.stream.has_pending_open())
            && self.peers[&key.peer]
                .output_set
                .range(..key.stream_id)
                .any(|&id| {
                    self.streams
                        .get(&StreamKey {
                            peer: key.peer,
                            stream_id: id,
                        })
                        .is_some_and(|entry| entry.stream.has_pending_open())
                })
    }
    /// Extract one bounded flow-control/reset command independently of DATA.
    /// Only ready stream indexes are visited; idle streams are never scanned.
    pub(crate) fn poll_priority_control(&mut self, peer: PeerId) -> Option<RoutedFrame> {
        if !self.peers.get(&peer)?.output_enabled {
            return None;
        }
        let p = self.peers.get_mut(&peer)?;
        if let Some(frame) = p.controls.pop_front().or_else(|| p.tx.frozen_frame()) {
            return Some(RoutedFrame {
                key: StreamKey { peer, stream_id: 0 },
                frame,
            });
        }
        let visits = p.output.len();
        for _ in 0..visits {
            let id = self.peers.get_mut(&peer)?.output.pop_front()?;
            self.peers.get_mut(&peer)?.output_set.remove(&id);
            let key = StreamKey {
                peer,
                stream_id: id,
            };
            let entry = self.streams.get_mut(&key)?;
            let before = entry.stream.snapshot().pending_send_bytes;
            let frame = entry.stream.poll_priority_control();
            let announced = entry.announced;
            self.refresh(key, before);
            self.reap(key);
            if let Some(frame) = frame {
                // Cancellation before OPEN publication has no remote object.
                if !announced && matches!(frame, Frame::Close { .. }) {
                    continue;
                }
                return Some(RoutedFrame { key, frame });
            }
        }
        None
    }
    pub(crate) fn validate_control_object(&self, key: StreamKey) -> Result<(), ManagerError> {
        if key.stream_id == 0 || self.streams.contains_key(&key) {
            return Ok(());
        }
        let p = self.peers.get(&key.peer).ok_or(ManagerError::UnknownPeer)?;
        let last = if key.stream_id & 1 == p.local_bit {
            p.local_sequence
        } else {
            p.remote_sequence
        };
        if key.stream_id >> 1 > last {
            return Err(ManagerError::InvalidOrigin);
        }
        Ok(())
    }
    /// Admit an encoded Core frame before extraction. Denied work stays owned
    /// by Core; ready peers/streams rotate within a finite scan, never idle slots.
    /// The callback accounts cumulative admissions within this batch.
    pub fn poll_frames_with_budget(
        &mut self,
        max: usize,
        mut admit: impl FnMut(StreamKey, usize, bool) -> bool,
    ) -> Vec<RoutedFrame> {
        let mut result = Vec::new();
        let max = max.min(MAX_BATCH_EVENTS);
        let mut attempts = max
            .saturating_add(self.streams.len())
            .saturating_add(self.peers.len());
        while result.len() < max && attempts > 0 {
            attempts -= 1;
            let Some(peer) = self.output.pop_front() else {
                break;
            };
            self.output_set.remove(&peer);
            let p = &self.peers[&peer];
            let describe = |key: StreamKey, frame: &Frame| {
                crate::wire::encoded_size(key.stream_id, frame)
                    .ok()
                    .map(|size| (key, size, matches!(frame, Frame::Data { .. }), false))
            };
            let next = if let Some(frame) = p.controls.front() {
                describe(StreamKey { peer, stream_id: 0 }, frame)
            } else if let Some(frame) = p.tx.peek_frozen_frame() {
                describe(StreamKey { peer, stream_id: 0 }, &frame)
            } else if p.rejection.is_some() && (p.output.is_empty() || p.reject_next) {
                let routed = p.rejection.as_ref().unwrap();
                describe(routed.key, &routed.frame)
            } else {
                p.output.front().and_then(|&id| {
                    let key = StreamKey {
                        peer,
                        stream_id: id,
                    };
                    self.streams
                        .get(&key)?
                        .stream
                        .next_frame_size_for(id)
                        .map(|(size, data)| (key, size, data, true))
                })
            };
            if let Some((key, size, _, stream)) = next {
                let cost = size + SCHEDULER_RECORD_BYTES;
                let p = self.peers.get_mut(&peer).unwrap();
                if cost > p.deficit {
                    p.deficit = p
                        .deficit
                        .saturating_add(SCHEDULER_QUANTUM)
                        .min(4 * SCHEDULER_QUANTUM + SCHEDULER_RECORD_BYTES + 28);
                    self.ready_peer(peer, true);
                    continue;
                }
                if stream {
                    let entry = self.streams.get_mut(&key).unwrap();
                    if cost > entry.deficit {
                        entry.deficit = entry
                            .deficit
                            .saturating_add(SCHEDULER_QUANTUM)
                            .min(4 * SCHEDULER_QUANTUM + SCHEDULER_RECORD_BYTES + 28);
                        let id = p.output.pop_front().unwrap();
                        p.output.push_back(id);
                        self.ready_peer(peer, true);
                        continue;
                    }
                }
            }
            // The receiver's replay watermark requires first OPEN publication
            // in identity order. Keep rotating denied work so smaller DATA,
            // FIN/CLOSE and peer controls can still use available credit.
            if let Some((key, size, data, stream)) = next
                && (self.open_output_blocked(key) || !admit(key, size, data))
            {
                if stream {
                    let p = self.peers.get_mut(&peer).unwrap();
                    p.reject_next = true;
                    let id = p.output.pop_front().unwrap();
                    p.output.push_back(id);
                }
                self.ready_peer(peer, true);
                continue;
            }
            if let Some(frame) = self.peers.get_mut(&peer).unwrap().controls.pop_front() {
                result.push(RoutedFrame {
                    key: StreamKey { peer, stream_id: 0 },
                    frame,
                });
            } else if let Some(frame) = self.peers.get_mut(&peer).unwrap().tx.frozen_frame() {
                result.push(RoutedFrame {
                    key: StreamKey { peer, stream_id: 0 },
                    frame,
                });
            } else if self.peers[&peer].rejection.is_some()
                && (self.peers[&peer].output.is_empty() || self.peers[&peer].reject_next)
            {
                let p = self.peers.get_mut(&peer).unwrap();
                result.push(p.rejection.take().unwrap());
                p.reject_next = false;
            } else {
                let p = self.peers.get_mut(&peer).unwrap();
                p.reject_next = true;
                if let Some(id) = p.output.pop_front() {
                    p.output_set.remove(&id);
                    let key = StreamKey {
                        peer,
                        stream_id: id,
                    };
                    if let Some(e) = self.streams.get_mut(&key) {
                        let before = e.stream.snapshot().pending_send_bytes;
                        if let Some(frame) = e.stream.poll_frames(1).pop() {
                            if matches!(frame, Frame::Open { .. }) {
                                e.announced = true;
                            }
                            if let Frame::Data { bytes, .. } = &frame {
                                self.peers
                                    .get_mut(&peer)
                                    .unwrap()
                                    .tx
                                    .dispatch(Amount::data(bytes.len()))
                                    .expect("dispatch conservation");
                                self.pending_records -= 1;
                                self.streams.get_mut(&key).unwrap().pending_records -= 1;
                            }
                            result.push(RoutedFrame { key, frame });
                        }
                        self.refresh(key, before);
                        self.reap(key);
                    }
                }
            }
            if let Some((key, size, _, stream)) = next {
                let cost = size + SCHEDULER_RECORD_BYTES;
                self.peers.get_mut(&peer).unwrap().deficit -= cost;
                if stream && let Some(entry) = self.streams.get_mut(&key) {
                    entry.deficit -= cost;
                    if entry.deficit != 0 && self.peers[&peer].output_set.contains(&key.stream_id) {
                        let p = self.peers.get_mut(&peer).unwrap();
                        p.output.retain(|id| *id != key.stream_id);
                        p.output.push_front(key.stream_id);
                    } else if entry.deficit == 0 {
                        entry.deficit = SCHEDULER_QUANTUM;
                    }
                }
            }
            let p = &self.peers[&peer];
            if !p.output.is_empty()
                || p.rejection.is_some()
                || !p.controls.is_empty()
                || (p.tx.freezing.is_some() && !p.tx.frozen && p.tx.pending == Amount::default())
            {
                let keep = p.deficit != 0;
                if !keep {
                    self.peers.get_mut(&peer).unwrap().deficit = SCHEDULER_QUANTUM;
                }
                self.ready_peer(peer, true);
                if keep && self.output.back() == Some(&peer) {
                    self.output.pop_back();
                    self.output.push_front(peer);
                }
            }
        }
        result
    }
    pub fn poll_events(&mut self, max: usize) -> Vec<ManagedEvent> {
        self.poll_events_with_data_budget(max, |_, _| true)
    }
    /// Transfer DATA only when its owner can retain the entire next chunk.
    /// The callback accounts cumulative admissions within this batch. Denied
    /// chunks remain owned by Core; other ready streams continue to be visited.
    pub fn poll_events_with_data_budget(
        &mut self,
        max: usize,
        mut admit: impl FnMut(StreamKey, usize) -> bool,
    ) -> Vec<ManagedEvent> {
        self.wake_budget_waiters();
        let mut result = Vec::new();
        let max = max.min(MAX_BATCH_EVENTS);
        let mut attempts = max.saturating_add(self.streams.len());
        while result.len() < max && attempts > 0 {
            attempts -= 1;
            let Some(peer) = self.events.pop_front() else {
                break;
            };
            self.event_set.remove(&peer);
            let p = self.peers.get_mut(&peer).unwrap();
            if let Some(id) = p.events.pop_front() {
                p.event_set.remove(&id);
                let key = StreamKey {
                    peer,
                    stream_id: id,
                };
                if let Some(e) = self.streams.get_mut(&key) {
                    let before = e.stream.snapshot().pending_send_bytes;
                    if e.stream
                        .next_event_data_bytes()
                        .is_none_or(|bytes| admit(key, bytes))
                        && let Some(event) = e.stream.poll_events(1).pop()
                    {
                        result.push(ManagedEvent { key, event });
                    }
                    self.refresh(key, before);
                    self.reap(key);
                }
            }
            if !self.peers[&peer].events.is_empty() {
                self.ready_peer(peer, false);
            }
        }
        result
    }
    pub fn tick(&mut self, now: u64) -> Result<(), ManagerError> {
        if now < self.now {
            return Err(Error::InvalidTime.into());
        }
        self.now = now;
        let expired: Vec<_> = self
            .peers
            .iter()
            .filter(|(_, p)| p.freeze_deadline.is_some_and(|d| now >= d))
            .map(|(id, _)| *id)
            .collect();
        for peer in expired {
            self.peer_lost(peer);
        }
        self.grow_credit();
        while let Some(&(deadline, peer)) = self.pressure_deadlines.first() {
            if deadline > now {
                break;
            }
            self.pressure_deadlines.remove(&(deadline, peer));
            self.apply_pressure(peer)?;
        }
        while let Some(&(deadline, key)) = self.deadlines.first() {
            if deadline > now {
                break;
            }
            self.deadlines.remove(&(deadline, key));
            if let Some(e) = self.streams.get_mut(&key) {
                let before = e.stream.snapshot().pending_send_bytes;
                e.deadline = None;
                e.stream.tick(now)?;
                self.refresh(key, before);
            }
        }
        Ok(())
    }
    pub fn next_deadline(&self) -> Option<u64> {
        self.deadlines
            .first()
            .map(|(d, _)| *d)
            .into_iter()
            .chain(self.peers.values().filter_map(|p| p.freeze_deadline))
            .chain(
                self.pressure_deadlines
                    .first()
                    .map(|(deadline, _)| *deadline),
            )
            .min()
    }
    pub fn transport_lost(&mut self) {
        if self.failed {
            return;
        }
        self.failed = true;
        let peers: Vec<_> = self.peers.keys().copied().collect();
        for peer in peers {
            self.peer_lost(peer);
        }
    }
    pub fn protocol_error(&mut self, peer: PeerId) {
        let keys: Vec<_> = self
            .streams
            .range(
                StreamKey { peer, stream_id: 0 }..=StreamKey {
                    peer,
                    stream_id: u64::MAX,
                },
            )
            .map(|(k, _)| *k)
            .collect();
        for key in keys {
            let _ = self.close(key, CloseReason::ProtocolError);
        }
    }
    pub fn peer_lost(&mut self, peer: PeerId) {
        if let Some(p) = self.peers.get_mut(&peer) {
            if !p.failed {
                self.failed_peers.insert(peer);
                self.minimum_send_records -= 64usize.saturating_sub(p.retained_send_records);
            }
            p.failed = true;
            p.rejection = None;
            p.controls.clear();
            p.freeze_deadline = None;
            if let Some(pressure) = p.pressure.take() {
                self.pressure_deadlines.remove(&(pressure.deadline, peer));
            }
        }
        let keys: Vec<_> = self
            .streams
            .range(
                StreamKey { peer, stream_id: 0 }..=StreamKey {
                    peer,
                    stream_id: u64::MAX,
                },
            )
            .map(|(k, _)| *k)
            .collect();
        for key in keys {
            let e = self.streams.get_mut(&key).unwrap();
            let before = e.stream.snapshot().pending_send_bytes;
            e.stream.transport_lost();
            self.refresh(key, before);
        }
        if let Some(p) = self.peers.get_mut(&peer) {
            self.receive_promises = self
                .receive_promises
                .sub(p.reserved_receive)
                .expect("lost peer conservation");
            p.rx.allowed = p.rx.consumed;
            p.reserved_receive = Amount::default();
        }
    }
    #[cfg(feature = "nats")]
    pub(crate) fn poll_failed_peer(&mut self) -> Option<PeerId> {
        self.failed_peers.pop_first()
    }
    #[cfg(feature = "nats")]
    pub(crate) fn peer_failed(&self, peer: PeerId) -> bool {
        self.peers.get(&peer).is_some_and(|p| p.failed)
    }
    #[cfg(feature = "fault-injection")]
    pub(crate) fn expire_credit_freeze(&mut self, peer: PeerId) {
        if let Some(p) = self.peers.get_mut(&peer) {
            p.freeze_deadline = Some(self.now);
        }
    }
    pub fn snapshot(&self, key: StreamKey) -> Option<Snapshot> {
        self.streams.get(&key).map(|e| e.stream.snapshot())
    }
    /// Explicit per-stream query; no background scan or payload allocation.
    /// Backlog age measures one continuous nonempty send backlog, not the age
    /// of an individual frame or the remote application's processing time.
    pub fn flow_observation(&self, key: StreamKey, now_ms: u64) -> Option<FlowObservation> {
        let e = self.streams.get(&key)?;
        let p = self.peers.get(&key.peer)?;
        Some(FlowObservation {
            backlog_age_ms: e.backlog_since.map(|since| now_ms.saturating_sub(since)),
            application_credit_blocked: e.stream.credit_blocked()
                && matches!(e.stream.state(), State::Open | State::HalfClosedRemote),
            local_budget_blocked: self.budget_waiters.contains(&key),
            queued_for_output: p.output_set.contains(&key.stream_id),
            independent_control_ready: e.stream.priority_control_ready(),
            unread_bytes: e.received - e.consumed,
            pending_send_bytes: e.stream.snapshot().pending_send_bytes,
        })
    }
    /// Inspect validated peer limits without advancing stream ownership.
    /// Unknown streams and streams awaiting peer negotiation return None.
    pub fn peer_limits(&self, key: StreamKey) -> Option<PeerLimits> {
        self.streams.get(&key).and_then(|e| e.stream.peer_limits())
    }
    /// Constant-time counters; buffer occupancy remains a detailed diagnostic.
    pub fn aggregate(&self) -> Aggregate {
        Aggregate {
            peers: self.peers.len(),
            streams: self.streams.len(),
            reserved_receive_bytes: self.receive_promises.bytes as usize,
            pending_send_bytes: self.pending_send,
            ready_output_peers: self.output_set.len(),
            ready_event_peers: self.event_set.len(),
        }
    }
    pub fn peer_streams(&self, id: PeerId) -> Option<usize> {
        self.peers.get(&id).map(|p| p.count)
    }
    /// Resource inspection is explicit and O(stream count), never a driver turn.
    pub fn resources(&self) -> Resources {
        let mut r = Resources {
            peers: self.peers.len(),
            streams: self.streams.len(),
            reserved_receive_bytes: self.receive_promises.bytes as usize,
            pending_send_bytes: self.pending_send,
            ready_output_peers: self.output_set.len(),
            ready_event_peers: self.event_set.len(),
            pending_rejections: self
                .peers
                .values()
                .filter(|p| p.rejection.is_some())
                .count(),
            ..Resources::default()
        };
        for e in self.streams.values() {
            let s = e.stream.snapshot();
            r.buffered_receive_bytes += s.buffered_receive_bytes;
            r.receive_capacity_bytes += s.receive_capacity_bytes;
            r.receive_unconsumed_bytes += s.receive_unconsumed_bytes;
        }
        r
    }
}

const _: () = {
    assert!(std::mem::size_of::<Entry>() <= skvoz_core_contracts::ENTRY_BYTES);
    assert!(std::mem::size_of::<Peer>() <= skvoz_core_contracts::PEER_BYTES);
};
