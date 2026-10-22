use std::path::PathBuf;

use clap::Parser;
use mp3blaster_rs::audio::{AudioConfig, AudioHandle};
use mp3blaster_rs::ui::app::{App, BannerLevel};
use mp3blaster_rs::{config, headless, library, log_ring, paths, playlist, session, ui};

#[derive(Parser, Debug)]
#[command(
    name = "mp3blaster-rs",
    version,
    about = "Terminal audio player in the spirit of MP3Blaster"
)]
struct Cli {
    /// Audio files, folders or playlists (.m3u, .m3u8, .pls, .blst) to play.
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

    /// Use this config file instead of the default location.
    #[arg(long, value_name = "FILE")]
    config: Option<PathBuf>,

    /// Write a commented default config file (if none exists) and exit.
    #[arg(long)]
    write_default_config: bool,

    /// Do not resume the last track and position from the previous session.
    #[arg(long)]
    no_resume: bool,
}

fn write_default_config(path: Option<PathBuf>) -> anyhow::Result<()> {
    let path = path
        .or_else(paths::config_file)
        .ok_or_else(|| anyhow::anyhow!("no config directory"))?;
    anyhow::ensure!(!path.exists(), "{} already exists", path.display());
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
    }
    std::fs::write(&path, config::default_file_text())?;
    println!("wrote {}", path.display());
    Ok(())
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
    if cli.write_default_config {
        return write_default_config(cli.config);
    }

    let loaded = config::load(cli.config.as_deref());
    for w in &loaded.warnings {
        log::warn!("{w}");
    }
    let cfg = loaded.config;
    let audio_config = AudioConfig {
        output_device: cli.device.clone().or_else(|| cfg.output_device()),
        buffer_ms: cfg.general.audio_buffer_ms,
        gapless: cfg.general.gapless_playback,
        volume: cli.volume.map_or(0.8, |v| f32::from(v) / 100.0),
    };

    if cli.headless {
        return headless::run(library::expand_paths(&cli.paths), audio_config);
    }
    let startup = playlist::from_paths(&cli.paths);
    let audio = AudioHandle::spawn(audio_config);
    let mut app = App::new(audio, startup.root);
    app.playlist_path = startup.playlist_path;
    let mut warnings = loaded.warnings;
    warnings.extend(app.apply_config(&cfg));
    warnings.extend(startup.errors);
    if let Some(first) = warnings.first() {
        let more = if warnings.len() > 1 {
            format!(" (+{} more, see log)", warnings.len() - 1)
        } else {
            String::new()
        };
        app.banner(BannerLevel::Warning, format!("{first}{more}"));
    }

    let session_path = paths::session_file();
    if let Some(saved) = session_path.as_deref().and_then(session::load) {
        // Paths on the command line replace the saved playlist; settings are still restored.
        let keep_playlist = !cli.paths.is_empty();
        let resume = cfg.general.resume_on_startup && !cli.no_resume && !keep_playlist;
        app.restore(saved, keep_playlist, resume, cli.volume.is_none());
    }
    ui::run(&mut app, session_path.map(ui::Autosave::new))
}
