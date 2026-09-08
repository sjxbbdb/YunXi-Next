# yunxi-voice

`yunxi-voice` is the standalone Rust contract and replaceable-provider crate
for the voice boundary:

- `voice.transcribe@1`: bounded audio input and `partial`/`final` transcript events.
- `voice.synthesize@1`: bounded text input and bounded synthesized audio output.
- Versioned sidecar frames for doctor, device enumeration, transcription,
  speech, chat, talk, playback, and save.
- Host-issued `DeviceGrant` values for external device access.

The crate contains no unstable system audio dependency, SDK binding, network
client, async runtime, or `unsafe` code. It includes a synchronous
`ProcessSidecarTransport` for an external JSONL child process and a
standard-library WAV/linear-PCM file adapter. The included fixtures and
Loopback/Mock providers remain deterministic test backends.

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

## Host plugin process

`yunxi-voice-fixture` is the compatibility-named child entry point used by the
Host. It uses the existing
`yunxi-protocol` version 2 handshake and bounded JSONL transport over the local
loopback connection supplied by the host (`YUNXI_PLUGIN_CONNECT_ADDRESS` and
`YUNXI_PLUGIN_CONNECT_TOKEN`). It does not open a listener itself.

Without `YUNXI_VOICE_SIDECAR_PROGRAM` it runs the deterministic fixture and
does not access a microphone, speaker, network service, codec backend, or SDK.
With an explicit sidecar program it launches the bounded process provider,
announces a required `Device` grant, and forwards transcription/synthesis
through that provider. The legacy plugin id remains stable for compatibility.

The process announces both `voice.transcribe@1` and `voice.synthesize@1` under
the fixture identity `yunxi.voice.fixture`. The operation is selected by both
the capability and operation name, so callers cannot use an output operation
through the input capability:

| Capability | Operation | Request | Deterministic response |
| --- | --- | --- | --- |
| `voice.transcribe@1` | `doctor` | `DoctorRequest` | bounded `DoctorReport` |
| `voice.transcribe@1` | `enumerate_devices` (`devices` alias) | `DeviceEnumerationRequest` | bounded `EnumeratedDevices` |
| `voice.transcribe@1` | `transcribe` | `TranscribeRequest` | status plus a partial transcript, and a final transcript when input is complete |
| `voice.transcribe@1` | `chat` | `ChatRequest` | bounded `ChatEvent` list |
| `voice.transcribe@1` | `talk` | `TalkRequest` | bounded transcript, chat, and audio events |
| `voice.synthesize@1` | `synthesize` | `SynthesisRequest` | status plus bounded synthesized audio chunks |
| `voice.synthesize@1` | `speak` | `SynthesisRequest` | alias of `synthesize` |
| `voice.synthesize@1` | `playback` | `AudioOutputPayload` | bounded `OutputResult` after Host-granted playback |
| `voice.synthesize@1` | `save` | `AudioOutputPayload` | bounded `OutputResult` for the logical save destination |
| either voice capability | `cancel` | `CancelRequest` | `CancelResult` with a terminal cancelled status |

`AudioOutputPayload` is `{ "request": AudioOutputRequest, "chunks":
[SynthesizedAudioChunk, ...] }`. Playback requires a matching output
`DeviceGrant`; save accepts only the logical `SaveDestinationId` in the
request. Every operation is decoded with the contract's bounded types and is
executed through `VoiceHostRuntime`, which adds the Host deadline, cooperative
cancellation checks, provider panic quarantine, and sidecar restart policy.
The protocol-level Host `Cancel` message also restarts the provider because it
does not carry a voice payload.

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

## Process JSONL transport

`ProcessSidecarConfig` and `ProcessSidecarTransport` provide the replaceable
real-provider boundary without linking a speech SDK into YunXi:

- `ProcessSidecarTransport::spawn(config)` starts one executable with piped
  stdin/stdout/stderr. The child environment is cleared; only
  `YUNXI_VOICE_SIDECAR_PROTOCOL=1` is supplied.
