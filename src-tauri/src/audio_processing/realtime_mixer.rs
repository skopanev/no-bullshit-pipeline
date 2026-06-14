use crate::resampler_compat::{
    SincFixedIn, SincInterpolationParameters, SincInterpolationType, WindowFunction,
};
use anyhow::Result;
use std::collections::VecDeque;
use std::fs::File;
use std::num::{NonZeroU8, NonZeroU32};
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::thread;
use std::time::Duration;
use vorbis_rs::{VorbisBitrateManagementStrategy, VorbisEncoder, VorbisEncoderBuilder};

use super::shared_buffer::TRANSCRIPTION_BUFFER;
use super::{MIC_BUFFER, SYSTEM_BUFFER};

/// Adaptive gain controller for real-time level normalization
struct AdaptiveGain {
    /// Sliding window of squared samples for RMS calculation
    rms_window: VecDeque<f32>,
    /// Window size in samples (0.5 seconds at 48kHz)
    window_size: usize,
    /// Sum of squared samples in window (for efficient RMS)
    sum_sq: f64,
    /// Current smoothed gain
    current_gain: f32,
    /// Target RMS level (linear, not dB)
    target_rms: f32,
    /// Gain smoothing factor (0-1, higher = slower adaptation)
    smoothing: f32,
}

impl AdaptiveGain {
    fn new(sample_rate: u32) -> Self {
        const TARGET_RMS_DB: f32 = -20.0;
        const WINDOW_SECONDS: f32 = 0.3; // 300ms window - good balance

        Self {
            rms_window: VecDeque::new(),
            window_size: (sample_rate as f32 * WINDOW_SECONDS) as usize,
            sum_sq: 0.0,
            current_gain: 1.0,
            target_rms: 10_f32.powf(TARGET_RMS_DB / 20.0),
            smoothing: 0.9, // Fast adaptation (~100ms response time)
        }
    }

    /// Update RMS estimate with new samples and return current gain
    fn update(&mut self, samples: &[f32]) -> f32 {
        for &sample in samples {
            let sq = (sample * sample) as f64;

            // Add to window
            self.rms_window.push_back(sample * sample);
            self.sum_sq += sq;

            // Remove old samples if window is full
            while self.rms_window.len() > self.window_size {
                if let Some(old) = self.rms_window.pop_front() {
                    self.sum_sq -= old as f64;
                    // Prevent negative due to float errors
                    if self.sum_sq < 0.0 {
                        self.sum_sq = 0.0;
                    }
                }
            }
        }

        // Calculate current RMS
        if self.rms_window.is_empty() {
            return self.current_gain;
        }

        let current_rms = (self.sum_sq / self.rms_window.len() as f64).sqrt() as f32;

        // Chase the target only while there is real signal. During a silent gap
        // the RMS plummets and, without this gate, the controller cranks toward
        // the +12 dB ceiling — then blasts the first words of the next phrase
        // straight into soft_clip (a square wave), recreating exactly the
        // distortion the upstream EBU silence-gate was added to prevent. Hold the
        // last speech-adapted gain through silence instead. Sources are already
        // EBU-normalized to a fixed loudness upstream, so this stage only trims
        // residual level differences (it can still boost up to +12 dB).
        const SILENCE_GATE_RMS: f32 = 0.0056; // ≈ -45 dBFS RMS
        let target_gain = if current_rms > SILENCE_GATE_RMS {
            (self.target_rms / current_rms).clamp(0.1, 4.0)
        } else {
            self.current_gain // hold through silence — do not chase the noise floor
        };

        // Smooth gain changes to avoid clicks
        self.current_gain =
            self.current_gain * self.smoothing + target_gain * (1.0 - self.smoothing);

        self.current_gain
    }

    /// Get current gain without updating
    fn gain(&self) -> f32 {
        self.current_gain
    }
}

/// Real-time mixer that reads from shared buffers and writes mixed output
pub struct RealtimeMixer {
    should_stop: Arc<AtomicBool>,
    handle: Option<thread::JoinHandle<()>>,
}

impl RealtimeMixer {
    pub fn new(output_path: PathBuf) -> Result<Self> {
        // Clear buffers before starting
        MIC_BUFFER.clear();
        SYSTEM_BUFFER.clear();
        TRANSCRIPTION_BUFFER.clear();

        let should_stop = Arc::new(AtomicBool::new(false));
        let should_stop_clone = should_stop.clone();

        let handle = thread::spawn(move || {
            if let Err(e) = run_realtime_mixer(output_path, should_stop_clone) {
                eprintln!("Real-time mixer error: {:?}", e);
            }
        });

        Ok(Self {
            should_stop,
            handle: Some(handle),
        })
    }

