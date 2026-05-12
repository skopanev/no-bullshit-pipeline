use anyhow::Result;
use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use ringbuf::{
    traits::{Consumer, Producer, Split, Observer},
    HeapRb,
};
use rubato::{SincFixedIn, SincInterpolationParameters, SincInterpolationType, WindowFunction, Resampler};
use std::fs::File;
use std::num::{NonZeroU32, NonZeroU8};
use std::sync::{
    atomic::{AtomicBool, Ordering},
    Arc,
};
use std::thread;
use vorbis_rs::{VorbisBitrateManagementStrategy, VorbisEncoderBuilder};

use crate::audio_processing::LoudnessNormalizer;

/// Standard output sample rate for real-time mixing
const MIXER_SAMPLE_RATE: u32 = 48000;

// Shared audio level for real-time visualization (0.0 - 1.0)
lazy_static::lazy_static! {
    static ref CURRENT_AUDIO_LEVEL: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(0);
}

/// Set the current audio level (0.0 - 1.0). Used by both the main mic-capture
/// pipeline and the Quick Dictate ephemeral stream so the UI meter reads from
/// a single source of truth.
pub(crate) fn set_audio_level(level: f32) {
    let level_u32 = level.clamp(0.0, 1.0).to_bits();
    CURRENT_AUDIO_LEVEL.store(level_u32, Ordering::Relaxed);
}

/// Get the current audio level (0.0 - 1.0)
pub fn get_current_audio_level() -> f32 {
    f32::from_bits(CURRENT_AUDIO_LEVEL.load(Ordering::Relaxed))
}

/// Reset audio level to 0
pub fn reset_audio_level() {
    CURRENT_AUDIO_LEVEL.store(0, Ordering::Relaxed);
}

/// Calculate RMS level from samples
fn calculate_rms(samples: &[f32]) -> f32 {
    if samples.is_empty() {
        return 0.0;
    }
    let sum_sq: f32 = samples.iter().map(|s| s * s).sum();
    (sum_sq / samples.len() as f32).sqrt()
}

pub struct MicAudioRecorder {
    should_stop: Arc<AtomicBool>,
    processing_thread: Option<thread::JoinHandle<()>>,
    // Stream must be kept alive to keep recording
    stream: Option<cpal::Stream>,
}

// SAFETY: MicAudioRecorder is stored inside Mutex<Option<...>> in AudioState.
// The cpal::Stream it holds is !Send due to platform-level PhantomData<*mut ()>,
// but the stream is only created, held alive, and dropped -- never sent or
// accessed from another thread. The Mutex serialises all access.
unsafe impl Send for MicAudioRecorder {}

impl MicAudioRecorder {
    pub fn new(output_path: std::path::PathBuf, device_name: Option<String>, skip_file: bool) -> Result<Self> {
        let host = cpal::default_host();

        // Use selected device if specified, otherwise fall back to default
        let device = if let Some(ref name) = device_name {
            crate::devices::get_device_by_name(name)
                .or_else(|| {
                    eprintln!("Device '{}' not found, using default", name);
                    host.default_input_device()
                })
                .ok_or(anyhow::anyhow!("No input device available"))?
        } else {
            host.default_input_device()
                .ok_or(anyhow::anyhow!("No input device available"))?
        };
        
        // Use device's default config (native sample rate)
        let config = device.default_input_config()?;
        let sample_rate = config.sample_rate().0;
        let channels = config.channels();
        
        #[cfg(debug_assertions)]
        eprintln!("Mic Config: Rate={}, Channels={}", sample_rate, channels);

        // Ring Buffer (1 second capacity is usually enough for processing thread to catch up)
        // 48k * 2ch * 4bytes = ~384KB.
        let ring_buffer_size = (sample_rate as usize) * (channels as usize) * 8; // generous buffer
        let rb = HeapRb::<f32>::new(ring_buffer_size);
        let (mut producer, consumer) = rb.split();

        // Error callback
        let err_fn = move |err| {
            eprintln!("an error occurred on stream: {}", err);
        };

        // Build stream with appropriate format handling
        let stream = match config.sample_format() {
            cpal::SampleFormat::F32 => device.build_input_stream(
                &config.into(),
                move |data: &[f32], _: &_| {
                    let _ = producer.push_slice(data);
                },
                err_fn,
                None
            )?,
            cpal::SampleFormat::I16 => device.build_input_stream(
                &config.into(),
                move |data: &[i16], _: &_| {
                    for &sample in data {
                        let s = (sample as f32) / (i16::MAX as f32);
                        let _ = producer.push_slice(&[s]);
                    }
                },
                err_fn,
                None
            )?,
            cpal::SampleFormat::U16 => device.build_input_stream(
                &config.into(),
                move |data: &[u16], _: &_| {
                    for &sample in data {
                        let s = (sample as f32 - u16::MAX as f32 / 2.0) / (u16::MAX as f32 / 2.0);
                        let _ = producer.push_slice(&[s]);
                    }
                },
                err_fn,
                None
            )?,
            _ => return Err(anyhow::anyhow!("Unsupported sample format")),
        };

        stream.play()?;

        // Processing Thread
        let should_stop = Arc::new(AtomicBool::new(false));
         let should_stop_clone = should_stop.clone();
        
        let path = output_path.clone();

        let handle = thread::spawn(move || {
            if let Err(e) = run_audio_processing(
                path,
                should_stop_clone,
                consumer,
                channels as u32,
                sample_rate,
                skip_file
            ) {
                eprintln!("Mic audio processing error: {:?}", e);
            }
        });

        Ok(Self {
            should_stop,
            processing_thread: Some(handle),
            stream: Some(stream),
        })
    }

