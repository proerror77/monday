use engine::dataflow::{ring_buffer};

#[test]
fn ring_buffer_basic() {
    let (p, c) = ring_buffer::spsc_ring_buffer::<i32>(8);
    assert!(p.send(1).is_ok());
    assert_eq!(c.recv(), Some(1));
}

