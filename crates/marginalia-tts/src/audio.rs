//! PCM audio buffers, WAV I/O and (optionally) speaker playback.

use std::path::Path;

use anyhow::{bail, Context, Result};

/// Mono 16-bit PCM.
#[derive(Debug, Clone, PartialEq)]
pub struct Audio {
    pub sample_rate: u32,
    pub samples: Vec<i16>,
}

impl Audio {
    pub fn silence(sample_rate: u32, millis: u32) -> Audio {
        Audio {
            sample_rate,
            samples: vec![0; (sample_rate as u64 * millis as u64 / 1000) as usize],
        }
    }

    /// From float samples in [-1, 1].
    pub fn from_f32(sample_rate: u32, samples: &[f32]) -> Audio {
        Audio {
            sample_rate,
            samples: samples
                .iter()
                .map(|&x| (x.clamp(-1.0, 1.0) * i16::MAX as f32) as i16)
                .collect(),
        }
    }

    pub fn duration_secs(&self) -> f32 {
        self.samples.len() as f32 / self.sample_rate.max(1) as f32
    }

    /// Read a WAV file, down-mixing to mono 16-bit.
    pub fn read_wav(path: &Path) -> Result<Audio> {
        let mut reader =
            hound::WavReader::open(path).with_context(|| format!("reading {}", path.display()))?;
        let spec = reader.spec();
        let channels = spec.channels.max(1) as usize;
        let interleaved: Vec<i16> = match (spec.sample_format, spec.bits_per_sample) {
            (hound::SampleFormat::Int, 16) => reader.samples::<i16>().collect::<Result<_, _>>()?,
            (hound::SampleFormat::Int, bits) if bits <= 32 => reader
                .samples::<i32>()
                .map(|s| s.map(|v| (v >> (bits - 16).min(31)) as i16))
                .collect::<Result<_, _>>()?,
            (hound::SampleFormat::Float, 32) => reader
                .samples::<f32>()
                .map(|s| s.map(|v| (v.clamp(-1.0, 1.0) * i16::MAX as f32) as i16))
                .collect::<Result<_, _>>()?,
            (fmt, bits) => bail!("unsupported WAV format {fmt:?} {bits}-bit"),
        };
        let samples = interleaved
            .chunks(channels)
            .map(|frame| (frame.iter().map(|&s| s as i32).sum::<i32>() / frame.len() as i32) as i16)
            .collect();
        Ok(Audio {
            sample_rate: spec.sample_rate,
            samples,
        })
    }
}

/// Writes consecutive [`Audio`] chunks into one WAV file.
pub struct WavWriter {
    inner: hound::WavWriter<std::io::BufWriter<std::fs::File>>,
    sample_rate: u32,
}

impl WavWriter {
    pub fn create(path: &Path, sample_rate: u32) -> Result<Self> {
        let spec = hound::WavSpec {
            channels: 1,
            sample_rate,
            bits_per_sample: 16,
            sample_format: hound::SampleFormat::Int,
        };
        Ok(WavWriter {
            inner: hound::WavWriter::create(path, spec)
                .with_context(|| format!("creating {}", path.display()))?,
            sample_rate,
        })
    }

    pub fn append(&mut self, audio: &Audio) -> Result<()> {
        if audio.sample_rate != self.sample_rate {
            bail!(
                "sample rate changed from {} to {}",
                self.sample_rate,
                audio.sample_rate
            );
        }
        for &s in &audio.samples {
            self.inner.write_sample(s)?;
        }
        Ok(())
    }

    pub fn finish(self) -> Result<()> {
        Ok(self.inner.finalize()?)
    }
}

/// Plays audio chunks through the default output device, gapless.
///
/// The output stream is tied to the thread that created it on some
/// platforms, so keep the `Player` on one thread and hand a
/// [`PlayerHandle`] to others for pause/resume/stop.
#[cfg(feature = "playback")]
pub struct Player {
    // Field order matters: the sink must drop before the stream.
    sink: std::sync::Arc<rodio::Sink>,
    _stream: rodio::OutputStream,
}

#[cfg(feature = "playback")]
impl Player {
    pub fn new() -> Result<Self> {
        let mut stream = rodio::OutputStreamBuilder::open_default_stream()
            .context("opening the default audio output device")?;
        stream.log_on_drop(false);
        let sink = std::sync::Arc::new(rodio::Sink::connect_new(stream.mixer()));
        Ok(Player {
            sink,
            _stream: stream,
        })
    }

    /// Queue audio after whatever is already playing.
    pub fn enqueue(&self, audio: &Audio) {
        let samples: Vec<f32> = audio
            .samples
            .iter()
            .map(|&s| s as f32 / i16::MAX as f32)
            .collect();
        self.sink.append(rodio::buffer::SamplesBuffer::new(
            1,
            audio.sample_rate,
            samples,
        ));
    }

    /// Queue a callback that runs when playback reaches this point, e.g. to
    /// highlight the sentence that is about to be heard.
    pub fn enqueue_marker(&self, callback: impl Fn() + Send + 'static) {
        self.sink
            .append(rodio::source::EmptyCallback::new(Box::new(callback)));
    }

    /// Number of queued items (audio chunks and markers) not yet finished.
    pub fn queued(&self) -> usize {
        self.sink.len()
    }

    pub fn wait_until_done(&self) {
        self.sink.sleep_until_end();
    }

    pub fn handle(&self) -> PlayerHandle {
        PlayerHandle(self.sink.clone())
    }
}

/// Thread-safe remote control for a [`Player`].
#[cfg(feature = "playback")]
#[derive(Clone)]
pub struct PlayerHandle(std::sync::Arc<rodio::Sink>);

#[cfg(feature = "playback")]
impl PlayerHandle {
    pub fn pause(&self) {
        self.0.pause();
    }

    pub fn resume(&self) {
        self.0.play();
    }

    pub fn is_paused(&self) -> bool {
        self.0.is_paused()
    }

    /// Stop playback and drop everything queued.
    pub fn stop(&self) {
        self.0.stop();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn wav_round_trip() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("a.wav");
        let a = Audio {
            sample_rate: 16000,
            samples: vec![0, 100, -100, i16::MAX],
        };
        let mut w = WavWriter::create(&path, 16000).unwrap();
        w.append(&a).unwrap();
        w.append(&Audio::silence(16000, 1)).unwrap();
        w.finish().unwrap();
        let back = Audio::read_wav(&path).unwrap();
        assert_eq!(back.sample_rate, 16000);
        assert_eq!(&back.samples[..4], &a.samples[..]);
        assert_eq!(back.samples.len(), 4 + 16);
    }
}
