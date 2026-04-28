/// Borrowed raw websocket frame with the receive timestamp already attached.
#[derive(Debug, Clone, Copy)]
pub struct RawFrameRef<'a> {
    pub recv_ts_ns: i64,
    pub bytes: &'a [u8],
}

/// Fixed-capacity raw frame buffer for later buffer-pool integration.
#[derive(Debug, Clone)]
pub struct RawFrameBuf<const N: usize> {
    pub recv_ts_ns: i64,
    pub len: usize,
    pub data: [u8; N],
}

impl<const N: usize> RawFrameBuf<N> {
    #[inline]
    pub fn try_from_slice(recv_ts_ns: i64, bytes: &[u8]) -> Option<Self> {
        if bytes.len() > N {
            return None;
        }

        let mut data = [0_u8; N];
        data[..bytes.len()].copy_from_slice(bytes);

        Some(Self {
            recv_ts_ns,
            len: bytes.len(),
            data,
        })
    }

    #[inline]
    pub fn as_ref(&self) -> RawFrameRef<'_> {
        RawFrameRef {
            recv_ts_ns: self.recv_ts_ns,
            bytes: &self.data[..self.len],
        }
    }
}
