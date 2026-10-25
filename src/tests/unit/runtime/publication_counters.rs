use super::*;
#[test]
fn transport_size_bins_and_published_counters_saturate() {
    let mut counters = Counters::default();
    for size in [1499, 1500, 8191, 8192, 16383, 16384, 16385, 65536] {
        published(
            &mut counters,
            &crate::Frame::Data {
                offset: 0,
                bytes: vec![0; size].into(),
            },
        );
    }
    assert_eq!(counters.data_frame_size_bins, [1, 2, 2, 1, 2]);
    assert_eq!(counters.data_frames_published, 8);
    counters.data_frames_published = u64::MAX;
    counters.data_payload_bytes_published = u64::MAX;
    counters.data_frame_size_bins[0] = u64::MAX;
    counters.window_frames_published = u64::MAX;
    published(
        &mut counters,
        &crate::Frame::Data {
            offset: 0,
            bytes: vec![0; 1].into(),
        },
    );
    published(&mut counters, &crate::Frame::WindowUpdate { consumed: 1 });
    assert_eq!(counters.data_frames_published, u64::MAX);
    assert_eq!(counters.data_payload_bytes_published, u64::MAX);
    assert_eq!(counters.data_frame_size_bins[0], u64::MAX);
    assert_eq!(counters.window_frames_published, u64::MAX);
}
