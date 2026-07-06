use std::sync::LazyLock;
use std::time::Duration;

use rodio::{ChannelCount, SampleRate, Source};
use std::num::NonZero;
use symphonia::core::audio::{Layout, SampleBuffer, SignalSpec};
use symphonia::core::codecs::{
    CodecRegistry, CODEC_TYPE_NULL,
    Decoder, DecoderOptions,
};
use symphonia::core::errors::Error;
use symphonia::core::formats::{
    FormatOptions, FormatReader, SeekMode, SeekTo, Track,
};
use symphonia::core::io::MediaSourceStream;
use symphonia::core::meta::MetadataOptions;
use symphonia::core::probe::Hint;
use symphonia::core::units::Time;
use tracing::{debug, info};

const DEFAULT_CHANNELS: NonZero<u16> = NonZero::<u16>::new(2).unwrap();
const DEFAULT_SAMPLE_RATE: NonZero<u32> = NonZero::<u32>::new(44100).unwrap();


pub mod read_seek_source;

static CODEC_REGISTRY: LazyLock<CodecRegistry> = LazyLock::new(|| {
    let mut registry = CodecRegistry::new();
    symphonia::default::register_enabled_codecs(&mut registry);
    registry
});

fn is_codec_null(track: &Track) -> bool {
    track.codec_params.codec == CODEC_TYPE_NULL
}

// `dyn Decoder` and `dyn FormatReader` from symphonia are not `Send`
// but their concrete implementations are safe to move between threads
// (no thread-local state).  Newtype wrappers localize the unsafe so
// that adding a non-Send field to SymphoniaDecoder fails to compile.
struct SendDecoder(Box<dyn Decoder>);
unsafe impl Send for SendDecoder {}
struct SendFormatReader(Box<dyn FormatReader>);
unsafe impl Send for SendFormatReader {}

pub struct SymphoniaDecoder {
    decoder: SendDecoder,
    track_id: u32,
    probed: SendFormatReader,
    buffer: SampleBuffer<f32>,
    spec: SignalSpec,
    current_frame_offset: usize,
    eos: bool,
    duration: Option<Duration>,
}

impl SymphoniaDecoder {
    pub fn new(mss: MediaSourceStream) -> Result<Self, SymphoniaError> {
        let probe_result = symphonia::default::get_probe().format(
            &Hint::default(),
            mss,
            &FormatOptions::default(),
            &MetadataOptions::default(),
        ).map_err(SymphoniaError::from)?;
        let probed = SendFormatReader(probe_result.format);

        let track = probed.0
            .default_track()
            .and_then(|v| if is_codec_null(v) { None } else { Some(v) })
            .or_else(|| probed.0.tracks().iter().find(|v| !is_codec_null(v)))
            .ok_or(SymphoniaError::NoStreams)?;

        let codec_params = &track.codec_params;

        let decoder = SendDecoder(CODEC_REGISTRY.make(
            codec_params,
            &DecoderOptions::default(),
        )?);

        let cp_dec = decoder.0.codec_params();

        let duration = codec_params.n_frames.and_then(|n_frames| {
            codec_params.time_base.map(|tb: symphonia::core::units::TimeBase| {
                let time: Time = tb.calc_time(n_frames);
                Duration::new(time.seconds, (1_000_000_000.0 * time.frac) as u32)
            })
        });

        let track_id = track.id;
        let cp = cp_dec;
        let spec = SignalSpec::new(
            cp.sample_rate.unwrap_or(44100),
            Layout::Stereo.into_channels(),
        );

        let buffer = SampleBuffer::new(0u64, spec);

        info!(
            codec_sample_rate = codec_params.sample_rate,
            decoder_sample_rate = spec.rate,
            decoder_channels = spec.channels.count(),
            duration_s = duration.map(|d: std::time::Duration| d.as_secs_f64()),
            "SymphoniaDecoder created"
        );
        // Log detailed codec tracking info for isomp4 debugging
        tracing::info!(
            n_frames = codec_params.n_frames.map(|f| f as i64),
            time_base_num = codec_params.time_base.map(|t| t.numer as i64),
            time_base_den = codec_params.time_base.map(|t| t.denom as i64),
            codec = %codec_params.codec,
            "SymphoniaDecoder codec params"
        );

        Ok(SymphoniaDecoder {
            decoder,
            track_id,
            probed,
            buffer,
            spec,
            current_frame_offset: 0,
            eos: false,
            duration,
        })
    }

    pub fn try_seek_to(&mut self, pos: Duration) -> Result<(), SymphoniaError> {
        let time = Time::new(
            pos.as_secs(),
            pos.subsec_nanos() as f64 / 1_000_000_000.0,
        );
        match self.probed.0.seek(SeekMode::Coarse, SeekTo::Time {
            time,
            track_id: Some(self.track_id),
        }) {
            Ok(_) => {
                self.current_frame_offset = 0;
                self.buffer.clear();
                self.eos = false;
                Ok(())
            }
            Err(_) => Err(SymphoniaError::SeekFailed),
        }
    }
}

impl Iterator for SymphoniaDecoder {
    type Item = f32;

