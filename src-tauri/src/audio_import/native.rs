//! Pull-based AVFoundation decoding straight into Vorbis. No temporary PCM
//! file, producer queue, or PCM allocation proportional to recording length.
use crate::storage::AudioInfo;
use cidre::{arc, av, cat, ns, objc};
use std::fs::File;
use std::num::{NonZeroU8, NonZeroU32};
use std::path::Path;
use std::time::{Duration, Instant};
use vorbis_rs::{VorbisBitrateManagementStrategy, VorbisEncoder, VorbisEncoderBuilder};

const BLOCK_FRAMES: usize = 4096;
const MAX_CHANNELS: usize = 2;

// Exactly 64 KiB of reusable application-owned PCM storage. Framework decoder
// buffers and Vorbis's internal working state are separate from this storage.
struct PcmBuffers {
    interleaved: Box<[f32; BLOCK_FRAMES * MAX_CHANNELS]>,
    planar: [Box<[f32; BLOCK_FRAMES]>; MAX_CHANNELS],
}

impl PcmBuffers {
    fn new() -> Self {
        Self {
            interleaved: Box::new([0.0; BLOCK_FRAMES * MAX_CHANNELS]),
            planar: std::array::from_fn(|_| Box::new([0.0; BLOCK_FRAMES])),
        }
    }

    fn deinterleave(&mut self, frames: usize, channels: usize) {
        for (index, frame) in self.interleaved[..frames * channels]
            .chunks_exact(channels)
            .enumerate()
        {
            for (channel, value) in frame.iter().enumerate() {
                self.planar[channel][index] = *value;
            }
        }
    }
}

struct ReaderGuard(arc::R<av::AssetReader>);

impl Drop for ReaderGuard {
    fn drop(&mut self) {
        if self.0.status() == av::asset::reader::Status::Reading {
            self.0.cancel_reading();
        }
    }
}

fn pcm_format(desc: &cat::audio::StreamBasicDesc) -> Result<(u32, usize), String> {
    let channels = desc.channels_per_frame as usize;
    if channels == 0 || channels > MAX_CHANNELS {
        return Err("Only mono and stereo audio files are supported.".into());
    }
    if !desc.sample_rate.is_finite()
        || desc.sample_rate < 1.0
        || desc.sample_rate > u32::MAX as f64
        || desc.sample_rate.fract() != 0.0
        || desc.format != cat::AudioFormat::LINEAR_PCM
        || !desc
            .format_flags
            .contains(cat::audio::FormatFlags::IS_FLOAT)
        || !desc.is_native_endian()
        || !desc.is_interleaved()
        || desc.bits_per_channel != 32
        || desc.bytes_per_frame as usize != channels * size_of::<f32>()
    {
        return Err("The native decoder returned an unsupported PCM format.".into());
    }
    Ok((desc.sample_rate as u32, channels))
}

