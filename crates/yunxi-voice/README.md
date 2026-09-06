# yunxi-voice

`yunxi-voice` is the standalone Rust contract and process-fixture crate for the
first voice migration baseline:

- `voice.transcribe@1`: bounded audio input and `partial`/`final` transcript events.
- `voice.synthesize@1`: bounded text input and bounded synthesized audio output.

The crate contains no device access, SDK binding, codec implementation, network
client, async runtime, process host, or `unsafe` code. Its fixture is launch-wired
into the CLI/Web Host and appears in inventory when `voice` is enabled, which
proves the replaceable process boundary. It remains a contract/test backend,
not a real microphone, speaker, or speech-service implementation.

## Contract boundaries

- Every audio chunk is limited to 64 KiB.
- A request contains at most 4096 contiguous chunks, all with one stream ID and
  one audio format.
- Stream IDs and request IDs are bounded ASCII tokens.
- Transcript text is limited to 16 KiB; synthesis input is limited to 64 KiB.
- Sample rates are limited to 8 kHz through 192 kHz and channels to 1 through 8.
- `StreamStatus` carries cancellation and backpressure state. Its serde
  implementation validates capacity, buffered bytes, and cancellation reasons
  during decoding.
- Public request/chunk/transcript types validate on construction and on serde
  deserialization, so untrusted plugin frames cannot bypass the bounds.

The fixture constructors `transcribe_fixture()` and `synthesize_fixture()` are
deterministic and contain no external resources.

## Fixture process

`yunxi-voice-fixture` is a test-only child process. It uses the existing
`yunxi-protocol` version 2 handshake and bounded JSONL transport over the local
loopback connection supplied by the host (`YUNXI_PLUGIN_CONNECT_ADDRESS` and
`YUNXI_PLUGIN_CONNECT_TOKEN`). It does not open a listener itself and it never
accesses a microphone, speaker, network service, codec backend, or SDK.

The process announces both `voice.transcribe@1` and `voice.synthesize@1` under
the fixture identity `yunxi.voice.fixture` and supports these operations:

| Capability | Operation | Request | Deterministic response |
| --- | --- | --- | --- |
| `voice.transcribe@1` | `transcribe` | `TranscribeRequest` | status plus a partial transcript, and a final transcript when input is complete |
| `voice.synthesize@1` | `synthesize` | `SynthesisRequest` | status plus two synthetic audio chunks |
| either voice capability | `cancel` | `CancelRequest` | `CancelResult` with a terminal cancelled status |

The audio response bytes are deliberately synthetic markers, not playable
audio. Invalid capability/operation pairs produce `unsupported_operation`;
invalid or out-of-bounds payloads produce `invalid_request`; protocol and
handshake failures terminate the fixture process so the supervising host can
apply its normal isolation and retry policy.

Build the entry point from the workspace with:

```text
cargo build -p yunxi-voice --bin yunxi-voice-fixture
```

The binary expects a YunXi plugin host to provide the loopback environment and
is not intended to be launched directly as a user-facing voice service.

`yunxi-voice/tests/host_runtime.rs` verifies the real process boundary,
expected-capability handshake, malformed-payload containment, and route
removal after disable. The fixture manifest is intentionally `Safe` and declares no
`Device` grant because it never touches an audio device. A production voice
adapter must be a separate implementation that requests host-issued device
authority and is integrated into the CLI/Web composition before this crate can
be marked as an enabled user feature.

## Replaceable provider boundary

The crate now exposes a low-dependency, synchronous provider boundary without
claiming to own a microphone, speaker, codec, or speech service:

- `Device` is the capture/playback boundary.
- `Transcriber` consumes an `AudioChunkSource` and emits streaming
  `TranscriptEvent` values through `TranscriptSink`.
- `Synthesizer` emits bounded `SynthesizedAudioChunk` values through
  `SynthesizedAudioSink`.
- `TextFallback` provides a text-only path when an audio provider is disabled
  or unavailable.
- `OperationContext` and `CancellationToken` propagate cancellation and a
  monotonic deadline through every blocking call.
- `AudioChunkQueue` is a byte-bounded `Mutex`/`Condvar` queue. `push` waits for
  capacity while honoring cancellation and timeout; `try_push` returns an
  explicit backpressure error. `AudioChunkIterator` revalidates stream ID,
  format, sequence, and chunk bounds.
- `ProviderGate` is the Host-facing enable/disable semantic. A disabled gate
  fails before provider work begins and does not contain or clear session data.

`MockDevice`, `MockTranscriber`, `MockSynthesizer`, `LoopbackProvider`, and
`TextFallbackProvider` are deterministic test doubles. Their output is
synthetic; it is not playable audio and no implementation opens a device or
contacts an external speech API. Debug output reports counts and sizes only,
never audio bytes.

The traits are the production integration point. A real implementation must
be a separate process/plugin that requests Host-issued device authority and
keeps SDK credentials outside protocol payloads. Platform device handles,
codec libraries, provider credentials, and an async runtime remain external
prerequisites; this crate intentionally does not add them.

## Files

| Path | Responsibility |
| --- | --- |
| `src/audio.rs` | bounded input/output audio chunks and formats |
| `src/message.rs` | versioned transcribe/synthesize contracts |
| `src/state.rs` | cancellation and backpressure state machine |
| `src/fixture.rs` | in-process deterministic contract fixtures |
| `src/plugin.rs` | loopback process handshake, dispatch, and fixture errors |
| `src/stream.rs` | bounded queues, chunk iterator, cancellation, and deadlines |
| `src/provider.rs` | Device/Transcriber/Synthesizer/TextFallback traits and test doubles |
| `src/bin/yunxi-voice-fixture.rs` | executable child-process entry point |

## Workspace status

The contract is included in the `D:\YunXi Next` workspace. The executable is
still a contract fixture: no device access, codec backend, SDK binding, real
voice runtime, CLI/Web route, or runtime device-permission enforcement is
claimed. Synthetic output bytes are not playable audio.
