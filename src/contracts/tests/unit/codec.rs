use super::*;
#[test]
fn oversized_sequence_stops_at_the_bridge_bound() {
    use serde::ser::SerializeSeq;
    let visited = std::cell::Cell::new(0);
    struct Sequence<'a>(&'a std::cell::Cell<usize>);
    impl serde::Serialize for Sequence<'_> {
        fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
            let mut sequence = serializer.serialize_seq(None)?;
            for n in 0..1_000_000 {
                self.0.set(n + 1);
                sequence.serialize_element(&255u8)?;
            }
            sequence.end()
        }
    }
    assert!(encode(&Sequence(&visited)).is_err());
    assert!(visited.get() < MAX_DTO_BYTES);
    let mut output = Capped(Vec::new());
    output.write_all(&vec![0; MAX_DTO_BYTES]).unwrap();
    assert!(output.write_all(&[0]).is_err());
    assert!(output.0.capacity() <= MAX_DTO_BYTES);
}