    pub fn stop(&mut self) {
        self.should_stop.store(true, Ordering::Relaxed);
        if let Some(handle) = self.handle.take() {
            let _ = handle.join();
        }
    }
}

impl Drop for RealtimeMixer {
    fn drop(&mut self) {
        self.stop();
    }
}

fn run_realtime_mixer(output_path: PathBuf, should_stop: Arc<AtomicBool>) -> Result<()> {
    #[cfg(debug_assertions)]
    eprintln!("Real-time mixer: Starting with adaptive normalization");

    // Output format: 48kHz stereo
    let sample_rate = 48000u32;
    let channels = 2u8;

    let output_file = File::create(&output_path)?;
    let mut encoder = VorbisEncoderBuilder::new_with_serial(
        NonZeroU32::new(sample_rate).ok_or(anyhow::anyhow!("Invalid sample rate"))?,
        NonZeroU8::new(channels).ok_or(anyhow::anyhow!("Invalid channels"))?,
        output_file,
        0,
    )
    .bitrate_management_strategy(VorbisBitrateManagementStrategy::QualityVbr {
        target_quality: 0.5,
    })
    .build()?;

    // Adaptive gain controllers for each source
    let mut mic_gain = AdaptiveGain::new(sample_rate);
    let mut sys_gain = AdaptiveGain::new(sample_rate);

    // Transcription resampler: 48kHz mono → 16kHz mono
    let transcription_chunk_size = 1024usize;
    let transcription_params = SincInterpolationParameters {
        sinc_len: 256,
        f_cutoff: 0.95,
        interpolation: SincInterpolationType::Linear,
        oversampling_factor: 256,
        window: WindowFunction::BlackmanHarris2,
    };
    let mut transcription_resampler = SincFixedIn::<f32>::new(
        16000.0 / 48000.0,
        2.0,
        transcription_params,
        transcription_chunk_size,
        1, // mono
    )
    .map_err(|e| anyhow::anyhow!(e))?;
    let mut transcription_accum: Vec<f32> = Vec::with_capacity(transcription_chunk_size * 2);

    // Continuous timeline tracking (like mic/system recorders)
    let start_time = std::time::Instant::now();
    let mut total_frames_written: u64 = 0;

    // Tick every 10ms to maintain timeline
    let tick_duration = Duration::from_millis(10);

    while !should_stop.load(Ordering::Relaxed) {
        // Calculate how many frames SHOULD exist by now
        let elapsed_secs = start_time.elapsed().as_secs_f64();
        let expected_frames = (elapsed_secs * sample_rate as f64) as u64;

        // If we're behind, we need to write frames
        if total_frames_written < expected_frames {
            // Write up to 100ms at a time to avoid huge blocks
            let catch_up_limit = (sample_rate as f64 * 0.1) as usize;
            let frames_needed =
                (expected_frames - total_frames_written).min(catch_up_limit as u64) as usize;

            let mic_avail = MIC_BUFFER.available();
            let sys_avail = SYSTEM_BUFFER.available();

            let mut frames_remaining = frames_needed;

            // 1. Process available audio from buffers
            let audio_frames_available = mic_avail.max(sys_avail);

            if audio_frames_available > 0 {
                let frames_to_process = audio_frames_available.min(frames_remaining);

                // Pop from both buffers
                let (mic_left, mic_right) = MIC_BUFFER.pop(frames_to_process);
                let (sys_left, sys_right) = SYSTEM_BUFFER.pop(frames_to_process);

                // Mix with adaptive normalization
                let frame_count = mic_left.len().max(sys_left.len());
                if frame_count > 0 {
                    // Update gain controllers with mono mix of each source
                    let mic_mono: Vec<f32> = (0..mic_left.len())
                        .map(|i| {
                            (mic_left[i] + mic_right.get(i).copied().unwrap_or(mic_left[i])) * 0.5
                        })
                        .collect();
                    let sys_mono: Vec<f32> = (0..sys_left.len())
                        .map(|i| {
                            (sys_left[i] + sys_right.get(i).copied().unwrap_or(sys_left[i])) * 0.5
                        })
                        .collect();

                    let mg = mic_gain.update(&mic_mono);
                    let sg = sys_gain.update(&sys_mono);

                    let mut mixed_left = Vec::with_capacity(frame_count);
                    let mut mixed_right = Vec::with_capacity(frame_count);

                    for i in 0..frame_count {
                        let ml = mic_left.get(i).copied().unwrap_or(0.0) * mg;
                        let mr = mic_right.get(i).copied().unwrap_or(0.0) * mg;
                        let sl = sys_left.get(i).copied().unwrap_or(0.0) * sg;
                        let sr = sys_right.get(i).copied().unwrap_or(0.0) * sg;

                        // Mix with soft clipping at full level (the transcription
                        // tap below reads this; headroom is applied afterwards).
                        mixed_left.push(soft_clip(ml + sl));
                        mixed_right.push(soft_clip(mr + sr));
                    }

                    // Tap the full-level mix to transcription (48kHz stereo → 16kHz
                    // mono). Live ASR does not go through Vorbis, so it must NOT be
                    // attenuated by the encoder headroom.
                    feed_transcription_resampler(
                        &mixed_left,
                        &mixed_right,
                        &mut transcription_resampler,
                        &mut transcription_accum,
                        transcription_chunk_size,
                    );

                    // Apply encoder headroom to the Vorbis-bound samples only.
                    apply_headroom(&mut mixed_left);
                    apply_headroom(&mut mixed_right);

                    let slices: Vec<&[f32]> = vec![&mixed_left, &mixed_right];
                    encoder.encode_audio_block(&slices)?;
                    total_frames_written += frame_count as u64;

                    if frames_remaining >= frame_count {
                        frames_remaining -= frame_count;
                    } else {
                        frames_remaining = 0;
                    }
                }
            }

            // 2. Fill remainder with silence to maintain timeline
            if frames_remaining > 0 {
                let silence = vec![0.0f32; frames_remaining];
                feed_transcription_resampler(
                    &silence,
                    &silence,
                    &mut transcription_resampler,
                    &mut transcription_accum,
                    transcription_chunk_size,
                );
                let slices: Vec<&[f32]> = vec![&silence, &silence];
                encoder.encode_audio_block(&slices)?;
                total_frames_written += frames_remaining as u64;
            }

            // Continue immediately if we still need to catch up
            if total_frames_written < expected_frames {
                continue;
            }
        }

        // Sleep until next tick if caught up
        thread::sleep(tick_duration);
    }

    // Drain remaining samples from buffers (use last known gains)
    drain_and_encode(
        &mut encoder,
        4096,
        mic_gain.gain(),
        sys_gain.gain(),
        &mut transcription_resampler,
        &mut transcription_accum,
        transcription_chunk_size,
    )?;

    // Final padding to match wall-clock duration
    let elapsed = start_time.elapsed().as_secs_f64();
    let expected_frames = (elapsed * sample_rate as f64) as u64;

    if total_frames_written < expected_frames {
        let missing_frames = expected_frames - total_frames_written;
        let silence_chunk = 4096;
        let silence = vec![0.0f32; silence_chunk];

        let mut remaining = missing_frames;
        while remaining > 0 {
            let chunk = remaining.min(silence_chunk as u64) as usize;
            feed_transcription_resampler(
                &silence[..chunk],
                &silence[..chunk],
                &mut transcription_resampler,
                &mut transcription_accum,
                transcription_chunk_size,
            );
            let slices: Vec<&[f32]> = vec![&silence[..chunk], &silence[..chunk]];
            encoder.encode_audio_block(&slices)?;
            remaining -= chunk as u64;
        }
    }

    // Flush remaining transcription resampler buffer and drain sinc filter delay
    // (must happen after all audio including silence padding has been fed)
    flush_transcription_resampler(&mut transcription_resampler, &mut transcription_accum);

    encoder.finish()?;
    #[cfg(debug_assertions)]
    eprintln!(
        "Real-time mixer: Finished. Wrote {} frames ({:.2}s)",
        total_frames_written,
        total_frames_written as f64 / sample_rate as f64
    );
    Ok(())
}

