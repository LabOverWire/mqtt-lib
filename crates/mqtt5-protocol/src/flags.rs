//! MQTT packet flag definitions using `BeBytes` v2.1.0 flag decomposition

use bebytes::BeBytes;

/// Flags for MQTT CONNECT packet
#[derive(Debug, Clone, Copy, PartialEq, Eq, BeBytes)]
#[bebytes(flags)]
pub enum ConnectFlags {
    /// Reserved bit - must be 0
    Reserved = 0x01,
    /// Clean Start flag
    CleanStart = 0x02,
    /// Will Flag
    WillFlag = 0x04,
    /// Will `QoS` bit 0
    WillQoS0 = 0x08,
    /// Will `QoS` bit 1  
    WillQoS1 = 0x10,
    /// Will Retain flag
    WillRetain = 0x20,
    /// Password flag
    PasswordFlag = 0x40,
    /// Username flag
    UsernameFlag = 0x80,
}

impl ConnectFlags {
    /// Extract Will `QoS` value from flags
    #[must_use]
    pub fn extract_will_qos(flags: u8) -> u8 {
        (flags >> crate::constants::connect_flags::WILL_QOS_SHIFT)
            & crate::constants::connect_flags::WILL_QOS_MASK
    }

    #[must_use]
    /// Create flags byte with Will `QoS` value
    pub fn with_will_qos(mut flags: u8, qos: u8) -> u8 {
        // Clear existing QoS bits
        flags &= crate::constants::connect_flags::WILL_QOS_CLEAR_MASK;
        // Set new QoS bits
        flags |= (qos & crate::constants::connect_flags::WILL_QOS_MASK)
            << crate::constants::connect_flags::WILL_QOS_SHIFT;
        flags
    }
}

/// Flags for MQTT PUBLISH packet
#[derive(Debug, Clone, Copy, PartialEq, Eq, BeBytes)]
#[bebytes(flags)]
pub enum PublishFlags {
    /// Retain flag
    Retain = 0x01,
    /// `QoS` bit 0
    QoS0 = 0x02,
    /// `QoS` bit 1
    QoS1 = 0x04,
    /// Duplicate delivery flag
    Dup = 0x08,
}

impl PublishFlags {
    /// Extract `QoS` value from flags
    #[must_use]
    pub fn extract_qos(flags: u8) -> u8 {
        (flags >> crate::constants::publish_flags::QOS_SHIFT)
            & crate::constants::publish_flags::QOS_MASK
    }

    #[must_use]
    /// Create flags byte with `QoS` value
    pub fn with_qos(mut flags: u8, qos: u8) -> u8 {
        // Clear existing QoS bits
        flags &= crate::constants::publish_flags::QOS_CLEAR_MASK;
        // Set new QoS bits
        flags |= (qos & crate::constants::publish_flags::QOS_MASK)
            << crate::constants::publish_flags::QOS_SHIFT;
        flags
    }
}

/// Flags for MQTT CONNACK packet
#[derive(Debug, Clone, Copy, PartialEq, Eq, BeBytes)]
#[bebytes(flags)]
pub enum ConnAckFlags {
    /// Session Present flag
    SessionPresent = 0x01,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_connect_flags_decompose() {
        // Clean start + username + password
        let flags: u8 = 0xC2; // 11000010
        let decomposed = ConnectFlags::decompose(flags);

        assert_eq!(decomposed.len(), 3);
        assert!(decomposed.contains(&ConnectFlags::CleanStart));
        assert!(decomposed.contains(&ConnectFlags::UsernameFlag));
        assert!(decomposed.contains(&ConnectFlags::PasswordFlag));
    }

    #[test]
    fn test_publish_flags_decompose() {
        // DUP + QoS 2 + Retain = 0x0D (00001101)
        let flags: u8 = 0x0D;
        let decomposed = PublishFlags::decompose(flags);

        assert!(decomposed.contains(&PublishFlags::Retain));
        assert!(decomposed.contains(&PublishFlags::QoS1)); // QoS 2 = both bits set
        assert!(decomposed.contains(&PublishFlags::Dup));

        // Extract QoS
        assert_eq!(PublishFlags::extract_qos(flags), 2);
    }

    #[test]
    fn test_connack_flags() {
        let flags: u8 = 0x01;
        let decomposed = ConnAckFlags::decompose(flags);

        assert_eq!(decomposed.len(), 1);
        assert!(decomposed.contains(&ConnAckFlags::SessionPresent));
    }

    #[test]
    fn test_flag_iteration() {
        let flags: u8 = 0x0D; // DUP + QoS 2 + Retain

        let collected: Vec<_> = PublishFlags::iter_flags(flags).collect();
        assert_eq!(collected.len(), 3);
    }
}
