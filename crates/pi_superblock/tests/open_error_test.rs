use std::error::Error;
use std::io;

use pi_superblock::{OpenError, RecoveryErrorContext, RecoveryFailureKind, RecoveryOperation};

#[test]
fn not_initialized_has_stable_display_and_no_source() {
    let error = OpenError::NotInitialized;

    assert_eq!(error.to_string(), "superblock is not initialized");
    assert!(error.source().is_none());
}

#[test]
fn recovery_failed_display_and_source_preserve_context() {
    let source_text = "checkpoint payload is truncated";
    let error = OpenError::RecoveryFailed {
        kind: RecoveryFailureKind::Decode,
        context: RecoveryErrorContext {
            device_key: None,
            operation: RecoveryOperation::DecodeCheckpoint,
            source: Box::new(io::Error::new(io::ErrorKind::InvalidData, source_text)),
        },
    };

    assert_eq!(
        error.to_string(),
        "superblock recovery failed during DecodeCheckpoint (Decode): checkpoint payload is truncated"
    );

    let source = error
        .source()
        .expect("RecoveryFailed must expose its source");
    let io_error = source
        .downcast_ref::<io::Error>()
        .expect("RecoveryFailed source must retain the concrete io::Error");
    assert_eq!(io_error.kind(), io::ErrorKind::InvalidData);
    assert_eq!(io_error.to_string(), source_text);
    assert!(io_error.source().is_none());
}
