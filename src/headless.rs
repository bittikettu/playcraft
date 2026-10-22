//! Minimal line-driven player used to verify the audio core without the TUI.

use std::io::{BufRead, Write};
use std::path::PathBuf;
use std::time::{Duration, Instant};

use crossbeam_channel::{select, unbounded};

use crate::audio::{AudioCommand, AudioConfig, AudioEvent, AudioHandle};
use crate::model::PlaybackStatus;

const HELP: &str =
    "commands: p pause/resume | s stop | n next | f/b seek +/-5s | +/- volume | q quit";

fn mmss(d: Duration) -> String {
    let s = d.as_secs();
    format!("{:02}:{:02}", s / 60, s % 60)
}

pub fn run(tracks: Vec<PathBuf>, config: AudioConfig) -> anyhow::Result<()> {
    anyhow::ensure!(!tracks.is_empty(), "no playable files given");
    let audio = AudioHandle::spawn(config);
    let shared = audio.shared().clone();

    let (line_tx, mut line_rx) = unbounded::<String>();
    std::thread::spawn(move || {
        for line in std::io::stdin().lock().lines() {
            let Ok(line) = line else { break };
            if line_tx.send(line).is_err() {
                break;
            }
        }
    });

    println!("{HELP}");
    // Index of the track requested with LoadAndPlay, and of the one queued after it.
    let mut loading = 0usize;
    let mut queued: Option<usize> = None;
    let mut playing: Option<usize> = None;
    let mut failures = 0usize;
    audio.send(AudioCommand::LoadAndPlay(tracks[0].clone()));

    let mut next_report = Instant::now() + Duration::from_secs(1);
    loop {
        select! {
            recv(audio.events()) -> ev => {
                let Ok(ev) = ev else { break };
                match ev {
                    AudioEvent::TrackStarted { path, duration, info } => {
                        failures = 0;
                        let from = playing.map_or(0, |i| i + 1).min(tracks.len() - 1);
                        let idx = tracks.iter().skip(from).position(|t| *t == path).map_or(loading, |i| i + from);
                        playing = Some(idx);
                        println!(
                            ">> [{}/{}] {} ({}, {} Hz, {} ch, {})",
                            idx + 1,
                            tracks.len(),
                            path.display(),
                            info.codec,
                            info.sample_rate,
                            info.channels,
                            duration.map(mmss).unwrap_or_else(|| "--:--".into()),
                        );
                        queued = (idx + 1 < tracks.len()).then_some(idx + 1);
                        audio.send(AudioCommand::QueueNext(queued.map(|i| tracks[i].clone())));
                    }
                    AudioEvent::TrackFailed { path, error, .. } => {
                        eprintln!("!! {}: {error}", path.display());
                        failures += 1;
                        if failures >= tracks.len() {
                            eprintln!("!! nothing playable, stopping");
                            break;
                        }
                        if queued.is_some_and(|q| tracks[q] == path) {
                            queued = queued.map(|q| q + 1).filter(|&q| q < tracks.len());
                            audio.send(AudioCommand::QueueNext(queued.map(|i| tracks[i].clone())));
                        } else if tracks[loading] == path {
                            loading += 1;
                            if loading >= tracks.len() {
                                break;
                            }
                            audio.send(AudioCommand::LoadAndPlay(tracks[loading].clone()));
                        }
                    }
                    AudioEvent::Finished => {
                        println!("-- finished");
                        break;
                    }
                    AudioEvent::DeviceLost(msg) => println!("!! {msg}"),
                    AudioEvent::DeviceRestored(name) => println!("-- audio output: {name}"),
                    AudioEvent::Warning(msg) => println!("!! {msg}"),
                }
            }
            recv(line_rx) -> line => {
                let Ok(line) = line else {
                    // stdin closed: keep playing without interactive control.
                    line_rx = crossbeam_channel::never();
                    continue;
                };
                match line.trim() {
                    "" => {}
                    "p" => match shared.status() {
                        PlaybackStatus::Playing => audio.send(AudioCommand::Pause),
                        PlaybackStatus::Paused => audio.send(AudioCommand::Resume),
                        PlaybackStatus::Stopped => {
                            let i = playing.unwrap_or(loading);
                            loading = i;
                            audio.send(AudioCommand::LoadAndPlay(tracks[i].clone()));
                        }
                    },
                    "s" => audio.send(AudioCommand::Stop),
                    "n" => {
                        let i = playing.map_or(loading, |i| i + 1);
                        if i < tracks.len() {
                            loading = i;
                            audio.send(AudioCommand::LoadAndPlay(tracks[i].clone()));
                        }
                    }
                    "f" => audio.send(AudioCommand::SeekRelative(5)),
                    "b" => audio.send(AudioCommand::SeekRelative(-5)),
                    "+" | "=" => audio.send(AudioCommand::SetVolume((shared.volume() + 0.05).min(1.0))),
                    "-" => audio.send(AudioCommand::SetVolume((shared.volume() - 0.05).max(0.0))),
                    "q" => break,
                    _ => println!("{HELP}"),
                }
            }
            default(Duration::from_millis(100)) => {}
        }
        if Instant::now() >= next_report {
            next_report += Duration::from_secs(1);
            if shared.status() != PlaybackStatus::Stopped {
                println!(
                    "   [{}] {} / {}  vol {:.0}%  underruns {}",
                    shared.status().label(),
                    mmss(shared.position()),
                    mmss(shared.duration()),
                    shared.volume() * 100.0,
                    shared.underruns.load(std::sync::atomic::Ordering::Relaxed),
                );
                let _ = std::io::stdout().flush();
            }
        }
    }
    Ok(())
}
