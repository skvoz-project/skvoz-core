//! Stable native boundary; engine ownership never crosses the allocator boundary.
use skvoz_core as engine;
use skvoz_core_contracts::{abi::*, runtime::*};
use std::{
    ffi::c_void,
    future::Future,
    panic::{AssertUnwindSafe, catch_unwind},
    pin::Pin,
    sync::{
        Arc, Mutex, OnceLock,
        atomic::{AtomicUsize, Ordering},
    },
    task::{Context, Poll, Waker},
    time::Duration,
};

static EXECUTOR: OnceLock<tokio::runtime::Runtime> = OnceLock::new();
static HANDLES: AtomicUsize = AtomicUsize::new(0);
fn executor() -> &'static tokio::runtime::Runtime {
    EXECUTOR.get_or_init(|| {
        tokio::runtime::Builder::new_multi_thread()
            .worker_threads(1)
            .max_blocking_threads(2)
            .thread_stack_size(512 * 1024)
            .enable_all()
            .thread_name("skvoz-core")
            .build()
            .expect("Core executor creation failed")
    })
}
struct Runtime {
    node: Option<Box<engine::runtime::NatsRuntime>>,
    active: bool,
    poisoned: bool,
}
impl Drop for Runtime {
    fn drop(&mut self) {
        HANDLES.fetch_sub(1, Ordering::AcqRel);
    }
}
#[derive(Clone, Copy)]
struct Host {
    context: usize,
    poll: HostPoll,
}
struct HostWake(Arc<Mutex<Option<Host>>>);
impl Future for HostWake {
    type Output = ();
    fn poll(self: Pin<&mut Self>, _: &mut Context<'_>) -> Poll<()> {
        let host = *self.0.lock().expect("Core host callback lock poisoned");
        if host.is_some_and(|h| unsafe { (h.poll)(h.context as *mut c_void) }) {
            Poll::Ready(())
        } else {
            Poll::Pending
        }
    }
}
enum Completed {
    Connected(engine::runtime::NatsRuntime),
    Turn(usize),
    Shutdown,
}
struct Operation {
    owner: *mut Runtime,
    future: Option<Pin<Box<dyn Future<Output = Result<Completed, RuntimeError>> + Send>>>,
    host: Arc<Mutex<Option<Host>>>,
}
impl Drop for Operation {
    fn drop(&mut self) {
        // Drop guards fence extracted output before releasing the exclusive owner.
        struct Release(*mut Runtime);
        impl Drop for Release {
            fn drop(&mut self) {
                unsafe {
                    (*self.0).active = false;
                }
            }
        }
        let _release = Release(self.owner);
        self.future.take();
    }
}
struct Notify(Wake);
// The ABI contract requires thread-safe retain/wake/release callbacks.
unsafe impl Send for Notify {}
unsafe impl Sync for Notify {}
impl std::task::Wake for Notify {
    fn wake(self: Arc<Self>) {
        unsafe {
            (self.0.wake)(self.0.context);
        }
    }
    fn wake_by_ref(self: &Arc<Self>) {
        unsafe {
            (self.0.wake)(self.0.context);
        }
    }
}
impl Drop for Notify {
    fn drop(&mut self) {
        unsafe {
            (self.0.release)(self.0.context);
        }
    }
}
fn guarded(owner: *mut Runtime, f: impl FnOnce() -> i32) -> i32 {
    match catch_unwind(AssertUnwindSafe(f)) {
        Ok(code) => code,
        Err(_) => {
            if !owner.is_null() {
                unsafe {
                    (*owner).poisoned = true;
                }
            }
            PANIC
        }
    }
}
fn json(value: &impl serde::Serialize) -> Result<Buffer, i32> {
    let bytes = skvoz_core_contracts::codec::encode(value).map_err(|_| INVALID)?;
    if bytes.len() > MAX_DTO_BYTES {
        return Err(INVALID);
    }
    let mut bytes = bytes.into_boxed_slice();
    let result = Buffer {
        data: bytes.as_mut_ptr(),
        len: bytes.len(),
    };
    std::mem::forget(bytes);
    Ok(result)
}
fn error(error: RuntimeError, reply: &mut Reply) -> i32 {
    match json(&error) {
        Ok(buffer) => {
            reply.buffer = buffer;
            CORE_ERROR
        }
        Err(code) => code,
    }
}
fn query(value: &impl serde::Serialize, reply: &mut Reply) -> i32 {
    match json(value) {
        Ok(buffer) => {
            reply.buffer = buffer;
            OK
        }
        Err(code) => code,
    }
}
unsafe fn input<'a>(data: *const u8, len: usize, max: usize) -> Result<&'a [u8], i32> {
    if len > max || (data.is_null() && len != 0) {
        return Err(INVALID);
    }
    Ok(if len == 0 {
        &[]
    } else {
        unsafe { std::slice::from_raw_parts(data, len) }
    })
}
#[unsafe(no_mangle)]
pub extern "C" fn skvoz_core_v1_version() -> u32 {
    ABI_VERSION
}
#[unsafe(no_mangle)]
pub extern "C" fn skvoz_core_v1_capabilities() -> u64 {
    CAPABILITIES
}
#[unsafe(no_mangle)]
pub unsafe extern "C" fn skvoz_core_v1_buffer_free(buffer: Buffer) {
    let _ = catch_unwind(AssertUnwindSafe(|| {
        if !buffer.data.is_null() {
            unsafe {
                drop(Box::from_raw(std::ptr::slice_from_raw_parts_mut(
                    buffer.data,
                    buffer.len,
                )));
            }
        }
    }));
}
#[unsafe(no_mangle)]
pub unsafe extern "C" fn skvoz_core_v1_runtime_new(out: *mut *mut c_void) -> i32 {
    guarded(std::ptr::null_mut(), || {
        if out.is_null() {
            return INVALID;
        }
        if HANDLES
            .fetch_update(Ordering::AcqRel, Ordering::Acquire, |n| {
                (n < MAX_HANDLES).then_some(n + 1)
            })
            .is_err()
        {
            return BUSY;
        }
        let runtime = Box::new(Runtime {
            node: None,
            active: false,
            poisoned: false,
        });
        unsafe {
            *out = Box::into_raw(runtime).cast();
        }
        OK
    })
}
#[unsafe(no_mangle)]
pub unsafe extern "C" fn skvoz_core_v1_runtime_free(handle: *mut c_void) -> i32 {
    let owner = handle.cast::<Runtime>();
    guarded(std::ptr::null_mut(), || {
        if owner.is_null() {
            return INVALID;
        }
        if unsafe { (*owner).active } {
            return BUSY;
        }
        let _enter = executor().enter();
        unsafe {
            drop(Box::from_raw(owner));
        }
        OK
    })
}
#[unsafe(no_mangle)]
pub unsafe extern "C" fn skvoz_core_v1_start(
    handle: *mut c_void,
    kind: u32,
    wait_ms: u64,
    data: *const u8,
    len: usize,
    out: *mut *mut c_void,
    reply: *mut Reply,
) -> i32 {
    let owner = handle.cast::<Runtime>();
    guarded(owner, || {
        if owner.is_null() || out.is_null() || reply.is_null() {
            return INVALID;
        }
        let runtime = unsafe { &mut *owner };
        if runtime.poisoned {
            return PANIC;
        }
        if runtime.active {
            return BUSY;
        }
        let _enter = executor().enter();
        let host = Arc::new(Mutex::new(None));
        let future: Pin<Box<dyn Future<Output = Result<Completed, RuntimeError>> + Send>> =
            match kind {
                0 if runtime.node.is_none() => {
                    let bytes = match unsafe { input(data, len, MAX_DTO_BYTES) } {
                        Ok(v) => v,
                        Err(e) => return e,
                    };
                    let (config, limits): (RuntimeConfig, engine::ManagerConfig) =
                        match serde_json::from_slice(bytes) {
                            Ok(v) => v,
                            Err(_) => return INVALID,
                        };
                    Box::pin(async move {
                        engine::runtime::NatsRuntime::connect(config, limits)
                            .await
                            .map(Completed::Connected)
                    })
                }
                1 | 2 if runtime.node.is_some() && wait_ms <= 86400000 => {
                    // The operation's exclusive borrow ends before runtime_free can succeed.
                    let node: &'static mut engine::runtime::NatsRuntime =
                        unsafe { &mut *(&mut **runtime.node.as_mut().unwrap() as *mut _) };
                    let wake = HostWake(host.clone());
                    if kind == 1 {
                        Box::pin(async move {
                            node.turn_with_wake(Duration::from_millis(wait_ms), wake)
                                .await
                                .map(Completed::Turn)
                        })
                    } else {
                        Box::pin(async move { node.shutdown().await.map(|()| Completed::Shutdown) })
                    }
                }
                _ => return INVALID,
            };
        runtime.active = true;
        unsafe {
            *out = Box::into_raw(Box::new(Operation {
                owner,
                future: Some(future),
                host,
            }))
            .cast();
        }
        OK
    })
}
#[unsafe(no_mangle)]
pub unsafe extern "C" fn skvoz_core_v1_poll(
    handle: *mut c_void,
    wake: Wake,
    host_context: *mut c_void,
    host_poll: HostPoll,
    reply: *mut Reply,
) -> i32 {
    let operation = handle.cast::<Operation>();
    if operation.is_null() || reply.is_null() {
        return INVALID;
    }
    let owner = unsafe { (*operation).owner };
    guarded(owner, || {
        let operation = unsafe { &mut *operation };
        if unsafe { (*owner).poisoned } {
            return PANIC;
        }
        let Some(future) = operation.future.as_mut() else {
            return INVALID;
        };
        unsafe {
            (wake.retain)(wake.context);
        }
        let waker = Waker::from(Arc::new(Notify(wake)));
        let mut context = Context::from_waker(&waker);
        *operation
            .host
            .lock()
            .expect("Core host callback lock poisoned") = Some(Host {
            context: host_context as usize,
            poll: host_poll,
        });
        let _enter = executor().enter();
        let result = future.as_mut().poll(&mut context);
        *operation
            .host
            .lock()
            .expect("Core host callback lock poisoned") = None;
        match result {
            Poll::Pending => PENDING,
            Poll::Ready(result) => {
                operation.future.take();
                match result {
                    Ok(Completed::Connected(node)) => {
                        unsafe {
                            (*owner).node = Some(Box::new(node));
                        }
                        OK
                    }
                    Ok(Completed::Turn(n)) => {
                        unsafe {
                            (*reply).value = n as u64;
                        }
                        OK
                    }
                    Ok(Completed::Shutdown) => OK,
                    Err(e) => error(e, unsafe { &mut *reply }),
                }
            }
        }
    })
}
#[unsafe(no_mangle)]
pub unsafe extern "C" fn skvoz_core_v1_operation_free(handle: *mut c_void) -> i32 {
    if handle.is_null() {
        return INVALID;
    }
    let operation = handle.cast::<Operation>();
    let owner = unsafe { (*operation).owner };
    guarded(owner, || {
        let _enter = executor().enter();
        unsafe {
            drop(Box::from_raw(operation));
        }
        OK
    })
}
#[unsafe(no_mangle)]
pub unsafe extern "C" fn skvoz_core_v1_call(
    handle: *mut c_void,
    command: u32,
    key: Key,
    argument: u64,
    data: *const u8,
    len: usize,
    reply: *mut Reply,
) -> i32 {
    let owner = handle.cast::<Runtime>();
    guarded(owner, || {
        if owner.is_null() || reply.is_null() {
            return INVALID;
        }
        let runtime = unsafe { &mut *owner };
        if runtime.poisoned {
            return PANIC;
        }
        if runtime.active {
            return BUSY;
        }
        let Some(node) = runtime.node.as_mut() else {
            return INVALID;
        };
        let bytes = match unsafe { input(data, len, EVENT_BRIDGE_BYTES) } {
            Ok(v) => v,
            Err(e) => return e,
        };
        let reply = unsafe { &mut *reply };
        let _enter = executor().enter();
        let key: RuntimeKey = key.into();
        let peer = engine::PeerId(argument);
        let result = match command {
            op::JOIN => node.join_peer(peer),
            op::REVOKE => node.revoke_peer(peer),
            op::AUTHORIZE => node.authorize_peer(peer, (key.incarnation != 0).then_some(key.epoch)),
            op::TERMINATE => {
                node.terminate_peer(peer);
                Ok(())
            }
            op::EPOCH => {
                reply.key.epoch_high = (node.epoch() >> 64) as u64;
                reply.key.epoch_low = node.epoch() as u64;
                Ok(())
            }
            op::STATUS => return query(&node.status(), reply),
            op::RESOURCES => return query(&node.resources(), reply),
            op::PEER_STATUS => return query(&node.peer_status(peer), reply),
            op::PEER_READY => {
                reply.value = u64::from(node.peer_ready(peer));
                Ok(())
            }
            op::PEER_DRAINED => {
                reply.value = u64::from(node.peer_streams_drained(peer));
                Ok(())
            }
            op::OPEN => node.open(peer, bytes).map(|key| reply.key = key.into()),
            op::ACCEPT => node.accept(key, bytes),
            op::REJECT => node.reject(key, bytes),
            op::SEND => match node.send(key, bytes) {
                Ok(engine::SendOutcome::Accepted(n)) => {
                    reply.value = n as u64;
                    Ok(())
                }
                Ok(engine::SendOutcome::WouldBlock) => return WOULD_BLOCK,
                Err(e) => Err(e),
            },
            op::FINISH => node.finish(key),
            op::CONSUME => node.consume_through(key, argument),
            op::CLOSE => node.close(key),
            op::SNAPSHOT => return query(&node.snapshot(key), reply),
            op::STREAM_DIAGNOSTICS => return query(&node.stream_diagnostics(key), reply),
            op::LIMITS => return query(&node.limits(), reply),
            op::PEER_LIMITS => return query(&node.peer_limits(key), reply),
            op::TRANSPORT_LOST => {
                node.transport_lost();
                Ok(())
            }
            op::DIAGNOSTICS => return query(&node.diagnostics(), reply),
            op::DELIVERY_STATUS => {
                return match node.delivery_status() {
                    Ok(status) => query(&status, reply),
                    Err(e) => error(e, reply),
                };
            }
            op::SET_DIAGNOSTICS => {
                node.set_diagnostics_enabled(argument != 0);
                Ok(())
            }
            _ => return INVALID,
        };
        match result {
            Ok(()) => OK,
            Err(e) => error(e, reply),
        }
    })
}
fn close_code(reason: engine::CloseReason) -> u32 {
    match reason {
        engine::CloseReason::Finished => 0,
        engine::CloseReason::Rejected => 1,
        engine::CloseReason::Cancelled => 2,
        engine::CloseReason::TransportLost => 3,
        engine::CloseReason::ProtocolError => 4,
        engine::CloseReason::OpenTimeout => 5,
        engine::CloseReason::ResourcePressure => 6,
    }
}
#[unsafe(no_mangle)]
pub unsafe extern "C" fn skvoz_core_v1_events(
    handle: *mut c_void,
    maximum: usize,
    context: *mut c_void,
    admit: Admit,
    receive: Receive,
) -> i32 {
    let owner = handle.cast::<Runtime>();
    guarded(owner, || {
        if owner.is_null() || maximum > engine::MAX_BATCH_EVENTS {
            return INVALID;
        }
        let runtime = unsafe { &mut *owner };
        if runtime.poisoned {
            return PANIC;
        }
        if runtime.active {
            return BUSY;
        }
        let Some(node) = runtime.node.as_mut() else {
            return INVALID;
        };
        let _enter = executor().enter();
        let events = node.poll_events_with_data_budget(maximum, |key, size| unsafe {
            admit(context, key.into(), size)
        });
        for event in events {
            let mut out = Event {
                key: event.key.into(),
                kind: 0,
                reason: 0,
                offset: 0,
                data: std::ptr::null(),
                len: 0,
            };
            let payload = match &event.event {
                engine::Event::IncomingOpen { metadata } => {
                    out.kind = 0;
                    Some(metadata)
                }
                engine::Event::Opened { metadata } => {
                    out.kind = 1;
                    Some(metadata)
                }
                engine::Event::Rejected { reason } => {
                    out.kind = 2;
                    Some(reason)
                }
                engine::Event::Data { offset, bytes } => {
                    out.kind = 3;
                    out.offset = *offset;
                    Some(bytes)
                }
                engine::Event::Writable => {
                    out.kind = 4;
                    None
                }
                engine::Event::RemoteFinished => {
                    out.kind = 5;
                    None
                }
                engine::Event::Closed { reason } => {
                    out.kind = 6;
                    out.reason = close_code(*reason);
                    None
                }
            };
            if let Some(bytes) = payload {
                out.data = bytes.as_ptr();
                out.len = bytes.len();
            }
            // Payload stays supplier-owned and is dropped exactly once after the callback.
            unsafe {
                receive(context, &out);
            }
        }
        OK
    })
}

#[cfg(test)]
#[path = "tests/unit/lib.rs"]
mod tests;