- Each exchange is exactly one bounded JSONL request followed by one bounded
  JSONL response. The default frame limit is 512 KiB and the hard limit is 4
  MiB. Parsed responses are validated again against the voice contract.
- Writer, reader, and stderr-drain threads prevent a blocked pipe from blocking
  lifecycle control. The configured timeout and `OperationContext` deadline
  are both enforced. Cancellation or timeout terminates the current child.
- A crash, EOF, write failure, malformed response, invalid response, or
  oversized frame becomes a stable error code without returning child output,
  request text, audio bytes, or credentials. The next exchange can start a
  fresh child; `restart` and `shutdown` are also explicit lifecycle controls.
- `ProcessSidecarConfig::from_environment()` is an optional constructor. It
  reads `YUNXI_VOICE_SIDECAR_PROGRAM`, simple whitespace-separated
  `YUNXI_VOICE_SIDECAR_ARGS`, `YUNXI_VOICE_SIDECAR_MAX_FRAME_BYTES`, and
  `YUNXI_VOICE_SIDECAR_TIMEOUT_MS`. These values configure the executable only;
  no host environment is forwarded to it.

The sidecar receives actual audio bytes in the `chunks` field of `transcribe`,
`talk`, `playback`, and `save` frames. A sidecar can implement platform audio
and STT/TTS with its own adapter without adding those dependencies to YunXi.
Its `doctor` and `enumerate_devices` responses are the runtime capability
check.

This is a transport boundary, not a hardware integration. A production
sidecar still has to implement actual microphone/speaker access, codecs, and
provider authentication. Device access is allowed only through host-issued
`DeviceGrant` values in the existing contract. Text chat and
`TextFallbackProvider` do not depend on this process. The CLI management facade
selects this transport only when `YUNXI_VOICE_SIDECAR_PROGRAM` is explicitly
configured. The launch-wired Host route now makes the same selection, so CLI,
TUI, and Web Host inventory all supervise the configured provider process.

`yunxi-voice/tests/host_runtime.rs` verifies the real process boundary,
expected-capability handshake, malformed-payload containment, and route
removal after disable. The no-sidecar fixture manifest is intentionally `Safe`
and declares no `Device` grant. The configured sidecar manifest is `External`
and requires `Device`; disabling Voice removes both the route and the process.
A real adapter must still supply hardware/provider behavior and pass external
device acceptance before Voice can be marked product-complete.

## Replaceable provider boundary

The crate exposes a low-dependency, synchronous provider boundary:

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
- `SidecarRequestFrame` and `SidecarResponseFrame` carry protocol version and
  validate one bounded message before dispatch.
- `ExternalSidecarProvider` maps the frame contract to doctor, devices,
  transcribe, speak, chat, talk, playback, and save. Feature bits reject
  unsupported operations before transport work.
- `DeviceGrant` is opaque host authority. External playback requires a matching
  output grant; external talk requires an input grant. Save uses a logical
  `SaveDestinationId`, never an arbitrary filesystem path.
- `LocalAudioConfig` and `LocalFileDevice` provide an explicit portable data
  adapter for `YUNXI_VOICE_INPUT_WAV`/`YUNXI_VOICE_INPUT_PCM` and
  `YUNXI_VOICE_OUTPUT_WAV`/`YUNXI_VOICE_OUTPUT_PCM`. WAV supports PCM S16LE and
  float32; `.pcm` is raw little-endian PCM using the requested format. Reading
  and writing require a matching Host `DeviceGrant`; probing checks file
  readiness and never opens an OS microphone or speaker.
- `OutputSelection` keeps playback and save mutually exclusive, and all output
  chunks are bounded by count, format, stream, sequence, and total bytes.

## Host runtime facade

