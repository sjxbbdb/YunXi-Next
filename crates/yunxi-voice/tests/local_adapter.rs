use std::fs;
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};

use yunxi_voice::{
    AudioChunk, AudioChunkSink, AudioCodec, AudioFormat, Device, DeviceAvailability,
    DeviceDirection, DeviceGrant, LocalAudioConfig, LocalFileDevice, OperationContext, PcmAudio,
    ProviderOutcome, SynthesizedAudioChunk, VecSynthesizedAudioSource, decode_wav, encode_wav,
    read_pcm_file, read_wav_file, write_wav_file,
};

static NEXT_FILE: AtomicU64 = AtomicU64::new(0);

struct TempFiles {
    input: PathBuf,
    output: PathBuf,
    raw: PathBuf,
}

impl TempFiles {
    fn new() -> Self {
        let id = NEXT_FILE.fetch_add(1, Ordering::Relaxed);
        let base =
            std::env::temp_dir().join(format!("yunxi-voice-local-{}-{id}", std::process::id()));
        Self {
            input: base.with_extension("input.wav"),
            output: base.with_extension("output.wav"),
            raw: base.with_extension("input.pcm"),
        }
    }
}

impl Drop for TempFiles {
    fn drop(&mut self) {
        let _ = fs::remove_file(&self.input);
        let _ = fs::remove_file(&self.output);
        let _ = fs::remove_file(&self.raw);
    }
}

#[derive(Default)]
struct AudioSink {
    chunks: Vec<AudioChunk>,
}

impl AudioChunkSink for AudioSink {
    fn push(
        &mut self,
        chunk: AudioChunk,
        context: &OperationContext,
    ) -> Result<(), yunxi_voice::VoiceProviderError> {
        context.check()?;
        self.chunks.push(chunk);
        Ok(())
    }
}

fn format() -> AudioFormat {
    AudioFormat::new(AudioCodec::PcmS16Le, 16_000, 1).expect("format")
}

fn grant(id: &str, direction: DeviceDirection) -> DeviceGrant {
    DeviceGrant::new(
        "host-grant",
        yunxi_voice::DeviceId::new(id).expect("device id"),
        direction,
    )
    .expect("grant")
}

#[test]
fn wav_and_pcm_round_trip_with_bounds() {
    let audio = PcmAudio::new(format(), vec![1, 2, 3, 4, 5, 6, 7, 8]).expect("pcm");
    let wav = encode_wav(&audio).expect("encode");
    let decoded = decode_wav(&wav).expect("decode");
    assert_eq!(decoded, audio);
    assert!(decode_wav(b"RIFF\x00\x00\x00\x00WAVE").is_err());
}

#[test]
fn local_file_device_requires_host_grants_and_writes_real_wav() {
    let files = TempFiles::new();
    let input_audio = PcmAudio::new(format(), vec![0, 1, 2, 3, 4, 5, 6, 7]).expect("input");
    write_wav_file(&files.input, &input_audio).expect("write input");
    let config = LocalAudioConfig::new(Some(files.input.clone()), Some(files.output.clone()))
        .expect("config");
    let devices = config.enumerate_devices().expect("devices");
    assert_eq!(devices.devices.len(), 2);
    assert!(
        devices
            .devices
            .iter()
            .all(|device| device.availability == DeviceAvailability::Available)
    );

    let mut device = LocalFileDevice::new(config).expect("device");
    let mut sink = AudioSink::default();
    let context = OperationContext::new();
    let error = device
        .capture_with_grant(
            format(),
            &grant("wrong", DeviceDirection::Input),
            &mut sink,
            &context,
        )
        .expect_err("wrong grant");
    assert!(
        matches!(error, yunxi_voice::VoiceProviderError::ProviderFailure { code, .. } if code == "device_grant_mismatch")
    );

    let outcome = device
        .capture_with_grant(
            format(),
            &grant(yunxi_voice::LOCAL_INPUT_DEVICE_ID, DeviceDirection::Input),
            &mut sink,
            &context,
        )
        .expect("capture");
    assert_eq!(outcome, ProviderOutcome::Completed);
    assert_eq!(
        sink.chunks
            .iter()
            .map(|chunk| chunk.data.len())
            .sum::<usize>(),
        8
    );
    assert!(
        matches!(device.capture(format(), &mut sink, &context), Err(yunxi_voice::VoiceProviderError::ProviderFailure { code, .. }) if code == "device_grant_required")
    );

    let output = SynthesizedAudioChunk::new(
        yunxi_voice::RequestId::new("request").expect("request"),
        yunxi_voice::StreamId::new("stream").expect("stream"),
        0,
        format(),
        vec![9, 8, 7, 6],
        true,
    )
    .expect("output chunk");
    let mut source = VecSynthesizedAudioSource::new([output]);
    let outcome = device
        .playback_with_grant(
            format(),
            &grant(yunxi_voice::LOCAL_OUTPUT_DEVICE_ID, DeviceDirection::Output),
            &mut source,
            &context,
        )
        .expect("playback");
    assert_eq!(outcome, ProviderOutcome::Completed);
    assert_eq!(
        read_wav_file(&files.output).expect("output wav").data,
        vec![9, 8, 7, 6]
    );
}

#[test]
fn local_file_device_supports_raw_pcm_input() {
    let files = TempFiles::new();
    fs::write(&files.raw, [11_u8, 12, 13, 14]).expect("raw pcm");
    let audio = read_pcm_file(&files.raw, format()).expect("read pcm");
    assert_eq!(audio.data, vec![11, 12, 13, 14]);
    let config = LocalAudioConfig::new(Some(files.raw.clone()), None).expect("config");
    let mut device = LocalFileDevice::new(config).expect("device");
    let mut sink = AudioSink::default();
    device
        .capture_with_grant(
            format(),
            &grant(yunxi_voice::LOCAL_INPUT_DEVICE_ID, DeviceDirection::Input),
            &mut sink,
            &OperationContext::new(),
        )
        .expect("capture raw");
    assert_eq!(sink.chunks[0].data, vec![11, 12, 13, 14]);
}

#[test]
fn local_file_device_reports_missing_endpoints_without_opening_them() {
    let files = TempFiles::new();
    let config = LocalAudioConfig::new(Some(files.input.clone()), Some(files.output.clone()))
        .expect("config");
    let devices = config.enumerate_devices().expect("devices");
    assert_eq!(
        devices.devices[0].availability,
        DeviceAvailability::Unavailable
    );
    assert_eq!(
        devices.devices[1].availability,
        DeviceAvailability::Available
    );

    let directory_config =
        LocalAudioConfig::new(None, Some(std::env::temp_dir())).expect("directory config");
    let directory_devices = directory_config
        .enumerate_devices()
        .expect("directory probe");
    assert_eq!(
        directory_devices.devices[0].availability,
        DeviceAvailability::Unavailable
    );
}
