use super::*;

#[test]
fn invalid_append_keeps_existing_prefix_bytes_and_capacity() {
    let invalid = [
        Frame::Data {
            offset: 0,
            bytes: Box::new([]),
        },
        Frame::Data {
            offset: 0,
            bytes: vec![0; MAX_FRAME_BYTES as usize + 1].into(),
        },
        Frame::Open {
            receive_window: 0,
            max_frame: 1,
            metadata: Box::new([]),
        },
        Frame::Reject {
            reason: vec![0; MAX_METADATA_BYTES + 1].into(),
        },
        Frame::Close {
            reason: CloseReason::Finished,
        },
    ];
    for frame in invalid {
        let mut buffer = b"existing envelope prefix".to_vec();
        let original = buffer.clone();
        let capacity = buffer.capacity();
        assert!(encode_into(&mut buffer, 1, &frame).is_err());
        assert_eq!(buffer, original);
        assert_eq!(buffer.capacity(), capacity);
    }
}

#[test]
fn exact_size_append_preserves_prefix_and_maximum_payload() {
    for size in [1, 1508, 16384, MAX_FRAME_BYTES as usize] {
        let frame = Frame::Data {
            offset: 123,
            bytes: vec![42; size].into(),
        };
        let encoded = encode(7, &frame).unwrap();
        assert_eq!(encoded_size(7, &frame).unwrap(), encoded.len());
        let mut envelope = Vec::with_capacity(24 + encoded.len());
        envelope.extend_from_slice(&[9; 24]);
        let capacity = envelope.capacity();
        encode_into(&mut envelope, 7, &frame).unwrap();
        assert_eq!(envelope.capacity(), capacity);
        assert_eq!(&envelope[..24], &[9; 24]);
        assert_eq!(&envelope[24..], encoded);
    }
}
