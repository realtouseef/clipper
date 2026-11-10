//! The one monotonic clock every capture timestamp comes from: the macOS host clock
//! (`mach_absolute_time`, i.e. `CLOCK_UPTIME_RAW`), in nanoseconds since boot.
//!
//! - Screen frames: AVFoundation stamps each sample buffer on the host clock; ffmpeg runs
//!   with `-copyts`, so every frame's PTS in `raw.mp4` is a host-clock time.
//! - Mic chunks: CoreAudio's `mHostTime` (exposed by cpal as `StreamInstant`) per buffer.
//! - Cursor samples: read with [`now`].
//!
//! Because they share one clock, the tracks line up by subtracting the first video frame's time.

/// Nanoseconds on the host clock.
pub type Nanos = u64;

const CLOCK_UPTIME_RAW: u32 = 8;

unsafe extern "C" {
    fn clock_gettime_nsec_np(clock_id: u32) -> u64;
}

pub fn now() -> Nanos {
    unsafe { clock_gettime_nsec_np(CLOCK_UPTIME_RAW) }
}
