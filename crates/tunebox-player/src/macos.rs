
use std::{
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
    thread,
    time::{Duration, Instant},
};

use anyhow::{Context, Result};
use librespot::playback::{
    NUM_CHANNELS, SAMPLE_RATE,
    audio_backend::{Sink, SinkError, SinkResult},
    convert::Converter,
    decoder::AudioPacket,
};
use rodio::{
    buffer::SamplesBuffer,
    cpal::{
        self, SampleFormat, StreamConfig,
        traits::{DeviceTrait, HostTrait},
    },
};
use tokio::sync::mpsc::UnboundedSender;
use tunebox_os::audio::OutputWatch;

const QUEUED_CHUNKS: usize = 26;
const DRAIN_LIMIT: Duration = Duration::from_secs(1);
const SILENT_AFTER_LOSS: Duration = Duration::from_secs(3);
const POLL: Duration = Duration::from_millis(10);
const OPEN_RETRY: Duration = Duration::from_secs(1);

pub struct FollowingSink {
    host: cpal::Host,
    output: Option<Output>,
    stale: Arc<AtomicBool>,
    _watch: OutputWatch,
    silent_until: Option<Instant>,
    lost: UnboundedSender<String>,
    reopen_at: Option<Instant>,
}

struct Output {
    name: String,
    sink: rodio::Sink,
    _stream: rodio::OutputStream,
}

impl FollowingSink {
    pub fn new(lost: UnboundedSender<String>) -> Option<Self> {
        let stale = Arc::new(AtomicBool::new(false));
        let watch = OutputWatch::new({
            let stale = stale.clone();
            move || stale.store(true, Ordering::Relaxed)
        })?;
        let host = cpal::default_host();
        let output = open(&host, &stale);
        let reopen_at = output.is_none().then(|| Instant::now() + OPEN_RETRY);
        Some(Self { host, output, stale, _watch: watch, silent_until: None, lost, reopen_at })
    }

    fn follow(&mut self, playing: bool) {
        if !self.stale.swap(false, Ordering::Relaxed) {
            return;
        }
        if let Some(previous) = self.output.take()
            && playing
            && !self.is_present(&previous.name)
        {
            tracing::info!("audio output {} went away", previous.name);
            let _ = self.lost.send(previous.name);
            self.silent_until = Some(Instant::now() + SILENT_AFTER_LOSS);
        }
        self.reopen();
    }

    fn reopen(&mut self) {
        self.output = match open_default(&self.host, &self.stale) {
            Ok(output) => {
                tracing::info!("audio output: {}", output.name);
                self.reopen_at = None;
                Some(output)
            }
            Err(err) => {
                if self.reopen_at.is_none() {
                    tracing::warn!("no audio output: {err:#}");
                }
                self.reopen_at = Some(Instant::now() + OPEN_RETRY);
                None
            }
        };
    }

    fn is_present(&self, name: &str) -> bool {
        match self.host.output_devices() {
            Ok(mut devices) => devices.any(|device| device.name().is_ok_and(|n| n == name)),
            Err(_) => true,
        }
    }

    fn is_silent(&self) -> bool {
        self.silent_until.is_some_and(|until| Instant::now() < until)
    }
}

impl Sink for FollowingSink {
    fn start(&mut self) -> SinkResult<()> {
        self.silent_until = None;
        if self.output.is_none() {
            self.stale.store(true, Ordering::Relaxed);
        }
        self.follow(false);
        if let Some(output) = &self.output {
            output.sink.play();
        }
        Ok(())
    }

    fn stop(&mut self) -> SinkResult<()> {
        self.silent_until = None;
        if let Some(output) = &self.output {
            let deadline = Instant::now() + DRAIN_LIMIT;
            while !output.sink.empty() && !self.stale.load(Ordering::Relaxed) && Instant::now() < deadline {
                thread::sleep(POLL);
            }
            output.sink.pause();
        }
        Ok(())
    }