/// Drain remaining samples from buffers with given gains
fn drain_and_encode(
    encoder: &mut VorbisEncoder<File>,
    block_size: usize,
    mg: f32,
    sg: f32,
    transcription_resampler: &mut SincFixedIn<f32>,
    transcription_accum: &mut Vec<f32>,
    transcription_chunk_size: usize,
) -> Result<()> {
    loop {
        let mic_avail = MIC_BUFFER.available();
        let sys_avail = SYSTEM_BUFFER.available();

        if mic_avail == 0 && sys_avail == 0 {
            break;
        }

        let process_count = mic_avail.max(sys_avail).min(block_size);
        let (mic_left, mic_right) = MIC_BUFFER.pop(process_count);
        let (sys_left, sys_right) = SYSTEM_BUFFER.pop(process_count);

        let frame_count = mic_left.len().max(sys_left.len());
        if frame_count == 0 {
            break;
        }

        let mut mixed_left = Vec::with_capacity(frame_count);
        let mut mixed_right = Vec::with_capacity(frame_count);

        for i in 0..frame_count {
            let ml = mic_left.get(i).copied().unwrap_or(0.0) * mg;
            let mr = mic_right.get(i).copied().unwrap_or(0.0) * mg;
            let sl = sys_left.get(i).copied().unwrap_or(0.0) * sg;
            let sr = sys_right.get(i).copied().unwrap_or(0.0) * sg;

            mixed_left.push(soft_clip(ml + sl));
            mixed_right.push(soft_clip(mr + sr));
        }

        feed_transcription_resampler(
            &mixed_left,
            &mixed_right,
            transcription_resampler,
            transcription_accum,
            transcription_chunk_size,
        );

        // Encoder headroom on the Vorbis-bound samples only (see main loop).
        apply_headroom(&mut mixed_left);
        apply_headroom(&mut mixed_right);

        let slices: Vec<&[f32]> = vec![&mixed_left, &mixed_right];
        encoder.encode_audio_block(&slices)?;
    }
    Ok(())
}

