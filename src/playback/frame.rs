use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

pub const MAX_IN_FLIGHT_FRAMES: usize = 3;
pub const MAX_FRAME_BYTES: usize = 64 * 1024 * 1024;
const FRAME_ALIGNMENT: usize = 64;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct FrameLayout {
    pub width: u32,
    pub height: u32,
    pub pitch: usize,
    byte_len: usize,
    packed_len: usize,
}

impl FrameLayout {
    pub fn new(width: u32, height: u32, pitch: usize) -> Option<Self> {
        if width == 0 || height == 0 || width > 8192 || height > 8192 {
            return None;
        }
        let row_len = (width as usize).checked_mul(4)?;
        if pitch < row_len || pitch & 3 != 0 {
            return None;
        }
        let byte_len = pitch.checked_mul(height as usize)?;
        let packed_len = row_len.checked_mul(height as usize)?;
        let aligned_len = byte_len
            .checked_add(FRAME_ALIGNMENT - 1)?
            .checked_div(FRAME_ALIGNMENT)?
            .checked_mul(FRAME_ALIGNMENT)?;
        (byte_len < MAX_FRAME_BYTES
            && aligned_len < MAX_FRAME_BYTES
            && packed_len < MAX_FRAME_BYTES)
            .then_some(Self {
                width,
                height,
                pitch,
                byte_len,
                packed_len,
            })
    }
}

#[derive(Debug)]
pub struct VideoFrame {
    pub width: u32,
    pub height: u32,
    pub pitch: usize,
    pub pixels: Vec<u8>,
    generation: u64,
}

impl VideoFrame {
    pub fn packed_rgba(&self) -> Option<Vec<u8>> {
        let layout = FrameLayout::new(self.width, self.height, self.pitch)?;
        if self.pixels.len() < layout.byte_len {
            return None;
        }
        let row_len = self.width as usize * 4;
        let mut pixels = Vec::new();
        pixels.try_reserve_exact(layout.packed_len).ok()?;
        for row in self
            .pixels
            .chunks_exact(self.pitch)
            .take(self.height as usize)
        {
            pixels.extend_from_slice(&row[..row_len]);
        }
        Some(pixels)
    }
}

#[derive(Default)]
pub struct FrameMailbox {
    generation: AtomicU64,
    in_flight: AtomicUsize,
    latest: Mutex<Option<VideoFrame>>,
}

impl FrameMailbox {
    pub fn reset(&self) -> u64 {
        let generation = self
            .generation
            .fetch_add(1, Ordering::AcqRel)
            .wrapping_add(1);
        self.lock_latest().take();
        generation
    }

    pub fn generation(&self) -> u64 {
        self.generation.load(Ordering::Acquire)
    }

    pub fn take_latest(&self) -> Option<VideoFrame> {
        self.lock_latest().take()
    }

    pub fn in_flight(&self) -> usize {
        self.in_flight.load(Ordering::Acquire)
    }

    pub(crate) fn allocate(
        self: &Arc<Self>,
        layout: FrameLayout,
        generation: u64,
    ) -> Option<CallbackFrame> {
        self.in_flight
            .fetch_update(Ordering::AcqRel, Ordering::Acquire, |count| {
                (count < MAX_IN_FLIGHT_FRAMES).then_some(count + 1)
            })
            .ok()?;
        let permit = FramePermit(Arc::clone(self));
        let Some(pixels) = AlignedBuffer::new(layout.byte_len) else {
            drop(permit);
            return None;
        };
        Some(CallbackFrame {
            layout,
            generation,
            pixels,
            _permit: permit,
        })
    }

    pub(crate) fn publish(&self, frame: VideoFrame) {
        let mut latest = self.lock_latest();
        if frame.generation == self.generation() {
            *latest = Some(frame);
        }
    }

    fn lock_latest(&self) -> std::sync::MutexGuard<'_, Option<VideoFrame>> {
        self.latest
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }
}

pub(crate) struct CallbackFrame {
    layout: FrameLayout,
    generation: u64,
    pixels: AlignedBuffer,
    _permit: FramePermit,
}

impl CallbackFrame {
    pub(crate) fn plane(&mut self) -> *mut u8 {
        self.pixels.as_mut_ptr()
    }

    pub(crate) fn set_generation(&mut self, generation: u64) {
        self.generation = generation;
    }

    pub(crate) fn copy_video_frame(&self) -> Option<VideoFrame> {
        Some(VideoFrame {
            width: self.layout.width,
            height: self.layout.height,
            pitch: self.layout.pitch,
            pixels: self.pixels.copy_to_vec(self.layout.byte_len)?,
            generation: self.generation,
        })
    }
}

#[repr(align(64))]
struct AlignedBlock([u8; FRAME_ALIGNMENT]);

