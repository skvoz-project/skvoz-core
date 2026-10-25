#[path = "../../../nats_fixture.rs"]
mod support;
use super::*;
use std::sync::atomic::AtomicBool;
#[derive(Default)]
struct Recorder {
    retained: AtomicUsize,
    released: AtomicUsize,
    wakes: AtomicUsize,
}
unsafe extern "C" fn retain(context: *mut c_void) {
    unsafe {
        (&*context.cast::<Recorder>())
            .retained
            .fetch_add(1, Ordering::SeqCst);
        Arc::<Recorder>::increment_strong_count(context.cast());
    }
}
unsafe extern "C" fn release(context: *mut c_void) {
    unsafe {
        (&*context.cast::<Recorder>())
            .released
            .fetch_add(1, Ordering::SeqCst);
        Arc::<Recorder>::decrement_strong_count(context.cast());
    }
}
unsafe extern "C" fn wake(context: *mut c_void) {
    unsafe {
        (&*context.cast::<Recorder>())
            .wakes
            .fetch_add(1, Ordering::SeqCst);
    }
}
fn callback(recorder: &Arc<Recorder>) -> Wake {
    Wake {
        context: Arc::as_ptr(recorder).cast_mut().cast(),
        retain,
        release,
        wake,
    }
}
unsafe extern "C" fn host_ready(context: *mut c_void) -> bool {
    if !context.is_null() {
        unsafe {
            (&*context.cast::<AtomicBool>()).store(true, Ordering::SeqCst);
        }
    }
    true
}
struct Handles {
    runtime: *mut c_void,
    operation: *mut c_void,
}
impl Handles {
    fn new() -> Self {
        let mut runtime = std::ptr::null_mut();
        assert_eq!(unsafe { skvoz_core_v1_runtime_new(&mut runtime) }, OK);
        Self {
            runtime,
            operation: std::ptr::null_mut(),
        }
    }
    fn free_operation(&mut self) {
        if !self.operation.is_null() {
            assert_eq!(unsafe { skvoz_core_v1_operation_free(self.operation) }, OK);
            self.operation = std::ptr::null_mut();
        }
    }
}
impl Drop for Handles {
    fn drop(&mut self) {
        self.free_operation();
        if !self.runtime.is_null() {
            unsafe {
                skvoz_core_v1_runtime_free(self.runtime);
            }
        }
    }
}
#[test]
fn retained_wake_outlives_cancelled_operation_and_runtime_with_balanced_references() {
    let recorder = Arc::new(Recorder::default());
    let retained = Arc::new(Mutex::new(None));
    let slot = retained.clone();
    let mut handles = Handles::new();
    let owner = handles.runtime.cast::<Runtime>();
    unsafe {
        (*owner).active = true;
    }
    let future = std::future::poll_fn(move |cx| {
        *slot.lock().unwrap() = Some(cx.waker().clone());
        Poll::<Result<Completed, RuntimeError>>::Pending
    });
    handles.operation = Box::into_raw(Box::new(Operation {
        owner,
        future: Some(Box::pin(future)),
        host: Arc::new(Mutex::new(None)),
    }))
    .cast();
    let mut reply = Reply::default();
    assert_eq!(
        unsafe {
            skvoz_core_v1_poll(
                handles.operation,
                callback(&recorder),
                std::ptr::null_mut(),
                host_ready,
                &mut reply,
            )
        },
        PENDING
    );
    handles.free_operation();
    assert!(!unsafe { (*owner).active });
    assert_eq!(unsafe { skvoz_core_v1_runtime_free(handles.runtime) }, OK);
    handles.runtime = std::ptr::null_mut();
    let retained = retained.lock().unwrap().take().unwrap();
    retained.wake_by_ref();
    drop(retained);
    assert_eq!(recorder.wakes.load(Ordering::SeqCst), 1);
    assert_eq!(
        recorder.retained.load(Ordering::SeqCst),
        recorder.released.load(Ordering::SeqCst)
    );
    assert_eq!(Arc::strong_count(&recorder), 1);
}
#[tokio::test(flavor = "current_thread")]
async fn host_wake_cannot_bypass_extracted_output_and_cancel_preserves_fence() {
    use engine::{Event as EngineEvent, PeerId};
    let mut fixture = support::Fixture::new();
    fixture.broker();
    let limits = support::Fixture::limits();
    let mut server = engine::runtime::NatsRuntime::connect(fixture.config(0), limits)
        .await
        .unwrap();
    let mut client = engine::runtime::NatsRuntime::connect(fixture.config(1), limits)
        .await
        .unwrap();
    let deadline = std::time::Instant::now() + Duration::from_secs(5);
    while !client.peer_ready(PeerId(0)) || !server.peer_ready(PeerId(1)) {
        assert!(std::time::Instant::now() < deadline);
        client.turn(Duration::ZERO).await.unwrap();
        server.turn(Duration::from_millis(1)).await.unwrap();
    }
    let key = client.open(PeerId(0), b"guard").unwrap();
    let mut opened = false;
    while !opened {
        assert!(std::time::Instant::now() < deadline);
        client.turn(Duration::ZERO).await.unwrap();
        server.turn(Duration::from_millis(1)).await.unwrap();
        for event in server.poll_events(256) {
            if matches!(event.event, EngineEvent::IncomingOpen { .. }) {
                server.accept(event.key, b"").unwrap();
            }
        }
        opened = client
            .poll_events(256)
            .iter()
            .any(|e| matches!(e.event, EngineEvent::Opened { .. }));
    }
    client.send(key, b"cancel after extraction").unwrap();
    let mut extracted = client.inject_pause_next_data_after_extraction();
    let mut handles = Handles::new();
    let owner = handles.runtime.cast::<Runtime>();
    unsafe {
        (*owner).node = Some(Box::new(client));
    }
    let recorder = Arc::new(Recorder::default());
    let host = AtomicBool::new(false);
    let mut observed = false;
    while !observed {
        assert!(
            std::time::Instant::now() < deadline,
            "ABI extraction deadline"
        );
        if handles.operation.is_null() {
            host.store(false, Ordering::SeqCst);
            let mut reply = Reply::default();
            assert_eq!(
                unsafe {
                    skvoz_core_v1_start(
                        handles.runtime,
                        1,
                        1000,
                        std::ptr::null(),
                        0,
                        &mut handles.operation,
                        &mut reply,
                    )
                },
                OK
            );
        }
        let mut reply = Reply::default();
        let code = unsafe {
            skvoz_core_v1_poll(
                handles.operation,
                callback(&recorder),
                (&host as *const AtomicBool).cast_mut().cast(),
                host_ready,
                &mut reply,
            )
        };
        unsafe {
            skvoz_core_v1_buffer_free(reply.buffer);
        }
        if extracted.try_recv().is_ok() {
            assert_eq!(code, PENDING);
            assert!(
                !host.load(Ordering::SeqCst),
                "host wake bypassed active output"
            );
            observed = true;
        } else if code == OK {
            handles.free_operation();
        } else {
            assert_eq!(code, PENDING);
        }
        if !observed {
            server.turn(Duration::from_millis(1)).await.unwrap();
            tokio::task::yield_now().await;
        }
    }
    handles.free_operation();
    let client = unsafe { (*owner).node.as_mut().unwrap() };
    let events = client.poll_events(256);
    assert!(events.iter().any(|e| e.key == key
        && matches!(
            e.event,
            EngineEvent::Closed {
                reason: engine::CloseReason::TransportLost
            }
        )));
    assert_eq!(client.resources().streams, 0);
    server.shutdown().await.unwrap();
}