/// Downsample stereo mixed audio to 16kHz mono and push to TRANSCRIPTION_BUFFER
fn feed_transcription_resampler(
    left: &[f32],
    right: &[f32],
    resampler: &mut SincFixedIn<f32>,
    accum: &mut Vec<f32>,
    chunk_size: usize,
) {
    let frame_count = left.len().min(right.len());
    for i in 0..frame_count {
        accum.push((left[i] + right[i]) * 0.5);
    }
    while accum.len() >= chunk_size {
        let chunk: Vec<f32> = accum.drain(..chunk_size).collect();
        if let Ok(resampled) = resampler.process(&[chunk], None)
            && !resampled[0].is_empty()
        {
            TRANSCRIPTION_BUFFER.push(&resampled[0]);
        }
    }
}

/// Flush remaining samples in the transcription resampler accumulation buffer
/// and drain the resampler's internal sinc filter delay
fn flush_transcription_resampler(resampler: &mut SincFixedIn<f32>, accum: &mut Vec<f32>) {
    // Process any remaining accumulated samples via process_partial which
    // handles zero-padding internally for sub-chunk input
    if !accum.is_empty() {
        let partial: Vec<Vec<f32>> = vec![std::mem::take(accum)];
        if let Ok(resampled) = resampler.process_partial(Some(&partial), None)
            && !resampled[0].is_empty()
        {
            TRANSCRIPTION_BUFFER.push(&resampled[0]);
        }
    }

    // Drain the resampler's internal sinc filter delay.
    // process_partial(None) feeds zero-padded input to push any remaining
    // delayed frames out. One call is sufficient since the delay is bounded
    // by sinc_len and fits within a single output chunk.
    if let Ok(resampled) = resampler.process_partial(None::<&[Vec<f32>]>, None)
        && !resampled[0].is_empty()
    {
        TRANSCRIPTION_BUFFER.push(&resampled[0]);
    }
}

/// Headroom applied to the Vorbis-bound mix only (−6 dB).
/// `soft_clip` (tanh) bounds the encoder input to ±1.0, but the Vorbis MDCT
/// round-trip reconstructs decoded peaks above that bound. Without headroom
/// those overshoot samples hard-clip when the OGG is later decoded to i16 for
/// transcription/diarization. −6 dB keeps the decoded peak well under 0 dBFS,
/// absorbing both MDCT overshoot and the inter-sample peaks the upstream
/// (sample-peak) limiter does not catch. The lost loudness is recoverable at
/// playback and is irrelevant to the internally-normalizing diarizer.
/// NOTE: applied via `apply_headroom` to the encoder slices ONLY — the live
/// transcription tap reads the full-level mix (it never goes through Vorbis).
const ENCODER_HEADROOM: f32 = 0.5;

/// Scale samples in place by the encoder headroom. Applied to the Vorbis-bound
/// buffers after the full-level mix has been tapped for live transcription.
#[inline]
fn apply_headroom(samples: &mut [f32]) {
    for s in samples.iter_mut() {
        *s *= ENCODER_HEADROOM;
    }
}

/// Soft clipping to prevent harsh distortion using tanh.
/// Consistent with offline mixer approach (mixer.rs).
/// tanh provides smooth, continuous clipping with no discontinuity:
/// - Near-linear passthrough for small values (tanh(x) ≈ x when |x| << 1)
/// - Smooth saturation toward ±1.0 for large values
#[inline]
fn soft_clip(x: f32) -> f32 {
    (x as f64).tanh() as f32
}
