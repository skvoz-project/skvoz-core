pub use skvoz_core_contracts::runtime::*;
pub use skvoz_core_tls::verified_tls_config;
pub fn published(counters: &mut Counters, frame: &crate::Frame) {
    match frame {
        crate::Frame::Data { bytes, .. } => {
            counters.data_frames_published = counters.data_frames_published.saturating_add(1);
            counters.data_payload_bytes_published = counters
                .data_payload_bytes_published
                .saturating_add(bytes.len() as u64);
            let bin = match bytes.len() {
                0..1500 => 0,
                1500..8192 => 1,
                8192..16384 => 2,
                16384 => 3,
                _ => 4,
            };
            counters.data_frame_size_bins[bin] =
                counters.data_frame_size_bins[bin].saturating_add(1);
        }
        crate::Frame::WindowUpdate { .. } => {
            counters.window_frames_published = counters.window_frames_published.saturating_add(1);
        }
        _ => {}
    }
}
#[cfg(test)]
mod counter_tests {
    use super::*;
    use crate::{ManagerConfig, PeerId};
    #[test]
    fn source_subscription_metadata_does_not_multiply_packet_queues_by_peer_count() {
        let config = RuntimeConfig::new(
            "tls://localhost:4222",
            Trust::System,
            Authentication {
                username: "fixture".into(),
                password: "fixture".into(),
            },
            "fixture",
            PeerId(0),
            Membership::BrokerAuthorized,
        );
        let one = ManagerConfig {
            max_peers: 1,
            ..ManagerConfig::default()
        };
        let eight = ManagerConfig {
            max_peers: 8,
            ..one
        };
        let bound_one = config.validate_profile(one).unwrap();
        let bound_eight = config.validate_profile(eight).unwrap();
        assert_eq!(
            bound_eight - bound_one,
            7 * (crate::TRANSPORT_DELIVERY_STATE_BYTES + 2048)
        );
    }

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
}
