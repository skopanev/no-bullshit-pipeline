//! Shared lock-free microphone capture primitive.
//!
//! Both the on-disk recording engine (`mic_audio`) and in-memory Quick Dictate
//! (`dictation`) capture the mic the same way: a cpal input stream whose
//! real-time callback converts the device's sample format to `f32` and pushes
//! into a lock-free ring buffer. What happens on the *consumer* side differs —
//! 48 kHz EBU normalize + OGG to disk for recordings, vs. raw samples drained
//! into RAM and resampled to 16 kHz mono for ASR in dictation — so that, and
//! device/config selection, stays with each caller. This module owns the one
//! piece that must be identical and allocation-/lock-free on the audio thread:
//! the stream construction and the format → f32 → ring hand-off.

use cpal::traits::{DeviceTrait, StreamTrait};
use ringbuf::HeapProd;
use ringbuf::traits::Producer;

/// Generous upper bound for a single callback's frame count (device buffers are
/// a few thousand frames at most). The i16/u16 scratch is reserved to this once
/// and only ever `clear()`ed, so the audio callback never hits the allocator.
const SCRATCH_SAMPLES: usize = 16_384;

/// `cpal::Stream` is `!Send` on macOS (it holds a `PhantomData<*mut ()>`). We
/// only ever move it between threads as an opaque keep-alive handle and drop it
/// on stop — its internals are never touched off the creating thread.
pub struct SendStream(pub cpal::Stream);

// SAFETY: see the type doc — the stream is only moved / stored / dropped, never
// used concurrently from two threads.
unsafe impl Send for SendStream {}

/// Build and start a cpal input stream on `device` / `config` that pushes every
/// captured sample (converted to `f32`) into `producer`. The returned stream is
/// already `play()`ing.
///
/// Real-time safety: the F32 path is a straight `push_slice` — no allocation, no
/// lock. The I16/U16 paths convert into a per-stream scratch buffer (allocated
/// once, only ever `clear()`ed) in `SCRATCH_SAMPLES`-sized chunks, so even an
/// oversized callback buffer (e.g. a 192 kHz card handing us 100 ms+ after a
/// wake) can't grow it — `extend` never reallocs on the audio thread. If the
/// consumer falls behind and the ring fills, `push_slice` drops the *newest*
/// samples that don't fit rather than blocking the audio thread.
pub fn build_input_stream(
    device: &cpal::Device,
    config: cpal::SupportedStreamConfig,
    mut producer: HeapProd<f32>,
) -> Result<SendStream, String> {
    let err_fn = |err| log::error!("audio capture stream error: {}", err);

    let stream = match config.sample_format() {
        cpal::SampleFormat::F32 => device.build_input_stream(
            config.into(),
            move |data: &[f32], _: &_| {
                let _ = producer.push_slice(data);
            },
            err_fn,
            None,
        ),
        cpal::SampleFormat::I16 => {
            let mut scratch: Vec<f32> = Vec::with_capacity(SCRATCH_SAMPLES);
            device.build_input_stream(
                config.into(),
                move |data: &[i16], _: &_| {
                    // Convert in scratch-sized chunks so `extend` can never grow
                    // the scratch past its reserved capacity (= realloc on the
                    // audio thread) no matter how large the callback buffer is.
                    for chunk in data.chunks(SCRATCH_SAMPLES) {
                        scratch.clear();
                        scratch.extend(chunk.iter().map(|&s| s as f32 / i16::MAX as f32));
                        let _ = producer.push_slice(&scratch);
                    }
                },
                err_fn,
                None,
            )
        }
        cpal::SampleFormat::U16 => {
            let mut scratch: Vec<f32> = Vec::with_capacity(SCRATCH_SAMPLES);
            device.build_input_stream(
                config.into(),
                move |data: &[u16], _: &_| {
                    for chunk in data.chunks(SCRATCH_SAMPLES) {
                        scratch.clear();
                        scratch.extend(chunk.iter().map(|&s| {
                            (s as f32 - u16::MAX as f32 / 2.0) / (u16::MAX as f32 / 2.0)
                        }));
                        let _ = producer.push_slice(&scratch);
                    }
                },
                err_fn,
                None,
            )
        }
        _ => return Err("Unsupported sample format".into()),
    }
    .map_err(|e| format!("build_input_stream: {}", e))?;

    stream.play().map_err(|e| format!("stream.play: {}", e))?;
    Ok(SendStream(stream))
}
