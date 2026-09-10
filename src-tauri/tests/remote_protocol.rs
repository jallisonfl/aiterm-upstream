use aiterm_lib::remote::model::{
    encode_terminal_frame, validate_terminal_frame, ProtocolError, RemoteRequest, TerminalSize,
};
use aiterm_lib::tabs::{AttachmentId, TabId};
use serde::Serialize;

#[derive(Serialize)]
struct TestEnvelope<'a> {
    version: u16,
    request_id: u64,
    kind: &'a str,
    payload: &'a [u8],
}

fn cbor_request(version: u16, kind: &str, payload: &[u8]) -> Vec<u8> {
    let mut bytes = Vec::new();
    ciborium::into_writer(
        &TestEnvelope {
            version,
            request_id: 42,
            kind,
            payload,
        },
        &mut bytes,
    )
    .expect("test envelope should encode");
    bytes
}

#[test]
fn rejects_unsupported_protocol_version() {
    let err = RemoteRequest::decode(&cbor_request(99, "session.list", b"{}"))
        .expect_err("an incompatible peer must not be accepted");

    assert_eq!(err.code(), "protocol.unsupported_version");
}

#[test]
fn rejects_an_operation_the_protocol_does_not_define() {
    let err = RemoteRequest::decode(&cbor_request(1, "filesystem.write_anywhere", b"{}"))
        .expect_err("unknown operations must fail closed");

    assert_eq!(err.code(), "protocol.unknown_request");
}

#[test]
fn accepts_a_known_request_without_rewriting_its_identity_or_payload() {
    let request = RemoteRequest::decode(&cbor_request(1, "session.list", b"phone"))
        .expect("a version-one list request should decode");

    assert_eq!(request.request_id(), 42);
    assert_eq!(request.kind(), "session.list");
    assert_eq!(request.payload(), b"phone");
}

#[test]
fn accepts_the_live_conversation_cursor_through_the_wire_decoder() {
    let payload = serde_json::json!({"session_id": "codex-session", "after": 417});
    let mut encoded = Vec::new();
    ciborium::into_writer(&payload, &mut encoded).unwrap();
    let request = RemoteRequest::decode(&cbor_request(1, "session.spine", &encoded)).expect(
        "the phone must reach the live feed instead of falling back to conversation snapshots",
    );
    assert_eq!(request.request_id(), 42);
    assert_eq!(request.kind(), "session.spine");
    assert_eq!(request.payload(), encoded);
    let subscription = RemoteRequest::decode(&cbor_request(1, "session.spine.subscribe", &encoded))
        .expect("subscription must pass the same authenticated gateway decoder");
    assert_eq!(subscription.kind(), "session.spine.subscribe");
}

#[test]
fn rejects_terminal_size_outside_bounds() {
    assert_eq!(
        TerminalSize::try_new(0, 24).unwrap_err(),
        ProtocolError::invalid_terminal_size()
    );
    assert_eq!(
        TerminalSize::try_new(513, 24).unwrap_err(),
        ProtocolError::invalid_terminal_size()
    );
    assert_eq!(
        TerminalSize::try_new(80, 0).unwrap_err(),
        ProtocolError::invalid_terminal_size()
    );
    assert_eq!(
        TerminalSize::try_new(80, 513).unwrap_err(),
        ProtocolError::invalid_terminal_size()
    );
}

#[test]
fn terminal_size_keeps_valid_dimensions() {
    let size = TerminalSize::try_new(80, 24).expect("ordinary terminal dimensions are valid");

    assert_eq!(size.cols(), 80);
    assert_eq!(size.rows(), 24);
}

/// Taking focus is the one terminal action with no desktop equivalent: without
/// it a second client can attach but never type, which is the state the phone
/// spends most of its life in.
#[test]
fn accepts_an_explicit_take_focus_request() {
    let request = RemoteRequest::decode(&cbor_request(1, "terminal.focus", b""))
        .expect("taking input ownership is part of the terminal protocol");

    assert_eq!(request.kind(), "terminal.focus");
}

#[test]
fn accepts_tab_and_scrollback_requests() {
    for kind in [
        "tab.list",
        "tab.open",
        "tab.close",
        "terminal.scrollback",
        "terminal.resume",
    ] {
        let request = RemoteRequest::decode(&cbor_request(1, kind, b""))
            .expect("the typed terminal protocol should define this request");

        assert_eq!(request.kind(), kind);
    }
}

#[test]
fn accepts_terminal_upload_requests() {
    for kind in [
        "terminal.upload.begin",
        "terminal.upload.chunk",
        "terminal.upload.finish",
        "terminal.upload.cancel",
    ] {
        let request = RemoteRequest::decode(&cbor_request(1, kind, b""))
            .expect("image uploads are part of the version-one terminal protocol");

        assert_eq!(request.kind(), kind);
    }
}

