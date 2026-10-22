//! Output device handling via cpal (WASAPI shared mode on Windows).
//!
//! The stream always runs at the device's own mix format; the controller resamples to it.
//! A stream opened on the system default device is told by cpal when the default changes
//! (`StreamInvalidated`) or disappears (`DeviceNotAvailable`); both arrive as `StreamFault`s.

use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use cpal::{FromSample, SampleFormat, SizedSample, StreamConfig};
use crossbeam_channel::Sender;

use super::render::Renderer;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DeviceFormat {
    pub sample_rate: u32,
    pub channels: usize,
}

/// Why the running stream stopped being usable.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StreamFault {
    /// The device was unplugged or disabled and no replacement exists.
    DeviceLost,
    /// The device changed (e.g. a new system default); the stream must be rebuilt.
    Invalidated,
}

#[derive(Debug, thiserror::Error)]
pub enum OutputError {
    #[error("no audio output device available")]
    NoDevice,
    #[error("output device matching \"{0}\" not found")]
    DeviceNotFound(String),
    #[error("audio device error: {0}")]
    Device(#[from] cpal::Error),
    #[error("unsupported device sample format {0}")]
    UnsupportedFormat(SampleFormat),
}

/// Names of all output devices, as shown by `--list-devices`.
pub fn list_devices() -> Result<Vec<String>, OutputError> {
    let host = cpal::default_host();
    let default = host.default_output_device().map(|d| d.to_string());
    let mut names = Vec::new();
    for device in host.output_devices()? {
        let name = device.to_string();
        let marker = if Some(&name) == default.as_ref() {
            " (default)"
        } else {
            ""
        };
        names.push(format!("{name}{marker}"));
    }
    Ok(names)
}

/// A chosen device and its native format, before the stream is built.
pub struct DeviceChoice {
    device: cpal::Device,
    config: cpal::SupportedStreamConfig,
    name: String,
}

/// Picks the configured device (case-insensitive substring of its name) or the system default.
pub fn choose_device(preferred: Option<&str>) -> Result<DeviceChoice, OutputError> {
    let host = cpal::default_host();
    let device = match preferred.map(str::trim).filter(|p| !p.is_empty()) {
        Some(wanted) => {
            let needle = wanted.to_lowercase();
            host.output_devices()?
                .find(|d| d.to_string().to_lowercase().contains(&needle))
                .ok_or_else(|| OutputError::DeviceNotFound(wanted.to_string()))?
        }
        None => host.default_output_device().ok_or(OutputError::NoDevice)?,
    };
    let config = device.default_output_config()?;
    let name = device.to_string();
    Ok(DeviceChoice {
        device,
        config,
        name,
    })
}

impl DeviceChoice {
    pub fn format(&self) -> DeviceFormat {
        DeviceFormat {
            sample_rate: self.config.sample_rate(),
            channels: usize::from(self.config.channels()),
        }
    }

    pub fn name(&self) -> &str {
        &self.name
    }

    /// Builds and starts the stream. Faults are reported on `faults`.
    pub fn start(
        self,
        renderer: Renderer,
        faults: Sender<StreamFault>,
    ) -> Result<Output, OutputError> {
        let format = self.format();
        let config: StreamConfig = self.config.config();
        let stream = match self.config.sample_format() {
            SampleFormat::F32 => build_f32(&self.device, config, renderer, faults)?,
            SampleFormat::F64 => build::<f64>(&self.device, config, renderer, faults)?,
            SampleFormat::I16 => build::<i16>(&self.device, config, renderer, faults)?,
            SampleFormat::I24 => build::<cpal::I24>(&self.device, config, renderer, faults)?,
            SampleFormat::I32 => build::<i32>(&self.device, config, renderer, faults)?,
            SampleFormat::U16 => build::<u16>(&self.device, config, renderer, faults)?,
            SampleFormat::U8 => build::<u8>(&self.device, config, renderer, faults)?,
            other => return Err(OutputError::UnsupportedFormat(other)),
        };
        stream.play()?;
        log::info!(
            "audio output: {} ({} Hz, {} ch, {:?})",
            self.name,
            format.sample_rate,
            format.channels,
            self.config.sample_format()
        );
        Ok(Output {
            _stream: stream,
            format,
            name: self.name,
        })
    }
}

/// A running output stream. Dropping it stops playback.
pub struct Output {
    _stream: cpal::Stream,
    format: DeviceFormat,
    name: String,
}

impl Output {
    pub fn format(&self) -> DeviceFormat {
        self.format
    }

    pub fn name(&self) -> &str {
        &self.name
    }
}

fn error_callback(faults: Sender<StreamFault>) -> impl FnMut(cpal::Error) + Send + 'static {
    move |err| {
        let fault = match err.kind() {
            cpal::ErrorKind::DeviceNotAvailable => Some(StreamFault::DeviceLost),
            cpal::ErrorKind::StreamInvalidated | cpal::ErrorKind::DeviceChanged => {
                Some(StreamFault::Invalidated)
            }
            _ => None,
        };
        match fault {
            Some(fault) => {
                let _ = faults.try_send(fault);
            }
            None => log::debug!("audio stream: {err}"),
        }
    }
}

fn build_f32(
    device: &cpal::Device,
    config: StreamConfig,
    mut renderer: Renderer,
    faults: Sender<StreamFault>,
) -> Result<cpal::Stream, OutputError> {
    Ok(device.build_output_stream::<f32, _, _>(
        config,
        move |data, _| renderer.fill(data),
        error_callback(faults),
        None,
    )?)
}

fn build<T>(
    device: &cpal::Device,
    config: StreamConfig,
    mut renderer: Renderer,
    faults: Sender<StreamFault>,
) -> Result<cpal::Stream, OutputError>
where
    T: SizedSample + FromSample<f32>,
{
    // Preallocated so the callback normally never allocates.
    let mut scratch = vec![0f32; 16_384 * usize::from(config.channels)];
    Ok(device.build_output_stream::<T, _, _>(
        config,
        move |data: &mut [T], _| {
            if scratch.len() < data.len() {
                scratch.resize(data.len(), 0.0);
            }
            let buf = &mut scratch[..data.len()];
            renderer.fill(buf);
            for (dst, &src) in data.iter_mut().zip(buf.iter()) {
                *dst = T::from_sample(src.clamp(-1.0, 1.0));
            }
        },
        error_callback(faults),
        None,
    )?)
}
