#[path = "../../../nats_fixture.rs"]
mod support;
use super::*;
use skvoz_core::ManagerConfig;
#[tokio::test]
async fn exhausted_handle_rejects_before_runtime_stream_allocation() {
    let mut fixture = support::Fixture::new();
    fixture.broker();
    let config = |id| fixture.config(id);
    let limits = ManagerConfig::default();
    let mut server = NatsRuntime::connect(config(0), limits).await.unwrap();
    let mut runtime = NatsRuntime::connect(config(1), limits).await.unwrap();
    runtime.join_peer(PeerId(0)).unwrap();
    let deadline = Instant::now() + Duration::from_secs(10);
    while !runtime.peer_ready(PeerId(0)) || !server.peer_ready(PeerId(1)) {
        assert!(Instant::now() < deadline);
        server.turn(Duration::ZERO).await.unwrap();
        runtime.turn(Duration::ZERO).await.unwrap();
        server.poll_events(256);
        runtime.poll_events(256);
        tokio::time::sleep(Duration::from_millis(2)).await;
    }
    let profile = Profile {
        ipc_path: std::path::PathBuf::new(),
        runtime: config(1),
        manager: limits,
        owners: 2,
        streams_per_owner: 2,
        output_frames: 8,
        output_bytes: 8192,
        timeout: Duration::from_secs(5),
        shutdown_timeout: Duration::from_secs(5),
    };
    let (socket, _reader) = UnixStream::pair().unwrap();
    let now = Instant::now();
    let owner = Owner {
        socket,
        decoder: Decoder::default(),
        hello: true,
        last_request: 0,
        started: now,
        partial_since: None,
        output_since: now,
        output: VecDeque::new(),
        output_bytes: 0,
        streams: BTreeSet::new(),
    };
    let mut host = Host {
        runtime,
        profile,
        owners: BTreeMap::from([(1, owner)]),
        bindings: BTreeMap::new(),
        keys: BTreeMap::new(),
        acceptor: None,
        epoch: 1,
        sequence: u64::MAX,
        session: 1,
    };
    let before = host.runtime.status().resources;
    host.command(
        1,
        Frame {
            kind: 2,
            request: 1,
            handle: 0,
            payload: 0u64.to_be_bytes().to_vec(),
        },
    );
    assert_eq!(before.streams, 0);
    assert_eq!(host.runtime.status().resources, before);
    assert!(host.bindings.is_empty());
    assert!(host.keys.is_empty());
    assert!(host.owners[&1].streams.is_empty());
    let reply = Frame::decode(host.owners[&1].output[0].bytes[4..].to_vec()).unwrap();
    assert_eq!(
        u16::from_be_bytes(reply.payload[..2].try_into().unwrap()),
        5
    );
    host.runtime.shutdown().await.unwrap();
    server.shutdown().await.unwrap();
}