#[test]
fn accepts_bounded_conversation_and_file_resource_requests() {
    for kind in ["session.conversation", "session.changes", "file.read"] {
        let request = RemoteRequest::decode(&cbor_request(1, kind, b""))
            .expect("structured phone resources are part of the remote protocol");
        assert_eq!(request.kind(), kind);
    }
}

#[derive(Serialize)]
struct EnvelopeWithUnknownField<'a> {
    version: u16,
    request_id: u64,
    kind: &'a str,
    payload: &'a [u8],
    unexpected: bool,
}

#[test]
fn strict_envelope_errors_retain_a_recoverable_request_id() {
    let mut bytes = Vec::new();
    ciborium::into_writer(
        &EnvelopeWithUnknownField {
            version: 1,
            request_id: 88,
            kind: "tab.list",
            payload: b"",
            unexpected: true,
        },
        &mut bytes,
    )
    .unwrap();

    let error = RemoteRequest::decode(&bytes).unwrap_err();

    assert_eq!(error.code(), "protocol.invalid_cbor");
    assert_eq!(error.request_id(), Some(88));
}

#[test]
fn request_envelope_rejects_trailing_cbor_and_garbage_with_correlation() {
    for suffix in [vec![0xf6], b"garbage".to_vec()] {
        let mut bytes = cbor_request(1, "tab.list", b"");
        bytes.extend(suffix);
        let error = RemoteRequest::decode(&bytes).unwrap_err();
        assert_eq!(error.code(), "protocol.invalid_cbor");
        assert_eq!(error.request_id(), Some(42));
    }
}

#[test]
fn wire_identifiers_require_canonical_lowercase_hyphenated_uuids() {
    #[derive(serde::Deserialize)]
    struct Ids {
        tab: TabId,
        attachment: AttachmentId,
    }

    for invalid in [
        "550E8400-E29B-41D4-A716-446655440000".to_owned(),
        "550e8400e29b41d4a716446655440000".to_owned(),
        "x".repeat(4096),
    ] {
        let value = serde_json::json!({
            "tab": invalid,
            "attachment": "550e8400-e29b-41d4-a716-446655440000"
        });
        assert!(serde_json::from_value::<Ids>(value).is_err());
    }

    let ids: Ids = serde_json::from_value(serde_json::json!({
        "tab": "550e8400-e29b-41d4-a716-446655440000",
        "attachment": "550e8400-e29b-41d4-a716-446655440001"
    }))
    .unwrap();
    assert_eq!(ids.tab.as_str(), "550e8400-e29b-41d4-a716-446655440000");
    assert_eq!(
        ids.attachment.as_str(),
        "550e8400-e29b-41d4-a716-446655440001"
    );
}

#[test]
fn terminal_frames_larger_than_one_mebibyte_are_rejected() {
    assert_eq!(
        validate_terminal_frame(&vec![0; 1024 * 1024 + 1])
            .unwrap_err()
            .code(),
        "protocol.frame_too_large"
    );
}

#[test]
fn terminal_frames_exactly_one_mebibyte_are_rejected() {
    assert_eq!(
        validate_terminal_frame(&vec![0; 1024 * 1024])
            .unwrap_err()
            .code(),
        "protocol.frame_too_large"
    );
}

#[derive(Serialize)]
struct OversizedTypedFrame {
    payload: Vec<u8>,
}

#[test]
fn typed_terminal_frames_stop_encoding_at_the_one_mebibyte_limit() {
    let err = encode_terminal_frame(&OversizedTypedFrame {
        payload: vec![0; 1024 * 1024 + 1],
    })
    .expect_err("oversized typed frame must not finish encoding");

    assert_eq!(err.code(), "protocol.frame_too_large");
}

#[derive(Serialize)]
struct RawTerminalSize {
    cols: u16,
    rows: u16,
}

#[test]
fn terminal_size_deserialization_rejects_invalid_json_and_cbor() {
    assert!(serde_json::from_str::<TerminalSize>(r#"{"cols":0,"rows":24}"#).is_err());
    assert!(serde_json::from_str::<TerminalSize>(r#"{"cols":80,"rows":513}"#).is_err());

    let mut cbor = Vec::new();
    ciborium::into_writer(
        &RawTerminalSize {
            cols: 513,
            rows: 24,
        },
        &mut cbor,
    )
    .expect("test CBOR should encode");
    let decoded: Result<TerminalSize, _> = ciborium::from_reader(cbor.as_slice());

    assert!(decoded.is_err());
}
