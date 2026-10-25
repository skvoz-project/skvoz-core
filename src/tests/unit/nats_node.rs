use super::*;
#[tokio::test]
async fn dropped_output_future_latches_failure_but_completed_batch_does_not() {
    let failure = Arc::new(Failure::default());
    let f = failure.clone();
    let mut pending = Box::pin(async move {
        let _guard = OutputGuard {
            failure: f,
            completed: false,
        };
        std::future::pending::<()>().await;
    });
    assert!(pending.as_mut().now_or_never().is_none());
    drop(pending);
    assert_eq!(failure.kind(), Some(FailureKind::ClientError));
    let healthy = Arc::new(Failure::default());
    let mut guard = OutputGuard {
        failure: healthy.clone(),
        completed: false,
    };
    guard.completed = true;
    drop(guard);
    assert_eq!(healthy.kind(), None);
}
