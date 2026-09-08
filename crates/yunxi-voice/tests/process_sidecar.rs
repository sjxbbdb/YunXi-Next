use std::thread;
use std::time::Duration;

use yunxi_voice::{
    DoctorRequest, OperationContext, ProcessSidecarConfig, ProcessSidecarTransport, RequestId,
    SidecarRequest, SidecarRequestFrame, SidecarTransport, VoiceProviderError,
};

fn config(mode: &str) -> ProcessSidecarConfig {
    ProcessSidecarConfig::new(env!("CARGO_BIN_EXE_yunxi-voice-sidecar-fixture"))
        .expect("fixture config")
        .timeout(Duration::from_millis(250))
        .expect("timeout")
        .with_args(["--fixture", mode])
        .expect("args")
}

fn doctor_frame() -> SidecarRequestFrame {
    SidecarRequestFrame::new(SidecarRequest::Doctor(DoctorRequest::new(
        RequestId::new("doctor").expect("request id"),
    )))
}

#[test]
fn process_transport_roundtrips_and_restarts() {
    let mut transport = ProcessSidecarTransport::spawn(config("normal")).expect("spawn");
    let first = transport
        .exchange(doctor_frame(), &OperationContext::new())
        .expect("doctor");
    assert_eq!(first.version, 1);
    assert!(transport.is_running());
    let second = transport
        .exchange(doctor_frame(), &OperationContext::new())
        .expect("second doctor on same process");
    assert_eq!(second.version, 1);
    transport.restart();
    assert_eq!(transport.generation(), 1);
    let third = transport
        .exchange(doctor_frame(), &OperationContext::new())
        .expect("restart doctor");
    assert_eq!(third.version, 1);
}

#[test]
fn malformed_frame_is_contained_without_raw_output() {
    let mut transport = ProcessSidecarTransport::spawn(config("bad")).expect("spawn");
    let error = transport
        .exchange(doctor_frame(), &OperationContext::new())
        .expect_err("bad frame");
    assert_eq!(
        error,
        VoiceProviderError::provider_failure("sidecar_bad_frame", false)
    );
    assert!(!error.to_string().contains("not-json"));
    assert!(!format!("{error:?}").contains("not-json"));
    assert!(!transport.is_running());
}

#[test]
fn timeout_cancels_and_kills_the_process() {
    let mut transport = ProcessSidecarTransport::spawn(config("sleep")).expect("spawn");
    let error = transport
        .exchange(doctor_frame(), &OperationContext::new())
        .expect_err("timeout");
    assert_eq!(error, VoiceProviderError::TimedOut);
    assert!(!transport.is_running());
}

#[test]
fn cancellation_kills_a_blocked_sidecar() {
    let mut transport = ProcessSidecarTransport::spawn(config("sleep")).expect("spawn");
    let context = OperationContext::new();
    let cancel = context.cancellation_token();
    let thread = thread::spawn(move || transport.exchange(doctor_frame(), &context));
    thread::sleep(Duration::from_millis(50));
    cancel.cancel();
    let error = thread.join().expect("join").expect_err("cancel");
    assert_eq!(error, VoiceProviderError::Cancelled);
}

#[test]
fn crashed_process_and_oversized_frame_are_bounded_errors() {
    let mut crashed = ProcessSidecarTransport::spawn(config("crash")).expect("spawn");
    let error = crashed
        .exchange(doctor_frame(), &OperationContext::new())
        .expect_err("crash");
    assert_eq!(
        error,
        VoiceProviderError::provider_failure("sidecar_exited", true)
    );

    let mut oversized = ProcessSidecarTransport::spawn(config("oversized")).expect("spawn");
    let error = oversized
        .exchange(doctor_frame(), &OperationContext::new())
        .expect_err("oversized");
    assert_eq!(
        error,
        VoiceProviderError::provider_failure("sidecar_frame_too_large", false)
    );
}

#[test]
fn configuration_and_wire_boundaries_are_rejected() {
    assert!(
        ProcessSidecarConfig::new("fixture")
            .expect("config")
            .max_frame_bytes(255)
            .is_err()
    );
    assert!(
        ProcessSidecarConfig::new("fixture")
            .expect("config")
            .timeout(Duration::ZERO)
            .is_err()
    );
    let config = ProcessSidecarConfig::new("fixture")
        .expect("config")
        .max_frame_bytes(256 * 1024)
        .expect("bounded frame");
    assert_eq!(config.max_frame_bytes_value(), 256 * 1024);
}

#[test]
fn debug_configuration_does_not_expose_argument_values() {
    let config = ProcessSidecarConfig::new("fixture")
        .expect("config")
        .arg("super-secret-token")
        .expect("argument");
    assert!(!format!("{config:?}").contains("super-secret-token"));
}