    pub fn stop(&mut self) {
        // Stop the stream FIRST to prevent more audio from being captured
        drop(self.stream.take());
        
        // Then signal encoder to stop and wait for it to finish
        self.should_stop.store(true, Ordering::Relaxed);
        if let Some(handle) = self.processing_thread.take() {
            let _ = handle.join();
        }
    }
}

pub fn start_mic_capture(output_path: std::path::PathBuf, device_name: Option<String>, skip_file: bool) -> Result<MicAudioRecorder> {
    MicAudioRecorder::new(output_path, device_name, skip_file)
}

/// Convert interleaved audio samples to planar stereo format.
///
/// Handles mono (duplicate to both channels), stereo (de-interleave),
/// and multi-channel (take first two channels) input.
fn to_planar_stereo(samples: &[f32], input_channels: u32) -> Vec<Vec<f32>> {
    let frames = samples.len() / input_channels as usize;
    if input_channels == 1 {
        // Mono -> Stereo: duplicate to both channels
        let ch_data = samples.to_vec();
        vec![ch_data.clone(), ch_data]
    } else if input_channels == 2 {
        // Stereo -> Stereo: de-interleave
        let mut left = Vec::with_capacity(frames);
        let mut right = Vec::with_capacity(frames);
        for chunk in samples.chunks(2) {
            if chunk.len() == 2 {
                left.push(chunk[0]);
                right.push(chunk[1]);
            }
        }
        vec![left, right]
    } else {
        // Multi-channel -> Stereo: take first two channels
        let mut left = Vec::with_capacity(frames);
        let mut right = Vec::with_capacity(frames);
        for chunk in samples.chunks(input_channels as usize) {
            if chunk.len() >= 2 {
                left.push(chunk[0]);
                right.push(chunk[1]);
            }
        }
        vec![left, right]
    }
}

