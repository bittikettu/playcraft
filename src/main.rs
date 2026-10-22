use std::path::PathBuf;

use clap::Parser;
use mp3blaster_rs::audio::AudioConfig;
use mp3blaster_rs::{headless, library, log_ring, paths};

#[derive(Parser, Debug)]
#[command(name = "mp3blaster-rs", version, about = "Terminal audio player in the spirit of MP3Blaster")]
struct Cli {
    /// Audio files, folders or playlists to play.
    paths: Vec<PathBuf>,

    /// Play without the TUI, controlled by line commands on stdin.
    #[arg(long)]
    headless: bool,

    /// List audio output devices and exit.
    #[arg(long)]
    list_devices: bool,

    /// Output device to use (case-insensitive part of its name).
    #[arg(long, value_name = "NAME")]
    device: Option<String>,

    /// Initial volume in percent.
    #[arg(long, value_parser = clap::value_parser!(u8).range(0..=100))]
    volume: Option<u8>,
}

fn main() -> anyhow::Result<()> {
    let cli = Cli::parse();
    log_ring::init(log::LevelFilter::Info, paths::log_file().as_deref());

    if cli.list_devices {
        for name in mp3blaster_rs::audio::output::list_devices()? {
            println!("{name}");
        }
        return Ok(());
    }

    let config = AudioConfig {
        output_device: cli.device.clone(),
        volume: cli.volume.map_or(0.8, |v| f32::from(v) / 100.0),
        ..AudioConfig::default()
    };
    let tracks = library::expand_paths(&cli.paths);
    if cli.headless {
        return headless::run(tracks, config);
    }
    anyhow::bail!("the TUI is not implemented yet; run with --headless <files>")
}
