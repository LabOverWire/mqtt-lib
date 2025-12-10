use crate::types::ReasonCode;

pub fn is_valid_publish_ack_reason_code(code: ReasonCode) -> bool {
    matches!(
        code,
        ReasonCode::Success
            | ReasonCode::NoMatchingSubscribers
            | ReasonCode::UnspecifiedError
            | ReasonCode::ImplementationSpecificError
            | ReasonCode::NotAuthorized
            | ReasonCode::TopicNameInvalid
            | ReasonCode::PacketIdentifierInUse
            | ReasonCode::QuotaExceeded
            | ReasonCode::PayloadFormatInvalid
    )
}

pub fn is_valid_pubrel_reason_code(code: ReasonCode) -> bool {
    matches!(
        code,
        ReasonCode::Success | ReasonCode::PacketIdentifierNotFound
    )
}