`VoiceHostRuntime<F, C>` is the dynamic Host-facing entry point when the Host
needs to select or replace a provider at runtime. It exposes one consistent
facade for `doctor`, `enumerate_devices`, `transcribe`, `speak`, `chat`,
`talk`, `cancel`, `playback`, and `save` while retaining the existing lower
level traits for custom adapters:

- `VoiceHostConfig` supplies a bounded default operation timeout. A caller can
  obtain a cancellable `OperationContext` with `operation_context()` or apply
  the Host deadline to an existing caller context with `bounded_context()`.
- `PanicIsolatedProvider` catches an in-process provider panic, returns the
  stable `provider_panicked` error, and quarantines that provider until
  `replace_provider()` installs a fresh instance. A panic is not treated as
  evidence that a real device or SDK is healthy.
- `VoiceProvider::cancel` is a source-compatible default lifecycle hook. The
  shared `OperationContext` token stops a cooperative live operation; the
  provider-level hook acknowledges the request and resets provider-owned
  state, including a process sidecar.
- `ProviderGate` and `VoiceRouter` preserve the existing enable/disable and
  text-fallback behavior. Chat remains independently available when audio is
  disabled or unavailable, and output fallback does not claim that audio was
  played or saved.

The facade does not preempt an arbitrary in-process Rust call that ignores its
context. Hard timeout and crash containment require `ProcessSidecarTransport`
or another process/sandbox boundary. The included loopback and sidecar
fixtures are deterministic test backends only; they do not verify a physical
microphone, speaker, codec, or provider credential.

`MockDevice`, `MockTranscriber`, `MockSynthesizer`, `LoopbackProvider`,
`LoopbackVoiceProvider`, `MockChatProvider`, and `TextFallbackProvider` are
deterministic test doubles. Their output is synthetic; it is not playable audio
and no implementation opens a device or contacts an external speech API. Debug
output reports counts and sizes only, never audio bytes.

The traits are the production integration point. A real implementation must
be a separate process/plugin that requests Host-issued device authority and
keeps SDK credentials outside protocol payloads. Platform device handles,
codec libraries, provider credentials, and an async runtime remain external
prerequisites; this crate intentionally does not add them.

## Files

| Path | Responsibility |
| --- | --- |
| `src/audio.rs` | bounded input/output audio chunks, formats, and WAV/PCM IO |
| `src/message.rs` | versioned transcribe/synthesize contracts |
| `src/state.rs` | cancellation and backpressure state machine |
| `src/fixture.rs` | in-process deterministic contract fixtures |
| `src/plugin.rs` | Host process handshake plus loopback/sidecar selection and dispatch |
| `src/stream.rs` | bounded queues, chunk iterator, cancellation, and deadlines |
| `src/provider.rs` | Device/Transcriber/Synthesizer/TextFallback traits and test doubles |
| `src/sidecar.rs` | versioned external provider contract, grants, routing, and fallback |
| `src/host.rs` | dynamic Host facade, timeout composition, provider replacement, and panic quarantine |
| `src/process.rs` | bounded JSONL process lifecycle, timeout, cancellation, and recovery |
| `src/local.rs` | explicit local WAV/PCM data adapter, capability probe, and grant gate |
| `src/bin/yunxi-voice-fixture.rs` | executable child-process entry point |
| `src/bin/yunxi-voice-sidecar-fixture.rs` | test-only JSONL child for transport failures |

## Workspace status

The repository-supplied sidecar executable remains a fixture and does not
access hardware. The process transport supplies lifecycle and bounded JSONL
mechanics, while a real sidecar
owns hardware, codec, SDK, and provider authentication. The local file adapter
is usable now for real WAV/PCM bytes and deterministic integration tests, but it
is not an OS device backend. When audio is disabled or unavailable,
`VoiceRouter` can use text fallback while its independent chat provider remains
available. Manual completion work still requires a real sidecar, microphone,
speaker, provider credentials, permission denial, playable-audio verification,
and hardware cancellation/timeout checks; fixture output is not evidence for
those external behaviors.