    fn write(&mut self, packet: AudioPacket, converter: &mut Converter) -> SinkResult<()> {
        self.follow(true);
        let samples = packet.samples().map_err(|err| SinkError::OnWrite(err.to_string()))?;
        let mut samples = converter.f64_to_f32(samples);
        if self.is_silent() {
            samples.fill(0.0);
        }
        let Some(output) = &self.output else {
            if self.reopen_at.is_some_and(|at| Instant::now() >= at) {
                self.stale.store(true, Ordering::Relaxed);
            }
            let frames = samples.len() as f64 / f64::from(NUM_CHANNELS);
            thread::sleep(Duration::from_secs_f64(frames / f64::from(SAMPLE_RATE)));
            return Ok(());
        };
        output.sink.append(SamplesBuffer::new(NUM_CHANNELS.into(), SAMPLE_RATE, samples));
        while output.sink.len() > QUEUED_CHUNKS && !self.stale.load(Ordering::Relaxed) {
            thread::sleep(POLL);
        }
        Ok(())
    }
}

fn open(host: &cpal::Host, stale: &Arc<AtomicBool>) -> Option<Output> {
    match open_default(host, stale) {
        Ok(output) => {
            tracing::info!("audio output: {}", output.name);
            Some(output)
        }
        Err(err) => {
            tracing::warn!("no audio output: {err:#}");
            None
        }
    }
}

fn open_default(host: &cpal::Host, stale: &Arc<AtomicBool>) -> Result<Output> {
    let device = host.default_output_device().context("No default audio output")?;
    let name = device.name().unwrap_or_else(|_| "unnamed output".into());
    let config = stereo_config(&device)?;
    let stale = stale.clone();
    let mut stream = rodio::OutputStreamBuilder::default()
        .with_device(device)
        .with_config(&config)
        .with_sample_format(SampleFormat::F32)
        .with_error_callback(move |err| {
            tracing::warn!("audio output: {err}");
            stale.store(true, Ordering::Relaxed);
        })
        .open_stream_or_fallback()?;
    stream.log_on_drop(false);
    let sink = rodio::Sink::connect_new(stream.mixer());
    Ok(Output { name, sink, _stream: stream })
}

fn stereo_config(device: &cpal::Device) -> Result<StreamConfig> {
    let default = device.default_output_config()?;
    let config = device
        .supported_output_configs()?
        .find(|c| c.channels() == u16::from(NUM_CHANNELS))
        .and_then(|c| {
            c.try_with_sample_rate(cpal::SampleRate(SAMPLE_RATE))
                .or_else(|| c.try_with_sample_rate(default.sample_rate()))
        })
        .unwrap_or(default);
    Ok(config.config())
}

#[cfg(test)]
mod tests {
    use tokio::sync::mpsc::{self, UnboundedReceiver};

    use super::*;

    fn silence() -> AudioPacket {
        AudioPacket::Samples(vec![0.0; 2048])
    }

    fn playing() -> Option<(FollowingSink, UnboundedReceiver<String>)> {
        let (tx, rx) = mpsc::unbounded_channel();
        let mut sink = FollowingSink::new(tx).expect("macOS reports changes of the default output");
        sink.output.as_ref()?;
        sink.start().unwrap();
        Some((sink, rx))
    }

    fn unplug(sink: &mut FollowingSink) {
        sink.output.as_mut().unwrap().name = "Headphones that went away".into();
        sink.stale.store(true, Ordering::Relaxed);
    }

    #[test]
    fn pauses_for_an_output_that_went_away() {
        let Some((mut sink, mut lost)) = playing() else { return };
        let mut converter = Converter::new(None);
        unplug(&mut sink);
        sink.write(silence(), &mut converter).unwrap();
        assert_eq!(lost.try_recv().as_deref(), Ok("Headphones that went away"));
        assert!(sink.is_silent(), "nothing plays through the new output until the pause");
        assert!(sink.output.is_some(), "the new default output is open");
        sink.stop().unwrap();
        assert!(!sink.is_silent());
    }

    #[test]
    fn moves_on_when_another_output_was_picked() {
        let Some((mut sink, mut lost)) = playing() else { return };
        let mut converter = Converter::new(None);
        sink.stale.store(true, Ordering::Relaxed);
        sink.write(silence(), &mut converter).unwrap();
        assert!(lost.try_recv().is_err());
        assert!(!sink.is_silent());
    }

    #[test]
    fn plays_on_after_an_output_went_away_while_paused() {
        let Some((mut sink, mut lost)) = playing() else { return };
        sink.stop().unwrap();
        unplug(&mut sink);
        sink.start().unwrap();
        assert!(lost.try_recv().is_err(), "starting again was asked for");
        assert!(!sink.is_silent());
    }
}
