#[must_use]
pub fn parse_shared_subscription(topic_filter: &str) -> (&str, Option<&str>) {
    if let Some(after_share) = topic_filter.strip_prefix("$share/") {
        if let Some(slash_pos) = after_share.find('/') {
            let group_name = &after_share[..slash_pos];
            let actual_filter = &after_share[slash_pos + 1..];
            return (actual_filter, Some(group_name));
        }
    }
    (topic_filter, None)
}

#[must_use]
pub fn strip_shared_subscription_prefix(topic_filter: &str) -> &str {
    parse_shared_subscription(topic_filter).0
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_shared_subscription() {
        assert_eq!(
            parse_shared_subscription("$share/workers/tasks/+"),
            ("tasks/+", Some("workers"))
        );
        assert_eq!(
            parse_shared_subscription("$share/group1/sensor/temperature"),
            ("sensor/temperature", Some("group1"))
        );
        assert_eq!(
            parse_shared_subscription("$share/mygroup/#"),
            ("#", Some("mygroup"))
        );
        assert_eq!(
            parse_shared_subscription("normal/topic"),
            ("normal/topic", None)
        );
        assert_eq!(parse_shared_subscription("#"), ("#", None));
        assert_eq!(
            parse_shared_subscription("$share/incomplete"),
            ("$share/incomplete", None)
        );
        assert_eq!(parse_shared_subscription(""), ("", None));
    }

    #[test]
    fn test_strip_shared_subscription_prefix() {
        assert_eq!(
            strip_shared_subscription_prefix("$share/workers/tasks/+"),
            "tasks/+"
        );
        assert_eq!(
            strip_shared_subscription_prefix("$share/group1/sensor/temp"),
            "sensor/temp"
        );
        assert_eq!(
            strip_shared_subscription_prefix("normal/topic"),
            "normal/topic"
        );
        assert_eq!(strip_shared_subscription_prefix("#"), "#");
        assert_eq!(
            strip_shared_subscription_prefix("$share/nofilter"),
            "$share/nofilter"
        );
    }
}