pub(super) fn encode_qta(source: &Path, destination: &Path) -> Result<AudioInfo, String> {
    let started = Instant::now();
    let _pool = objc::AutoreleasePoolPage::push();
    let path = source.to_str().ok_or("Invalid QTA file path")?;
    let url = ns::Url::with_fs_path_str(path, false);
    let asset = av::UrlAsset::with_url(&url, None).ok_or("Cannot open QTA asset")?;
    let tracks = tauri::async_runtime::block_on(async {
        tokio::time::timeout(
            Duration::from_secs(30),
            asset.load_tracks_with_media_type(av::MediaType::audio()),
        )
        .await
        .map_err(|_| "Timed out reading QTA audio tracks".to_string())?
        .map_err(|error| format!("Cannot read QTA audio tracks: {error:?}"))
    })?;
    // Voice Memos puts the conventional audio track first; a second APAC
    // spatial track is the same recording, not an additional audio source.
    if tracks.is_empty() {
        return Err("QTA contains no audio track".into());
    }
    let track = tracks
        .get(0)
        .map_err(|error| format!("Cannot select QTA audio track: {error:?}"))?;
    let time_range = track.time_range();
    let expected_duration = time_range.duration.as_secs();
    let settings = ns::Dictionary::with_keys_values(
        &[
            av::audio::all_formats_keys::id(),
            av::audio::linear_pcm_keys::bit_depth(),
            av::audio::linear_pcm_keys::is_float(),
            av::audio::linear_pcm_keys::is_big_endian(),
            av::audio::linear_pcm_keys::is_non_interleaved(),
        ],
        &[
            cat::AudioFormat::LINEAR_PCM.as_ref(),
            32i32.as_ref(),
            1u8.as_ref(),
            0u8.as_ref(),
            0u8.as_ref(),
        ],
    );
    let mut output = av::AssetReaderTrackOutput::with_track(&track, Some(&settings))
        .map_err(|error| format!("Cannot configure QTA decoder: {error:?}"))?;
    output.set_always_copies_sample_data(false);
    let mut reader = ReaderGuard(
        av::AssetReader::with_asset(&asset)
            .map_err(|error| format!("Cannot create QTA reader: {error:?}"))?,
    );
    if !reader.0.can_add_output(&output) {
        return Err("Cannot decode this QTA audio track.".into());
    }
    reader
        .0
        .add_output(&output)
        .map_err(|error| format!("Cannot attach QTA decoder: {error:?}"))?;
    if time_range.is_valid() && !time_range.is_indefinite() {
        reader
            .0
            .set_time_range(time_range)
            .map_err(|error| format!("Cannot select QTA time range: {error:?}"))?;
    }
    if !reader
        .0
        .start_reading()
        .map_err(|error| format!("Cannot start QTA decoder: {error:?}"))?
    {
        return Err(format!("Cannot start QTA decoding: {:?}", reader.0.error()));
    }

    let mut buffers = PcmBuffers::new();
    let mut encoder: Option<VorbisEncoder<File>> = None;
    let mut format = None;
    let mut total_frames = 0u64;
    let mut blocks = 0u64;
    let mut largest_native_buffer = 0usize;
    loop {
        // Drain autoreleased framework objects every iteration, not once at
        // the end of a multi-hour import. The retained sample dies before it.
        let _sample_pool = objc::AutoreleasePoolPage::push();
        let Some(sample) = output
            .next_sample_buf()
            .map_err(|error| format!("Cannot read QTA samples: {error:?}"))?
        else {
            break;
        };
        let desc = sample
            .format_desc()
            .and_then(|desc| desc.stream_basic_desc())
            .ok_or("QTA decoder returned no audio format")?;
        let current_format = pcm_format(desc)?;
        let (rate, channels) = current_format;
        if let Some(previous) = format {
            if previous != current_format {
                return Err("QTA audio format changed during decoding.".into());
            }
        } else {
            format = Some(current_format);
            encoder = Some(
                VorbisEncoderBuilder::new_with_serial(
                    NonZeroU32::new(rate).ok_or("Invalid QTA sample rate")?,
                    NonZeroU8::new(channels as u8).ok_or("Invalid QTA channel count")?,
                    File::create(destination).map_err(|error| error.to_string())?,
                    0,
                )
                .bitrate_management_strategy(VorbisBitrateManagementStrategy::QualityVbr {
                    target_quality: 0.4,
                })
                .build()
                .map_err(|error| error.to_string())?,
            );
        }
        let frames =
            usize::try_from(sample.num_samples()).map_err(|_| "Invalid QTA sample count")?;
        if frames == 0 {
            continue;
        }
        let data = sample
            .data_buf()
            .ok_or("QTA decoder returned no PCM data")?;
        let bytes_per_frame = channels * size_of::<f32>();
        let expected_bytes = frames
            .checked_mul(bytes_per_frame)
            .ok_or("QTA sample size overflow")?;
        if data.data_len() != expected_bytes {
            return Err("QTA decoder returned an incomplete PCM buffer.".into());
        }
        largest_native_buffer = largest_native_buffer.max(data.data_len());
        for start in (0..frames).step_by(BLOCK_FRAMES) {
            let count = (frames - start).min(BLOCK_FRAMES);
            // SAFETY: the fixed destination contains 4096 stereo f32 frames.
            // count <= 4096, channels <= 2, and the source byte range was
            // checked above. Native PCM is packed little-endian float32.
            unsafe {
                data.copy_bytes(
                    start * bytes_per_frame,
                    count * bytes_per_frame,
                    buffers.interleaved.as_mut_ptr().cast::<u8>(),
                )
            }
            .map_err(|error| format!("Cannot copy QTA PCM block: {error:?}"))?;
            buffers.deinterleave(count, channels);
            let planar = [&buffers.planar[0][..count], &buffers.planar[1][..count]];
            encoder
                .as_mut()
                .ok_or("Missing QTA encoder")?
                .encode_audio_block(&planar[..channels])
                .map_err(|error| error.to_string())?;
            total_frames += count as u64;
            blocks += 1;
        }
        // No next read until Vorbis has consumed the current block: natural
        // backpressure, with no unbounded producer/consumer queue.
    }
    if reader.0.status() != av::asset::reader::Status::Completed {
        return Err(format!("QTA decoding failed: {:?}", reader.0.error()));
    }
    let (rate, channels) = format.ok_or("QTA contains no decodable audio")?;
    if total_frames == 0 {
        return Err("QTA contains no decodable audio".into());
    }
    let duration = total_frames as f64 / rate as f64;
    if expected_duration.is_finite()
        && expected_duration > 0.0
        && (duration - expected_duration).abs() > 0.1
    {
        return Err(format!(
            "Incomplete QTA audio: decoded {duration:.3}s of {expected_duration:.3}s"
        ));
    }
    encoder
        .ok_or("Missing QTA encoder")?
        .finish()
        .map_err(|error| error.to_string())?;
    log::info!(
        "audio_import: streamed QTA {:.3}s in {:.3}s, blocks={}, PCM scratch=65536 bytes, largest native buffer={} bytes",
        duration,
        started.elapsed().as_secs_f64(),
        blocks,
        largest_native_buffer
    );
    Ok(AudioInfo {
        file: "audio_mix.ogg".into(),
        duration_sec: duration,
        sample_rate: rate,
        channels: channels as u16,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pcm_storage_is_fixed_and_reused() {
        let mut buffers = PcmBuffers::new();
        let address = buffers.interleaved.as_ptr();
        let pcm_bytes = size_of_val(buffers.interleaved.as_ref())
            + buffers
                .planar
                .iter()
                .map(|channel| size_of_val(channel.as_ref()))
                .sum::<usize>();
        assert_eq!(pcm_bytes, 65_536);
        for _ in 0..128 {
            buffers.interleaved[0] = 0.25;
            buffers.interleaved[1] = -0.5;
            buffers.deinterleave(BLOCK_FRAMES, 2);
            assert_eq!(buffers.planar[0][0], 0.25);
            assert_eq!(buffers.planar[1][0], -0.5);
            assert_eq!(buffers.interleaved.as_ptr(), address);
        }
    }

    #[test]
    fn validates_pcm_layout_before_copying_into_fixed_storage() {
        let mut desc = cat::audio::StreamBasicDesc {
            sample_rate: 48_000.0,
            format: cat::AudioFormat::LINEAR_PCM,
            format_flags: cat::audio::FormatFlags::NATIVE_FLOAT_PACKED,
            channels_per_frame: 2,
            bits_per_channel: 32,
            bytes_per_frame: 8,
            ..Default::default()
        };
        assert_eq!(pcm_format(&desc).unwrap(), (48_000, 2));
        desc.channels_per_frame = 3;
        assert!(pcm_format(&desc).is_err());
        desc.channels_per_frame = 0;
        assert!(pcm_format(&desc).is_err());
        desc.channels_per_frame = 2;
        desc.bytes_per_frame = 4;
        assert!(pcm_format(&desc).is_err());
        desc.bytes_per_frame = 8;
        desc.sample_rate = f64::NAN;
        assert!(pcm_format(&desc).is_err());
    }
}
