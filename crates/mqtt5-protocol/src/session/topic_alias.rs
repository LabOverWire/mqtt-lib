use crate::error::{MqttError, Result};
use crate::prelude::{HashMap, String, ToString};

#[derive(Debug)]
pub struct TopicAliasManager {
    topic_alias_maximum: u16,
    alias_to_topic: HashMap<u16, String>,
    topic_to_alias: HashMap<String, u16>,
    next_alias: u16,
}

impl TopicAliasManager {
    #[must_use]
    pub fn new(topic_alias_maximum: u16) -> Self {
        Self {
            topic_alias_maximum,
            alias_to_topic: HashMap::new(),
            topic_to_alias: HashMap::new(),
            next_alias: 1,
        }
    }

    #[must_use]
    pub fn get_or_create_alias(&mut self, topic: &str) -> Option<u16> {
        if let Some(&alias) = self.topic_to_alias.get(topic) {
            return Some(alias);
        }

        if self.topic_alias_maximum == 0
            || self.alias_to_topic.len() >= usize::from(self.topic_alias_maximum)
        {
            return None;
        }

        while self.alias_to_topic.contains_key(&self.next_alias)
            && self.next_alias <= self.topic_alias_maximum
        {
            self.next_alias += 1;
            if self.next_alias > self.topic_alias_maximum {
                self.next_alias = 1;
            }
        }

        let alias = self.next_alias;
        self.alias_to_topic.insert(alias, topic.to_string());
        self.topic_to_alias.insert(topic.to_string(), alias);

        self.next_alias += 1;
        if self.next_alias > self.topic_alias_maximum {
            self.next_alias = 1;
        }

        Some(alias)
    }

    /// # Errors
    /// Returns `TopicAliasInvalid` if alias is 0 or exceeds the maximum.
    pub fn register_alias(&mut self, alias: u16, topic: &str) -> Result<()> {
        if alias == 0 || alias > self.topic_alias_maximum {
            return Err(MqttError::TopicAliasInvalid(alias));
        }

        if let Some(old_topic) = self.alias_to_topic.get(&alias) {
            self.topic_to_alias.remove(old_topic);
        }

        self.alias_to_topic.insert(alias, topic.to_string());
        self.topic_to_alias.insert(topic.to_string(), alias);

        Ok(())
    }

    #[must_use]
    pub fn get_topic(&self, alias: u16) -> Option<&str> {
        self.alias_to_topic.get(&alias).map(String::as_str)
    }

    #[must_use]
    pub fn get_alias(&self, topic: &str) -> Option<u16> {
        self.topic_to_alias.get(topic).copied()
    }

    pub fn clear(&mut self) {
        self.alias_to_topic.clear();
        self.topic_to_alias.clear();
        self.next_alias = 1;
    }

    #[must_use]
    pub fn topic_alias_maximum(&self) -> u16 {
        self.topic_alias_maximum
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_topic_alias_basic() {
        let mut ta = TopicAliasManager::new(10);

        let alias1 = ta.get_or_create_alias("topic/1").unwrap();
        assert_eq!(alias1, 1);

        let alias2 = ta.get_or_create_alias("topic/2").unwrap();
        assert_eq!(alias2, 2);

        let alias1_again = ta.get_or_create_alias("topic/1").unwrap();
        assert_eq!(alias1_again, 1);

        assert_eq!(ta.get_topic(1), Some("topic/1"));
        assert_eq!(ta.get_alias("topic/1"), Some(1));
    }

    #[test]
    fn test_topic_alias_register() {
        let mut ta = TopicAliasManager::new(5);

        ta.register_alias(3, "remote/topic").unwrap();
        assert_eq!(ta.get_topic(3), Some("remote/topic"));

        assert!(ta.register_alias(0, "topic").is_err());
        assert!(ta.register_alias(6, "topic").is_err());

        ta.register_alias(3, "new/topic").unwrap();
        assert_eq!(ta.get_topic(3), Some("new/topic"));
        assert!(ta.get_alias("remote/topic").is_none());
    }

    #[test]
    fn test_topic_alias_limit() {
        let mut ta = TopicAliasManager::new(2);

        let alias1 = ta.get_or_create_alias("topic/1");
        let alias2 = ta.get_or_create_alias("topic/2");
        let alias3 = ta.get_or_create_alias("topic/3");

        assert!(alias1.is_some());
        assert!(alias2.is_some());
        assert!(alias3.is_none());
    }

    #[test]
    fn test_topic_alias_clear() {
        let mut ta = TopicAliasManager::new(10);

        let _ = ta.get_or_create_alias("topic/1");
        let _ = ta.get_or_create_alias("topic/2");
        ta.register_alias(5, "topic/5").unwrap();

        ta.clear();

        assert!(ta.get_topic(1).is_none());
        assert!(ta.get_topic(5).is_none());
        assert!(ta.get_alias("topic/1").is_none());
    }
}