struct AlignedBuffer {
    blocks: Vec<AlignedBlock>,
}

impl AlignedBuffer {
    fn new(byte_len: usize) -> Option<Self> {
        let block_count = byte_len
            .checked_add(FRAME_ALIGNMENT - 1)?
            .checked_div(FRAME_ALIGNMENT)?;
        let mut blocks = Vec::new();
        blocks.try_reserve_exact(block_count).ok()?;
        blocks.resize_with(block_count, || AlignedBlock([0; FRAME_ALIGNMENT]));
        Some(Self { blocks })
    }

    fn as_mut_ptr(&mut self) -> *mut u8 {
        self.blocks.as_mut_ptr().cast::<u8>()
    }

    fn copy_to_vec(&self, byte_len: usize) -> Option<Vec<u8>> {
        let mut pixels = Vec::new();
        pixels.try_reserve_exact(byte_len).ok()?;
        let mut remaining = byte_len;
        for block in &self.blocks {
            let length = remaining.min(FRAME_ALIGNMENT);
            pixels.extend_from_slice(&block.0[..length]);
            remaining -= length;
            if remaining == 0 {
                break;
            }
        }
        (remaining == 0).then_some(pixels)
    }
}

struct FramePermit(Arc<FrameMailbox>);

impl Drop for FramePermit {
    fn drop(&mut self) {
        self.0.in_flight.fetch_sub(1, Ordering::AcqRel);
    }
}

#[cfg(test)]
mod tests {
    use super::{FrameLayout, FrameMailbox, VideoFrame, MAX_IN_FLIGHT_FRAMES};
    use std::sync::Arc;

    #[test]
    fn validates_bounds_and_copies_rows_without_pitch_padding() {
        assert!(FrameLayout::new(0, 2, 8).is_none());
        assert!(FrameLayout::new(2, 2, 4).is_none());
        assert!(FrameLayout::new(u32::MAX, 2, usize::MAX).is_none());
        assert!(FrameLayout::new(4096, 4096, 4096 * 4).is_none());
        assert!(FrameLayout::new(4096, 4095, 4096 * 4).is_some());
        assert!(FrameLayout::new(8192, 8192, 8192 * 4).is_none());

        let frame = VideoFrame {
            width: 1,
            height: 2,
            pitch: 8,
            pixels: vec![
                255, 0, 0, 255, 99, 99, 99, 99, 0, 255, 0, 255, 88, 88, 88, 88,
            ],
            generation: 0,
        };
        assert_eq!(
            frame.packed_rgba().unwrap(),
            [255, 0, 0, 255, 0, 255, 0, 255]
        );
    }

    #[test]
    fn mailbox_is_bounded_and_drops_stale_frames() {
        let mailbox = Arc::new(FrameMailbox::default());
        let layout = FrameLayout::new(1, 1, 4).unwrap();
        let generation = mailbox.generation();

        let mut held = Vec::new();
        for _ in 0..MAX_IN_FLIGHT_FRAMES {
            held.push(mailbox.allocate(layout, generation).unwrap());
        }
        assert!(mailbox.allocate(layout, generation).is_none());
        assert_eq!(mailbox.in_flight(), MAX_IN_FLIGHT_FRAMES);
        drop(held);
        assert_eq!(mailbox.in_flight(), 0);

        let first = mailbox.allocate(layout, generation).unwrap();
        let second = mailbox.allocate(layout, generation).unwrap();
        let first = first.copy_video_frame().unwrap();
        let second = second.copy_video_frame().unwrap();
        mailbox.publish(first);
        mailbox.publish(second);
        assert_eq!(mailbox.take_latest().unwrap().generation, generation);
        assert!(mailbox.take_latest().is_none());

        let stale = mailbox
            .allocate(layout, generation)
            .unwrap()
            .copy_video_frame()
            .unwrap();
        mailbox.reset();
        mailbox.publish(stale);
        assert!(mailbox.take_latest().is_none());
    }

    #[test]
    fn rejects_frames_at_or_above_the_callback_allocation_cap() {
        assert!(FrameLayout::new(4096, 4096, 4096 * 8).is_none());
        assert!(FrameLayout::new(4096, 4096, 4096 * 4).is_none());
        assert!(FrameLayout::new(4096, 4095, 4096 * 4).is_some());
    }

    #[test]
    fn callback_plane_has_at_least_64_byte_alignment() {
        let mailbox = Arc::new(FrameMailbox::default());
        let layout = FrameLayout::new(17, 9, 128).unwrap();
        let mut frame = mailbox.allocate(layout, mailbox.generation()).unwrap();
        assert_eq!(frame.plane() as usize % 64, 0);
    }
}