    fn next(&mut self) -> Option<Self::Item> {
        if self.eos {
            return None;
        }

        if self.current_frame_offset >= self.buffer.len() {
            loop {
                let packet = match self.probed.0.next_packet() {
                    Ok(packet) => packet,
                    Err(err) => {
                        tracing::debug!("Error reading packet: {err:?}");
                        self.eos = true;
                        return None;
                    }
                };

                if packet.track_id() != self.track_id {
                    continue;
                }

                match self.decoder.0.decode(&packet) {
                    Ok(audio_buf) => {
                            if audio_buf.frames() == 0 {
                                continue;
                            }
                            self.spec = *audio_buf.spec();
                            let num_frames = audio_buf.frames();
                        self.buffer =
                            SampleBuffer::new(num_frames as u64, self.spec);
                        self.buffer.copy_interleaved_ref(audio_buf);
                        self.current_frame_offset = 0;
                        break;
                    }
                    Err(Error::DecodeError(_err)) => {
                        continue;
                    }
                    Err(err) => {
                        tracing::error!("Fatal decode error: {err:?}");
                        self.eos = true;
                        return None;
                    }
                }
            }
        }

        let sample = *self.buffer.samples().get(self.current_frame_offset)?;
        self.current_frame_offset += 1;
        Some(sample)
    }
}

impl Source for SymphoniaDecoder {
    fn current_span_len(&self) -> Option<usize> {
        if self.eos {
            debug!("current_span_len -> Some(0) (eos)");
            Some(0)
        } else {
            None
        }
    }

    fn channels(&self) -> ChannelCount {
        let c = u16::try_from(self.spec.channels.count()).unwrap_or(2);
        NonZero::new(c).unwrap_or(DEFAULT_CHANNELS)
    }

    fn sample_rate(&self) -> SampleRate {
        let r = self.spec.rate;
        NonZero::new(r).unwrap_or(DEFAULT_SAMPLE_RATE)
    }

    fn total_duration(&self) -> Option<Duration> {
        self.duration
    }

    fn try_seek(&mut self, pos: Duration) -> Result<(), rodio::source::SeekError> {
        self.try_seek_to(pos).map_err(|_| {
            rodio::source::SeekError::NotSupported {
                underlying_source: "",
            }
        })
    }
}

#[derive(Debug)]
pub enum SymphoniaError {
    UnrecognizedFormat,
    IoError(String),
    DecodeError(&'static str),
    LimitError(&'static str),
    ResetRequired,
    NoStreams,
    SeekFailed,
}

impl std::fmt::Display for SymphoniaError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::UnrecognizedFormat => write!(f, "Unrecognized format"),
            Self::IoError(msg) => write!(f, "{msg}"),
            Self::DecodeError(msg) | Self::LimitError(msg) => write!(f, "{msg}"),
            Self::ResetRequired => write!(f, "Reset required"),
            Self::NoStreams => write!(f, "No audio streams found"),
            Self::SeekFailed => write!(f, "Seek failed"),
        }
    }
}

impl std::error::Error for SymphoniaError {}

impl From<symphonia::core::errors::Error> for SymphoniaError {
    fn from(value: symphonia::core::errors::Error) -> Self {
        match value {
            Error::IoError(e) => Self::IoError(e.to_string()),
            Error::DecodeError(e) => Self::DecodeError(e),
            Error::SeekError(_) => Self::SeekFailed,
            Error::Unsupported(_) => Self::UnrecognizedFormat,
            Error::LimitError(e) => Self::LimitError(e),
            Error::ResetRequired => Self::ResetRequired,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::decoder::read_seek_source::ReadSeekSource;
    use std::io::Cursor;

    #[test]
    fn new_with_empty_buffer_returns_error() {
        let cursor = Cursor::new(Vec::new());
        let source = ReadSeekSource::new(cursor, Some(0));
        let mss = MediaSourceStream::new(Box::new(source), Default::default());
        let result = SymphoniaDecoder::new(mss);
        assert!(result.is_err(), "Decoder should fail for empty buffer");
    }

    #[test]
    fn new_with_truncated_data_returns_error() {
        let cursor = Cursor::new(vec![0u8; 16]);
        let source = ReadSeekSource::new(cursor, Some(16));
        let mss = MediaSourceStream::new(Box::new(source), Default::default());
        let result = SymphoniaDecoder::new(mss);
        assert!(result.is_err(), "Decoder should fail for truncated data");
    }

    #[test]
    fn error_display_formats_known_variants() {
        let cases: Vec<(SymphoniaError, &str)> = vec![
            (SymphoniaError::UnrecognizedFormat, "Unrecognized format"),
            (SymphoniaError::NoStreams, "No audio streams found"),
            (SymphoniaError::SeekFailed, "Seek failed"),
            (SymphoniaError::DecodeError("bad"), "bad"),
            (SymphoniaError::IoError("eof".into()), "eof"),
        ];
        for (err, expected) in cases {
            assert_eq!(err.to_string(), expected);
        }
    }

    #[test]
    fn error_into_trait_from_io_error() {
        let io_err = Error::IoError(std::io::Error::new(std::io::ErrorKind::UnexpectedEof, "eof"));
        let se: SymphoniaError = io_err.into();
        assert!(matches!(se, SymphoniaError::IoError(_)));
    }
}
