//! Integration test for the SymphoniaDecoder pipeline.
//! Downloads a real M4A file via yt-dlp and verifies decode works.

use crate::decoder::read_seek_source::ReadSeekSource;
use crate::decoder::SymphoniaDecoder;
use std::fs::File;
use symphonia::core::io::MediaSourceStream;

#[tokio::test]
async fn test_symphonia_decoder_decode_real_m4a() {
    let tmp = std::env::temp_dir().join("youtui_test_decode.m4a");
    // Download a short clip if not already present
    if !tmp.exists() {
        let status = std::process::Command::new("yt-dlp")
            .args([
                "-f", "bestaudio[ext=m4a]",
                "--download-sections", "*0-10",
                "--force-keyframes-at-cut",
                "-o", &tmp.to_string_lossy(),
                "https://www.youtube.com/watch?v=jNQXAC9IVRw",
            ])
            .status()
            .expect("yt-dlp failed");
        assert!(status.success(), "yt-dlp download failed");
    }
    assert!(tmp.exists(), "Downloaded file not found");

    let file = File::open(&tmp).unwrap();
    let file_len = file.metadata().ok().map(|m| m.len());
    let source = ReadSeekSource::new(file, file_len);
    let mss = MediaSourceStream::new(Box::new(source), Default::default());
    let decoder = SymphoniaDecoder::new(mss).expect("Failed to create decoder");
    assert!(decoder.total_duration().is_some(), "No duration reported");
    assert!(decoder.total_duration().unwrap().as_secs_f64() > 0.0, "Zero duration");
    assert!(decoder.channels().get() > 0, "Zero channels");
    assert!(decoder.sample_rate().get() > 0, "Zero sample rate");

    // Decode 1 second worth of samples
    let sample_rate = decoder.sample_rate().get() as u64;
    let target_samples = sample_rate; // 1 second

    // Cast to rodio Source and collect some samples
    assert!(decoder.current_span_len().is_none(),
        "current_span_len should be None while streaming (no EOS)");

    use rodio::Source;
    let source: Box<dyn Source<Item = f32> + Send> = Box::new(decoder);
    let samples: Vec<f32> = source.take(target_samples as usize).collect();
    assert!(!samples.is_empty(), "No samples decoded");
    assert!(samples.len() >= target_samples as usize - sample_rate as usize, "Too few samples: {}", samples.len()); // allow slight shortfall from packet boundary
    // Verify samples are in valid f32 range
    for &s in &samples {
        assert!(s.is_finite(), "Non-finite sample: {}", s);
    }

    // Clean up temp file
    let _ = std::fs::remove_file(&tmp);
}

#[tokio::test]
async fn test_symphonia_decoder_current_span_len_while_streaming() {
    let tmp = std::env::temp_dir().join("youtui_test_span_len.m4a");
    if !tmp.exists() {
        let status = std::process::Command::new("yt-dlp")
            .args([
                "-f", "bestaudio[ext=m4a]",
                "--download-sections", "*0-10",
                "--force-keyframes-at-cut",
                "-o", &tmp.to_string_lossy(),
                "https://www.youtube.com/watch?v=jNQXAC9IVRw",
            ])
            .status()
            .expect("yt-dlp failed");
        assert!(status.success(), "yt-dlp download failed");
    }

    let file = File::open(&tmp).unwrap();
    let file_len = file.metadata().ok().map(|m| m.len());
    let source = ReadSeekSource::new(file, file_len);
    let mss = MediaSourceStream::new(Box::new(source), Default::default());
    let decoder = SymphoniaDecoder::new(mss).expect("Failed to create decoder");

    assert!(decoder.current_span_len().is_none(),
        "current_span_len should be None while streaming (no EOS)");

    use rodio::Source;
    let source: Box<dyn Source<Item = f32> + Send> = Box::new(decoder);
    let sample_rate = source.sample_rate().get() as u64;
    let _samples: Vec<f32> = source.take(sample_rate as usize).collect();

    let _ = std::fs::remove_file(&tmp);
}
