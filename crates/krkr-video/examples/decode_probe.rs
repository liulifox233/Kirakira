//! Decodes a few frames of a video file and prints stream metadata.
//! Usage: decode_probe <file> [frame_count] [seek_ms]
//! With `--audio` it also drains the soundtrack and reports chunk stats.

use std::path::Path;

fn main() {
    let mut args: Vec<String> = std::env::args().skip(1).collect();
    let audio_mode = if let Some(index) = args.iter().position(|arg| arg == "--audio") {
        args.remove(index);
        true
    } else {
        false
    };
    let mut args = args.into_iter();
    let path = args
        .next()
        .expect("usage: decode_probe <file> [frame_count] [seek_ms] [--audio]");
    let wanted: u32 = args.next().and_then(|v| v.parse().ok()).unwrap_or(5);
    let seek_ms: i64 = args.next().and_then(|v| v.parse().ok()).unwrap_or(0);
    let mut decoder = krkr_video::create_decoder(Path::new(&path)).expect("create_decoder");
    let meta = decoder.metadata().clone();
    println!(
        "meta: {}x{} @ {:.3} fps, {} frames, {} ms, audio={}",
        meta.width, meta.height, meta.fps, meta.frame_count, meta.duration_ms, meta.has_audio
    );
    println!("audio_spec: {:?}", decoder.audio_spec());
    if seek_ms > 0 {
        decoder.seek_ms(seek_ms).expect("seek");
    }
    let mut decoded = 0;
    while decoded < wanted {
        match decoder.next_frame() {
            Ok(Some(frame)) => {
                let checksum: u64 = frame.data.iter().take(4096).map(|b| u64::from(*b)).sum();
                let rgb_sum: u64 = frame
                    .data
                    .chunks_exact(4)
                    .map(|p| u64::from(p[0]) + u64::from(p[1]) + u64::from(p[2]))
                    .sum();
                let pixels = u64::from(frame.width) * u64::from(frame.height);
                println!(
                    "frame {}: pts={} ms {}x{} stride={} bytes={} head-sum={} rgb-mean={:.1}",
                    decoded,
                    frame.pts_ms,
                    frame.width,
                    frame.height,
                    frame.stride,
                    frame.data.len(),
                    checksum,
                    rgb_sum as f64 / (pixels * 3) as f64
                );
                decoded += 1;
            }
            Ok(None) => {
                println!("EOF after {decoded} frames");
                break;
            }
            Err(error) => {
                println!("decode error: {error}");
                std::process::exit(1);
            }
        }
    }
    if audio_mode {
        let mut chunks = 0u32;
        let mut samples_total = 0usize;
        let mut peak = 0.0f32;
        let mut last_pts = 0i64;
        loop {
            match decoder.next_audio_chunk() {
                Ok(Some(chunk)) => {
                    samples_total += chunk.samples.len();
                    last_pts = chunk.pts_ms;
                    for sample in &chunk.samples {
                        peak = peak.max(sample.abs());
                    }
                    if chunks < 5 {
                        let rms = (chunk
                            .samples
                            .iter()
                            .map(|sample| sample * sample)
                            .sum::<f32>()
                            / chunk.samples.len().max(1) as f32)
                            .sqrt();
                        println!(
                            "audio chunk {}: pts={} ms, {} samples, rms={:.4}",
                            chunks,
                            chunk.pts_ms,
                            chunk.samples.len(),
                            rms
                        );
                    }
                    chunks += 1;
                }
                Ok(None) => break,
                Err(error) => {
                    println!("audio decode error: {error}");
                    std::process::exit(1);
                }
            }
        }
        println!(
            "audio EOF: {chunks} chunks, {samples_total} samples, last pts={last_pts} ms, peak={peak:.4}"
        );
    }
}
