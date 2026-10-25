use super::*;

fn vector(name: &str) -> Vec<u8> {
    let row = include_str!("../../fixtures/runtime-v3.tsv")
        .lines()
        .find_map(|line| line.strip_prefix(&format!("{name}\t")))
        .expect("public runtime vector");
    row.as_bytes()
        .chunks_exact(2)
        .map(|pair| u8::from_str_radix(std::str::from_utf8(pair).unwrap(), 16).unwrap())
        .collect()
}

#[test]
fn control_layout_is_exact_and_reserved_bytes_fail_closed() {
    assert_eq!(crate::runtime::shard(crate::PeerId(u64::MAX), 7), 1);
    assert_eq!(crate::runtime::shard(crate::PeerId(u64::MAX), 32), 31);
    let control = Control {
        kind: 8,
        sender: 1,
        recipient: 2,
        hello: 3,
        token: 4,
        watermark: 5,
        serial: 6,
        ack: 7,
        epoch: 8,
        records: 9,
        applied_bytes: 10,
        applied_records: 11,
        ..Control::default()
    };
    let mut bytes = control.encode();
    assert_eq!(bytes, vector("control-limit"));
    assert_eq!(bytes.len(), CONTROL_BYTES);
    assert_eq!(&bytes[..8], b"SKC3\x08\0\0\0");
    assert_eq!(Control::decode(&bytes).unwrap(), control);
    for length in 0..CONTROL_BYTES {
        assert!(Control::decode(&bytes[..length]).is_err());
    }
    for (offset, value) in [(4, 15), (0, 0), (3, b'1'), (3, b'2')] {
        let mut invalid = bytes.clone();
        invalid[offset] = value;
        assert!(Control::decode(&invalid).is_err());
    }
    bytes[7] = 1;
    assert!(Control::decode(&bytes).is_err());
    bytes[7] = 0;
    bytes.push(0);
    assert!(Control::decode(&bytes).is_err());
}

#[test]
fn data_envelope_preserves_opaque_body_and_rejects_truncated_lengths() {
    let header = DataHeader {
        token: 7,
        sequence: 9,
        control_ack: 4,
        epoch: 2,
    };
    let mut bytes = Vec::new();
    header.encode(3, &mut bytes).unwrap();
    assert_eq!(bytes.len(), DATA_HEADER);
    bytes.extend_from_slice(&[0, 255, 4]);
    assert_eq!(bytes, vector("data-opaque"));
    let (decoded, body) = DataHeader::decode(&bytes).unwrap();
    assert_eq!(decoded, header);
    assert_eq!(body, &[0, 255, 4]);
    for length in 0..bytes.len() {
        assert!(DataHeader::decode(&bytes[..length]).is_err());
    }
    bytes.push(0);
    assert!(DataHeader::decode(&bytes).is_err());
}
