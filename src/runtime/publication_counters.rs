use super::Counters;
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
#[path = "../tests/unit/runtime/publication_counters.rs"]
mod counter_tests;
