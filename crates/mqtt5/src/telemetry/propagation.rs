#[cfg(feature = "opentelemetry")]
use opentelemetry::{
    trace::{SpanContext, SpanId, TraceContextExt, TraceFlags, TraceId, TraceState},
    Context,
};

#[cfg(feature = "opentelemetry")]
use crate::protocol::v5::properties::{Properties, PropertyId, PropertyValue};

const TRACEPARENT_HEADER: &str = "traceparent";
const TRACESTATE_HEADER: &str = "tracestate";

pub type UserProperty = (String, String);

#[cfg(feature = "opentelemetry")]
pub fn extract_user_properties(properties: &Properties) -> Vec<UserProperty> {
    if let Some(values) = properties.get_all(PropertyId::UserProperty) {
        values
            .iter()
            .filter_map(|v| {
                if let PropertyValue::Utf8StringPair(k, v) = v {
                    Some((k.clone(), v.clone()))
                } else {
                    None
                }
            })
            .collect()
    } else {
        Vec::new()
    }
}

#[cfg(feature = "opentelemetry")]
pub fn inject_trace_context(user_properties: &mut Vec<UserProperty>) {
    let context = Context::current();
    let span = context.span();
    let span_context = span.span_context();

    if !span_context.is_valid() {
        return;
    }

    let traceparent = format!(
        "00-{}-{}-{:02x}",
        span_context.trace_id(),
        span_context.span_id(),
        span_context.trace_flags().to_u8()
    );

    user_properties.push((TRACEPARENT_HEADER.to_string(), traceparent));

    let trace_state_header = span_context.trace_state().header();
    if !trace_state_header.is_empty() {
        user_properties.push((TRACESTATE_HEADER.to_string(), trace_state_header));
    }
}

#[cfg(feature = "opentelemetry")]
pub fn extract_trace_context(user_properties: &[UserProperty]) -> Option<SpanContext> {
    let traceparent = user_properties
        .iter()
        .find(|(key, _)| key.eq_ignore_ascii_case(TRACEPARENT_HEADER))?;

    let parts: Vec<&str> = traceparent.1.split('-').collect();
    if parts.len() < 4 || parts[0] != "00" {
        tracing::warn!(
            traceparent = %traceparent.1,
            "Invalid traceparent format"
        );
        return None;
    }

    let trace_id = TraceId::from_hex(parts[1]).ok()?;
    let span_id = SpanId::from_hex(parts[2]).ok()?;
    let flags = u8::from_str_radix(parts[3], 16).ok()?;
    let trace_flags = TraceFlags::new(flags);

    let trace_state = user_properties
        .iter()
        .find(|(key, _)| key.eq_ignore_ascii_case(TRACESTATE_HEADER))
        .and_then(|(_, value)| {
            TraceState::from_key_value(vec![(value.clone(), String::new())]).ok()
        })
        .unwrap_or_default();

    Some(SpanContext::new(
        trace_id,
        span_id,
        trace_flags,
        true,
        trace_state,
    ))
}

#[cfg(feature = "opentelemetry")]
pub fn with_remote_context<F, R>(user_properties: &[UserProperty], f: F) -> R
where
    F: FnOnce() -> R,
{
    if let Some(span_context) = extract_trace_context(user_properties) {
        let context = Context::current().with_remote_span_context(span_context);
        let _guard = context.attach();
        f()
    } else {
        f()
    }
}

#[cfg(all(test, feature = "opentelemetry"))]
mod tests {
    use super::*;

    #[test]
    fn test_extract_valid_traceparent() {
        let user_properties = vec![(
            TRACEPARENT_HEADER.to_string(),
            "00-0af7651916cd43dd8448eb211c80319c-b7ad6b7169203331-01".to_string(),
        )];

        let result = extract_trace_context(&user_properties);
        assert!(result.is_some());

        let span_context = result.unwrap();
        assert!(span_context.is_valid());
    }

    #[test]
    fn test_extract_invalid_traceparent() {
        let user_properties = vec![(TRACEPARENT_HEADER.to_string(), "invalid-format".to_string())];

        let result = extract_trace_context(&user_properties);
        assert!(result.is_none());
    }

    #[test]
    fn test_extract_wrong_version() {
        let user_properties = vec![(
            TRACEPARENT_HEADER.to_string(),
            "99-00000000000000000000000000000001-0000000000000001-01".to_string(),
        )];

        let result = extract_trace_context(&user_properties);
        assert!(result.is_none());
    }

    #[test]
    fn test_inject_without_active_span() {
        let mut user_properties = Vec::new();
        inject_trace_context(&mut user_properties);

        assert!(user_properties.is_empty());
    }

    #[test]
    fn test_with_remote_context_no_trace() {
        let user_properties = vec![];

        let result = with_remote_context(&user_properties, || 42);

        assert_eq!(result, 42);
    }
}
