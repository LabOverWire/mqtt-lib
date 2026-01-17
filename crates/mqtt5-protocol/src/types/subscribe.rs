use super::QoS;

#[derive(Debug, Clone)]
pub struct SubscribeOptions {
    pub qos: QoS,
    pub no_local: bool,
    pub retain_as_published: bool,
    pub retain_handling: RetainHandling,
    pub subscription_identifier: Option<u32>,
}

impl Default for SubscribeOptions {
    fn default() -> Self {
        Self {
            qos: QoS::AtMostOnce,
            no_local: false,
            retain_as_published: false,
            retain_handling: RetainHandling::SendAtSubscribe,
            subscription_identifier: None,
        }
    }
}

impl SubscribeOptions {
    #[must_use]
    pub fn with_subscription_identifier(mut self, id: u32) -> Self {
        self.subscription_identifier = Some(id);
        self
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RetainHandling {
    SendAtSubscribe = 0,
    SendIfNew = 1,
    DontSend = 2,
}
