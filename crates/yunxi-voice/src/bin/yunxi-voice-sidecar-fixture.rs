//! JSONL child used only by `yunxi-voice` process-transport tests.

use std::io::{self, BufRead, Write};
use std::thread;
use std::time::Duration;

use yunxi_voice::{
    AudioFormat, ChatEvent, ChatEventKind, DeviceAvailability, DeviceDirection,
    DeviceEnumerationRequest, DeviceId, DeviceInfo, DoctorReport, EnumeratedDevices,
    ProviderFeatures, SidecarRequest, SidecarResponse, SidecarResponseFrame, SynthesisEvent,
    SynthesizedAudioChunk, TalkEvent, TranscribeEvent, TranscriptEvent,
};

fn main() {
    let mode = std::env::args().nth(2).unwrap_or_default();
    if mode == "crash" {
        std::process::exit(17);
    }
    let stdin = io::stdin();
    let mut stdout = io::BufWriter::new(io::stdout().lock());
    for line in stdin.lock().lines() {
        let Ok(line) = line else {
            return;
        };
        match mode.as_str() {
            "bad" => {
                let _ = writeln!(stdout, "not-json");
                let _ = stdout.flush();
                return;
            }
            "oversized" => {
                let _ = writeln!(stdout, "{}", "x".repeat(600 * 1024));
                let _ = stdout.flush();
                return;
            }
            "sleep" => thread::sleep(Duration::from_secs(5)),
            _ => {
                let Ok(request) = serde_json::from_str::<yunxi_voice::SidecarRequestFrame>(&line)
                else {
                    return;
                };
                let response = match request.request {
                    SidecarRequest::Doctor(_) => SidecarResponse::Doctor(
                        DoctorReport::ready(ProviderFeatures::audio_and_text(), 0)
                            .expect("fixture report"),
                    ),
                    SidecarRequest::EnumerateDevices(DeviceEnumerationRequest { .. }) => {
                        let format = AudioFormat::new(yunxi_voice::AudioCodec::PcmS16Le, 16_000, 1)
                            .expect("fixture format");
                        SidecarResponse::Devices(
                            EnumeratedDevices::new(vec![
                                DeviceInfo::new(
                                    DeviceId::new("sidecar-default").expect("device id"),
                                    "Sidecar fixture device",
                                    DeviceDirection::Duplex,
                                    DeviceAvailability::Available,
                                    vec![format],
                                )
                                .expect("device"),
                            ])
                            .expect("devices"),
                        )
                    }
                    SidecarRequest::Transcribe(request) => {
                        let mut events = vec![TranscribeEvent::Status(request.status.clone())];
                        events.push(TranscribeEvent::Transcript(
                            TranscriptEvent::partial(
                                request.stream_id.clone(),
                                0,
                                "sidecar partial",
                            )
                            .expect("partial transcript"),
                        ));
                        if request.input_complete {
                            events.push(TranscribeEvent::Transcript(
                                TranscriptEvent::final_text(request.stream_id, 1, "sidecar final")
                                    .expect("final transcript"),
                            ));
                        }
                        SidecarResponse::Transcripts(events)
                    }
                    SidecarRequest::Speak(request) => SidecarResponse::Speech(vec![
                        SynthesisEvent::Status(request.status.clone()),
                        SynthesisEvent::Audio(
                            SynthesizedAudioChunk::new(
                                request.request_id,
                                request.stream_id,
                                0,
                                request.format,
                                vec![1, 2, 3, 4],
                                true,
                            )
                            .expect("speech chunk"),
                        ),
                    ]),
                    SidecarRequest::Chat(_) => SidecarResponse::Chat(vec![
                        ChatEvent::new(ChatEventKind::Final, "sidecar chat").expect("chat event"),
                    ]),
                    SidecarRequest::Talk(request) => SidecarResponse::Talk(vec![
                        TalkEvent::Transcript(
                            TranscriptEvent::final_text(
                                request.input.stream_id.clone(),
                                0,
                                "sidecar talk transcript",
                            )
                            .expect("talk transcript"),
                        ),
                        TalkEvent::Chat(
                            ChatEvent::new(ChatEventKind::Final, "sidecar talk chat")
                                .expect("talk chat"),
                        ),
                        TalkEvent::Audio(
                            SynthesizedAudioChunk::new(
                                request.request_id,
                                request.input.stream_id,
                                0,
                                request.output_format,
                                vec![5, 6, 7, 8],
                                true,
                            )
                            .expect("talk audio"),
                        ),
                    ]),
                    SidecarRequest::Playback { chunks, .. } => {
                        SidecarResponse::Playback(yunxi_voice::OutputResult {
                            chunks: chunks.len(),
                            bytes: chunks.iter().map(|chunk| chunk.data.len()).sum(),
                        })
                    }
                    SidecarRequest::Save { chunks, .. } => {
                        SidecarResponse::Saved(yunxi_voice::OutputResult {
                            chunks: chunks.len(),
                            bytes: chunks.iter().map(|chunk| chunk.data.len()).sum(),
                        })
                    }
                };
                let frame = SidecarResponseFrame::new(response);
                let encoded = serde_json::to_string(&frame).expect("fixture response");
                let _ = writeln!(stdout, "{encoded}");
                let _ = stdout.flush();
            }
        }
    }
}