fn run_audio_processing(
    path: std::path::PathBuf,
    should_stop: Arc<AtomicBool>,
    mut consumer: ringbuf::HeapCons<f32>,
    input_channels: u32,
    sample_rate: u32,
    skip_file: bool,
) -> Result<()> {
    // We always output Stereo 48kHz (or input sample rate)
    // If input is Mono, we duplicate.
    // If input is Stereo, we pass through.

    let output_channels = 2; // Target Stereo

    // Setup Normalizer
    let mut normalizer = LoudnessNormalizer::new(input_channels, sample_rate)?;

    // Setup resampler if needed (for real-time mixer which expects 48kHz)
    let needs_resampling = sample_rate != MIXER_SAMPLE_RATE;
    let resampler_chunk_size = 1024usize;
    let mut resampler: Option<SincFixedIn<f32>> = if needs_resampling {
        #[cfg(debug_assertions)]
        eprintln!("Mic: Resampling from {}Hz to {}Hz for real-time mixer", sample_rate, MIXER_SAMPLE_RATE);
        let params = SincInterpolationParameters {
            sinc_len: 256,
            f_cutoff: 0.95,
            interpolation: SincInterpolationType::Linear,
            oversampling_factor: 256,
            window: WindowFunction::BlackmanHarris2,
        };
        let resample_ratio = MIXER_SAMPLE_RATE as f64 / sample_rate as f64;
        Some(SincFixedIn::<f32>::new(
            resample_ratio,
            2.0,
            params,
            resampler_chunk_size,
            2,    // stereo output
        )?)
    } else {
        None
    };

    // Buffer to accumulate frames for resampler (needs exactly chunk_size frames)
    let mut resample_buffer_left: Vec<f32> = Vec::with_capacity(resampler_chunk_size * 2);
    let mut resample_buffer_right: Vec<f32> = Vec::with_capacity(resampler_chunk_size * 2);

    // Setup Encoder (only if not skipping file output)
    let mut encoder = if !skip_file {
        let output_file = File::create(&path)?;
        Some(VorbisEncoderBuilder::new_with_serial(
            NonZeroU32::new(sample_rate).ok_or(anyhow::anyhow!("Invalid sample rate"))?,
            NonZeroU8::new(output_channels).ok_or(anyhow::anyhow!("Invalid channels"))?,
            output_file,
            0,
        )
        .bitrate_management_strategy(VorbisBitrateManagementStrategy::QualityVbr { target_quality: 0.4 })
        .build()?)
    } else {
        #[cfg(debug_assertions)]
        eprintln!("Mic: Skipping file output (mix-only mode)");
        None
    };

    let chunk_size = 4096;
    let mut buffer = vec![0.0f32; chunk_size];
    
    // Continuous Timeline tracking
    let start_time = std::time::Instant::now();
    let mut total_frames_written: u64 = 0;
    
    // Tick every 10ms
    let tick_duration = std::time::Duration::from_millis(10);

    while !should_stop.load(Ordering::Relaxed) {
        // Calculate expected frames
        let elapsed_secs = start_time.elapsed().as_secs_f64();
        let expected_frames = (elapsed_secs * sample_rate as f64) as u64;
        
        if total_frames_written < expected_frames {
            // Write UP TO 100ms to catch up
            let catch_up_limit = (sample_rate as f64 * 0.1) as usize; 
            let frames_needed = (expected_frames - total_frames_written).min(catch_up_limit as u64) as usize;
            
            let available = consumer.occupied_len();
            let mut frames_remaining = frames_needed;
            
            // 1. Consume available audio
            let audio_frames_available = available / input_channels as usize;
            
            if audio_frames_available > 0 {
                let max_frames_in_buffer = chunk_size / input_channels as usize;
                let frames_to_read = audio_frames_available.min(frames_remaining).min(max_frames_in_buffer);
                let samples_to_read = frames_to_read * input_channels as usize;
                
                let n = consumer.pop_slice(&mut buffer[..samples_to_read]);
                
                if n > 0 {
                    let raw_samples = &buffer[..n];

                    // Update audio level for visualization (dynamic scaling)
                    let rms = calculate_rms(raw_samples);
                    // Soft compression: sqrt provides natural curve that boosts quiet signals
                    // while preventing loud signals from clipping
                    let scaled = (rms * 4.0).sqrt().min(1.0);
                    set_audio_level(scaled);

                    let normalized_samples = normalizer.normalize_loudness(raw_samples);
                    
                    // Convert to planar stereo format
                    let frames_encoded = normalized_samples.len() / input_channels as usize;
                    let planar_slices = to_planar_stereo(&normalized_samples, input_channels);

                    // Push to shared buffer for real-time mixing (resample if needed)
                    if planar_slices.len() >= 2 {
                        if let Some(ref mut rs) = resampler {
                            // Accumulate frames in buffer
                            resample_buffer_left.extend(&planar_slices[0]);
                            resample_buffer_right.extend(&planar_slices[1]);

                            // Process complete chunks
                            while resample_buffer_left.len() >= resampler_chunk_size
                                && resample_buffer_right.len() >= resampler_chunk_size {
                                let chunk_left: Vec<f32> = resample_buffer_left.drain(..resampler_chunk_size).collect();
                                let chunk_right: Vec<f32> = resample_buffer_right.drain(..resampler_chunk_size).collect();
                                let input_frames = vec![chunk_left, chunk_right];
                                if let Ok(resampled) = rs.process(&input_frames, None) {
                                    if resampled.len() >= 2 && !resampled[0].is_empty() {
                                        crate::audio_processing::MIC_BUFFER.push_planar(&resampled[0], &resampled[1]);
                                    }
                                }
                            }
                        } else {
                            // No resampling needed, push directly
                            crate::audio_processing::MIC_BUFFER.push_planar(&planar_slices[0], &planar_slices[1]);
                        }
                    }

                    // Encode to file (if not skipping)
                    if let Some(ref mut enc) = encoder {
                        let slices_ref: Vec<&[f32]> = planar_slices.iter().map(|v| v.as_slice()).collect();
                        enc.encode_audio_block(&slices_ref)?;
                    }
                    total_frames_written += frames_encoded as u64;
                    
                    if frames_remaining >= frames_encoded {
                        frames_remaining -= frames_encoded;
                    } else {
                        frames_remaining = 0;
                    }
                }
            }
            
            // 2. Silence Padding if still behind (only if encoding to file)
            if frames_remaining > 0 {
                if let Some(ref mut enc) = encoder {
                    // Generate silence for remaining frames
                    let silence_vec = vec![0.0f32; frames_remaining];
                    let silence_planar = vec![silence_vec.clone(), silence_vec]; // Output is always Stereo

                    let slices_ref: Vec<&[f32]> = silence_planar.iter().map(|v| v.as_slice()).collect();
                    enc.encode_audio_block(&slices_ref)?;
                }
                total_frames_written += frames_remaining as u64;
            }

            // Loop immediately if we still need to catch up
            if total_frames_written < expected_frames {
                continue;
            }
        } else {
             thread::sleep(tick_duration);
        }
    }

    // Flush remaining samples in resample buffer (pad to chunk size if needed)
    if let Some(ref mut rs) = resampler {
        let remaining_left = resample_buffer_left.len();
        let remaining_right = resample_buffer_right.len();
        if remaining_left > 0 || remaining_right > 0 {
            // Pad to chunk size with zeros
            resample_buffer_left.resize(resampler_chunk_size, 0.0);
            resample_buffer_right.resize(resampler_chunk_size, 0.0);
            let input_frames = vec![
                std::mem::take(&mut resample_buffer_left),
                std::mem::take(&mut resample_buffer_right),
            ];
            if let Ok(resampled) = rs.process(&input_frames, None) {
                if resampled.len() >= 2 && !resampled[0].is_empty() {
                    // Only push the non-padded portion (proportional to original remaining)
                    let valid_frames = (remaining_left.max(remaining_right) as f64
                        * (MIXER_SAMPLE_RATE as f64 / sample_rate as f64)) as usize;
                    let left_to_push = &resampled[0][..valid_frames.min(resampled[0].len())];
                    let right_to_push = &resampled[1][..valid_frames.min(resampled[1].len())];
                    crate::audio_processing::MIC_BUFFER.push_planar(left_to_push, right_to_push);
                }
            }
        }
    }

    // Drain remaining samples in the buffer (only what is currently there)
    loop {
        let available = consumer.occupied_len();
        if available == 0 {
            break;
        }
        
        // Read aligned to input channels
        let max_frames_in_buffer = chunk_size / input_channels as usize;
        let audio_frames_available = available / input_channels as usize;
        
        let frames_to_read = audio_frames_available.min(max_frames_in_buffer);
        let samples_to_read = frames_to_read * input_channels as usize;

        if samples_to_read == 0 {
            break; 
        }
        
        let n = consumer.pop_slice(&mut buffer[..samples_to_read]);
        
        if n > 0 {
            let raw_samples = &buffer[..n];
            let normalized_samples = normalizer.normalize_loudness(raw_samples);
            let frames_encoded = normalized_samples.len() / input_channels as usize;
            
            let planar_slices = to_planar_stereo(&normalized_samples, input_channels);
            
            if let Some(ref mut enc) = encoder {
                let slices_ref: Vec<&[f32]> = planar_slices.iter().map(|v| v.as_slice()).collect();
                if let Err(e) = enc.encode_audio_block(&slices_ref) {
                    eprintln!("Error encoding remaining mic audio: {}", e);
                    break;
                }
            }
            total_frames_written += frames_encoded as u64;
        }
    }

    // FINAL PADDING and finish (only if encoding to file)
    if let Some(mut enc) = encoder {
        let elapsed = start_time.elapsed().as_secs_f64();
        let expected_frames = (elapsed * sample_rate as f64) as u64;

        if total_frames_written < expected_frames {
            let missing_frames = expected_frames - total_frames_written;

            // Create silence block
            let silence_limit_chunk = 4096;
            let silence_vec = vec![0.0f32; silence_limit_chunk];
            let silence_planar = vec![silence_vec.clone(), silence_vec];
            let silence_refs: Vec<&[f32]> = silence_planar.iter().map(|v| v.as_slice()).collect();

            let mut remaining = missing_frames;
            while remaining > 0 {
                let chunk = remaining.min(silence_limit_chunk as u64);
                let partial_refs: Vec<&[f32]> = silence_refs.iter().map(|v| &v[..chunk as usize]).collect();
                if let Err(e) = enc.encode_audio_block(&partial_refs) {
                    eprintln!("Error writing final mic silence: {}", e);
                    break;
                }
                remaining -= chunk;
            }
        }

        enc.finish()?;
    }

    Ok(())
}
