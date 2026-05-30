//! A minimal cpal-backed [`AudioBackend`] for Ruffle.
//!
//! Ruffle's reference cpal backend lives in `ruffle_frontend_utils`, but that
//! crate also pulls in `reqwest` and a full navigator, which we don't want. The
//! heavy lifting is done by `ruffle_core`'s [`AudioMixer`]; this just owns a cpal
//! output stream whose callback pulls mixed samples from the mixer's proxy. This
//! is a trimmed copy of `ruffle_frontend_utils::backends::audio::CpalAudioBackend`.

use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use cpal::SampleFormat;
use ruffle_core::backend::audio::{
    swf, AudioBackend, AudioMixer, DecodeError, RegisterError, SoundHandle, SoundInstanceHandle,
    SoundStreamInfo, SoundTransform,
};
use ruffle_core::impl_audio_mixer_backend;

#[derive(Debug, thiserror::Error)]
pub enum CpalError {
    #[error("No audio devices available")]
    NoDevices,
    #[error("Failed to get default output config")]
    DefaultStream(#[from] cpal::DefaultStreamConfigError),
    #[error("Unsupported sample format {0:?}")]
    UnsupportedSampleFormat(SampleFormat),
    #[error("Couldn't play the audio stream")]
    Play(#[from] cpal::PlayStreamError),
    #[error("Failed to construct audio stream")]
    Build(#[from] cpal::BuildStreamError),
}

pub struct CpalAudioBackend {
    #[allow(dead_code)] // kept alive for the lifetime of the stream
    device: cpal::Device,
    #[allow(dead_code)]
    config: cpal::StreamConfig,
    stream: cpal::Stream,
    mixer: AudioMixer,
}

impl CpalAudioBackend {
    pub fn new() -> Result<Self, CpalError> {
        let host = cpal::default_host();
        let device = host.default_output_device().ok_or(CpalError::NoDevices)?;

        let config = device
            .default_output_config()
            .map_err(CpalError::DefaultStream)?;
        let sample_format = config.sample_format();
        let config = cpal::StreamConfig::from(config);
        let mixer = AudioMixer::new(config.channels as u8, config.sample_rate.0);

        let stream = {
            let mixer = mixer.proxy();
            let error_handler = move |err| tracing::error!("Audio stream error: {err}");

            match sample_format {
                SampleFormat::F32 => device.build_output_stream(
                    &config,
                    move |buffer, _| mixer.mix::<f32>(buffer),
                    error_handler,
                    None,
                ),
                SampleFormat::I16 => device.build_output_stream(
                    &config,
                    move |buffer, _| mixer.mix::<i16>(buffer),
                    error_handler,
                    None,
                ),
                SampleFormat::U16 => device.build_output_stream(
                    &config,
                    move |buffer: &mut [u16], _| {
                        // Mix as i16 then bias to make 32768 the equilibrium.
                        mixer.mix::<i16>(bytemuck::cast_slice_mut(buffer));
                        for s in buffer.iter_mut() {
                            *s = (*s).wrapping_add(32768);
                        }
                    },
                    error_handler,
                    None,
                ),
                _ => return Err(CpalError::UnsupportedSampleFormat(sample_format)),
            }?
        };

        stream.play().map_err(CpalError::Play)?;

        Ok(Self {
            device,
            config,
            stream,
            mixer,
        })
    }
}

impl AudioBackend for CpalAudioBackend {
    impl_audio_mixer_backend!(mixer);

    fn play(&mut self) {
        if let Err(e) = self.stream.play() {
            tracing::warn!("Failed to resume audio stream: {e}");
        }
    }

    fn pause(&mut self) {
        if let Err(e) = self.stream.pause() {
            tracing::warn!("Failed to pause audio stream: {e}");
        }
    }
}
