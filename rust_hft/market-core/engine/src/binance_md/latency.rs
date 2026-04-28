#[repr(C)]
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct LatencyTrace {
    pub recv_ns: i64,
    pub parse_done_ns: i64,
    pub book_done_ns: i64,
    pub feature_done_ns: i64,
    pub signal_done_ns: i64,
}

impl LatencyTrace {
    #[inline]
    pub fn parse_latency_ns(self) -> i64 {
        self.parse_done_ns - self.recv_ns
    }

    #[inline]
    pub fn book_latency_ns(self) -> i64 {
        self.book_done_ns - self.parse_done_ns
    }

    #[inline]
    pub fn feature_latency_ns(self) -> i64 {
        self.feature_done_ns - self.book_done_ns
    }

    #[inline]
    pub fn signal_latency_ns(self) -> i64 {
        self.signal_done_ns - self.feature_done_ns
    }

    #[inline]
    pub fn total_latency_ns(self) -> i64 {
        self.signal_done_ns - self.recv_ns
    }
}
