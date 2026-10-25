//! The ffmpeg command line for the TV stream, and finding an H.264 encoder that works on
//! this computer (a graphics card's, else the x264 software encoder).

use std::path::Path;
use std::process::{Command, Stdio};
use std::sync::Mutex;

use super::scene::{HEIGHT, WIDTH};

/// Frames per second of the stream.
pub const FPS: usize = 30;
/// Audio sample rate of the stream.
pub const RATE: usize = 48_000;
pub const PLAYLIST: &str = "live.m3u8";

/// Encoders tried by `auto`, best first: hardware ones, then software.
const CANDIDATES: &[&str] = &["h264_nvenc", "h264_qsv", "h264_amf", "libx264", "h264_mf"];

/// The encoder found by the first `auto` search.
static FOUND: Mutex<Option<String>> = Mutex::new(None);

/// Whether ffmpeg can encode a few frames with `encoder` here.
fn works(ffmpeg: &str, encoder: &str) -> bool {
    let size = "256x144";
    Command::new(ffmpeg)
        .args(["-hide_banner", "-loglevel", "error", "-f", "lavfi", "-i"])
        .arg(format!("color=black:s={size}:r=30"))
        .args(["-frames:v", "3", "-c:v", encoder, "-f", "null", "-"])
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .is_ok_and(|s| s.success())
}

/// The encoder to use: `wanted` itself, or for `auto` the first that works (remembered).
/// `Err` when ffmpeg cannot be run at all.
pub fn choose(ffmpeg: &str, wanted: &str) -> Result<String, String> {
    let runs = Command::new(ffmpeg)
        .arg("-version")
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .is_ok_and(|s| s.success());
    if !runs {
        return Err(format!(
            "TV visuals need ffmpeg (\"{ffmpeg}\" does not run). Install it with: winget \
             install Gyan.FFmpeg"
        ));
    }
    if wanted != "auto" {
        return Ok(wanted.to_string());
    }
    let mut found = FOUND.lock().unwrap_or_else(|e| e.into_inner());
    if let Some(encoder) = found.as_ref() {
        return Ok(encoder.clone());
    }
    let encoder = CANDIDATES
        .iter()
        .find(|e| works(ffmpeg, e))
        .ok_or_else(|| "ffmpeg has no H.264 encoder that works here".to_string())?;
    log::info!("TV stream encoder: {encoder}");
    *found = Some(encoder.to_string());
    Ok(encoder.to_string())
}

/// Settings for low delay, by encoder.
fn tuning(encoder: &str) -> &'static [&'static str] {
    match encoder {
        "libx264" => &[
            "-preset",
            "veryfast",
            "-tune",
            "zerolatency",
            "-sc_threshold",
            "0",
        ],
        "h264_nvenc" => &["-preset", "p4", "-tune", "ll", "-forced-idr", "1"],
        "h264_qsv" => &["-preset", "veryfast"],
        "h264_amf" => &["-usage", "lowlatency", "-quality", "speed"],
        _ => &[],
    }
}

/// ffmpeg's arguments: raw video on stdin, raw audio from `audio_port` on localhost, HLS
/// into `dir`.
pub fn arguments(encoder: &str, audio_port: u16, dir: &Path) -> Vec<String> {
    let size = format!("{WIDTH}x{HEIGHT}");
    let mut args: Vec<String> = [
        "-hide_banner",
        "-loglevel",
        "error",
        // Video: RGB frames on stdin.
        "-f",
        "rawvideo",
        "-pix_fmt",
        "rgb24",
        "-s",
        &size,
        "-r",
        &FPS.to_string(),
        "-thread_queue_size",
        "64",
        "-probesize",
        "32",
        "-i",
        "pipe:0",
        // Audio: 16-bit stereo from a local socket.
        "-f",
        "s16le",
        "-ar",
        &RATE.to_string(),
        "-ac",
        "2",
        "-thread_queue_size",
        "1024",
        "-probesize",
        "32",
        "-i",
        &format!("tcp://127.0.0.1:{audio_port}"),
        "-map",
        "0:v",
        "-map",
        "1:a",
        "-c:v",
        encoder,
    ]
    .iter()
    .map(|s| s.to_string())
    .collect();
    args.extend(tuning(encoder).iter().map(|s| s.to_string()));
    let rest = [
        // A keyframe every second, so every segment starts with one.
        "-g",
        &FPS.to_string(),
        "-force_key_frames",
        "expr:gte(t,n_forced*1)",
        "-pix_fmt",
        "yuv420p",
        "-b:v",
        "4M",
        "-maxrate",
        "4M",
        "-bufsize",
        "8M",
        "-c:a",
        "aac",
        "-b:a",
        "192k",
        "-f",
        "hls",
        "-hls_time",
        "1",
        "-hls_list_size",
        "6",
        "-hls_flags",
        "delete_segments+independent_segments",
        "-hls_segment_filename",
    ];
    args.extend(rest.iter().map(|s| s.to_string()));
    args.push(dir.join("seg%05d.ts").to_string_lossy().into_owned());
    args.push(dir.join(PLAYLIST).to_string_lossy().into_owned());
    args
}

/// How many segments a playlist lists.
pub fn segments(playlist: &str) -> usize {
    playlist
        .lines()
        .filter(|l| l.starts_with("#EXTINF"))
        .count()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn arguments_feed_hls_from_raw_video_and_audio() {
        let dir = Path::new("tvdir");
        let args = arguments("libx264", 5555, dir);
        let joined = args.join(" ");
        assert!(
            joined.contains("-f rawvideo -pix_fmt rgb24 -s 1280x720 -r 30"),
            "{joined}"
        );
        assert!(joined.contains("-i tcp://127.0.0.1:5555"), "{joined}");
        assert!(joined.contains("-c:v libx264 -preset veryfast -tune zerolatency"));
        assert!(joined.contains("-c:a aac"));
        assert!(joined.contains("-f hls -hls_time 1"));
        assert!(args.last().unwrap().ends_with(PLAYLIST));
        // Hardware encoders get their own settings, unknown ones none.
        assert!(arguments("h264_nvenc", 1, dir).contains(&"ll".to_string()));
        let plain = arguments("h264_mf", 1, dir).join(" ");
        assert!(plain.contains("-c:v h264_mf -g 30"), "{plain}");
    }

    #[test]
    fn counts_playlist_segments() {
        let playlist = "#EXTM3U\n#EXT-X-TARGETDURATION:1\n#EXTINF:1.000000,\nseg00001.ts\n\
                        #EXTINF:1.000000,\nseg00002.ts\n";
        assert_eq!(segments(playlist), 2);
        assert_eq!(segments(""), 0);
    }

    #[test]
    fn a_missing_ffmpeg_is_explained() {
        let err = choose("no-such-ffmpeg-here", "auto").unwrap_err();
        assert!(err.contains("winget install Gyan.FFmpeg"), "{err}");
    }
}
