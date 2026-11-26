//! lapis-ble-v1 Layer 1: `msg_id | chunk_idx | total_chunks | flags` chunk framing.

use std::time::{Duration, Instant};

pub const HEADER_LEN: usize = 4;
pub const FLAG_ENCRYPTED: u8 = 0x01;
pub const FLAG_RESET: u8 = 0x80;
/// Largest reassembled message (kept from the CM0 RAM guard so both devices behave alike).
pub const MAX_MESSAGE: usize = 16_384;
pub const REASSEMBLY_TIMEOUT: Duration = Duration::from_secs(5);
pub const DEFAULT_ATT_MTU: usize = 23;
/// Bluetooth caps an attribute value at 512 bytes whatever the MTU (Android enforces it).
pub const MAX_ATT_VALUE: usize = 512;

#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum FrameError {
    #[error("empty payload")]
    Empty,
    #[error("message exceeds 16 KiB")]
    TooLarge,
    #[error("message needs more than 255 chunks at this MTU")]
    TooManyChunks,
    #[error("short frame")]
    Short,
    #[error("invalid chunk header")]
    Header,
    #[error("chunk out of order")]
    Order,
}

/// `D_max = min(ATT_MTU - 3, 512) - 4`.
pub fn max_chunk_data(att_mtu: usize) -> usize {
    att_mtu
        .saturating_sub(3)
        .min(MAX_ATT_VALUE)
        .saturating_sub(HEADER_LEN)
        .max(1)
}

pub fn split(
    msg_id: u8,
    flags: u8,
    payload: &[u8],
    chunk: usize,
) -> Result<Vec<Vec<u8>>, FrameError> {
    if payload.is_empty() {
        return Err(FrameError::Empty);
    }
    if payload.len() > MAX_MESSAGE {
        return Err(FrameError::TooLarge);
    }
    let chunk = chunk.max(1);
    let total = payload.len().div_ceil(chunk);
    let total_u8 = u8::try_from(total).map_err(|_| FrameError::TooManyChunks)?;
    Ok(payload
        .chunks(chunk)
        .enumerate()
        .map(|(idx, data)| {
            let mut frame = Vec::with_capacity(HEADER_LEN + data.len());
            // idx < total <= 255, so the cast is exact.
            frame.extend_from_slice(&[msg_id, idx as u8, total_u8, flags]);
            frame.extend_from_slice(data);
            frame
        })
        .collect())
}

/// Transport abort: the peer drops whatever it was reassembling.
pub fn reset_frame(msg_id: u8) -> [u8; 4] {
    [msg_id, 0, 1, FLAG_RESET]
}

#[derive(Default)]
pub struct Reassembler {
    key: Option<(u8, u8, u8)>,
    buf: Vec<u8>,
    next: u8,
    started: Option<Instant>,
}

impl Reassembler {
    pub fn reset(&mut self) {
        self.key = None;
        self.buf.clear();
        self.next = 0;
        self.started = None;
    }

    /// `Ok(Some((flags, payload)))` when a message completes. On `Err` the partial buffer is
    /// already dropped; the caller answers with one [`reset_frame`]. A reset frame from the
    /// peer returns `Ok(None)`.
    pub fn feed(
        &mut self,
        frame: &[u8],
        now: Instant,
    ) -> Result<Option<(u8, Vec<u8>)>, FrameError> {
        if let Some(started) = self.started {
            if now.duration_since(started) > REASSEMBLY_TIMEOUT {
                self.reset();
            }
        }
        if frame.len() < HEADER_LEN {
            self.reset();
            return Err(FrameError::Short);
        }
        let (msg_id, idx, total, flags) = (frame[0], frame[1], frame[2], frame[3]);
        let data = &frame[HEADER_LEN..];
        if flags & FLAG_RESET != 0 {
            self.reset();
            return Ok(None);
        }
        if total < 1 || idx >= total || data.is_empty() || flags & !FLAG_ENCRYPTED != 0 {
            self.reset();
            return Err(FrameError::Header);
        }
        if idx == 0 {
            self.reset();
            self.key = Some((msg_id, total, flags));
            self.started = Some(now);
        } else if self.key != Some((msg_id, total, flags)) || idx != self.next {
            self.reset();
            return Err(FrameError::Order);
        }
        self.buf.extend_from_slice(data);
        self.next = idx + 1;
        if self.buf.len() > MAX_MESSAGE {
            self.reset();
            return Err(FrameError::TooLarge);
        }
        if self.next < total {
            return Ok(None);
        }
        let payload = std::mem::take(&mut self.buf);
        self.reset();
        Ok(Some((flags, payload)))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mtu_limits() {
        assert_eq!(max_chunk_data(23), 16);
        assert_eq!(max_chunk_data(185), 178);
        assert_eq!(max_chunk_data(517), 508);
        assert_eq!(max_chunk_data(0), 1);
    }

    #[test]
    fn round_trip_and_errors() {
        let payload: Vec<u8> = (0..=255u8).cycle().take(1000).collect();
        let frames = split(9, FLAG_ENCRYPTED, &payload, 16).unwrap();
        let mut rx = Reassembler::default();
        let now = Instant::now();
        let mut out = None;
        for f in &frames {
            out = rx.feed(f, now).unwrap();
        }
        assert_eq!(out, Some((FLAG_ENCRYPTED, payload.clone())));

        // Out of order: skip chunk 1.
        assert_eq!(rx.feed(&frames[0], now).unwrap(), None);
        assert_eq!(rx.feed(&frames[2], now), Err(FrameError::Order));
        // Peer reset drops the buffer.
        assert_eq!(rx.feed(&frames[0], now).unwrap(), None);
        assert_eq!(rx.feed(&reset_frame(9), now).unwrap(), None);
        assert_eq!(rx.feed(&frames[1], now), Err(FrameError::Order));
        // Timeout: a stale partial message is discarded.
        assert_eq!(rx.feed(&frames[0], now).unwrap(), None);
        let later = now + Duration::from_secs(6);
        assert_eq!(rx.feed(&frames[1], later), Err(FrameError::Order));
        // Reserved flag bits and oversize.
        assert_eq!(rx.feed(&[1, 0, 1, 0x02, 0], now), Err(FrameError::Header));
        assert_eq!(
            split(0, 0, &vec![0; MAX_MESSAGE + 1], 508),
            Err(FrameError::TooLarge)
        );
        assert_eq!(split(0, 0, &[0; 300], 1), Err(FrameError::TooManyChunks));
        assert_eq!(split(0, 0, &[], 16), Err(FrameError::Empty));
    }
}
