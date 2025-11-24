Feature: TLS Transport
  As a CLI user
  I want to use TLS encrypted connections
  So that my MQTT communication is secure

  Scenario: Basic TLS connection works
    Given a broker with TLS is running
    When I publish "secure message" to "test/tls" using TLS
    Then the publish command should succeed

  Scenario: TLS publish and subscribe
    Given a broker with TLS is running
    When I subscribe to "test/tls-pubsub" expecting 1 message using TLS
    And I publish "encrypted data" to "test/tls-pubsub" using TLS
    Then I should receive "encrypted data"

  Scenario: QoS works over TLS
    Given a broker with TLS is running
    When I subscribe to "test/tls-qos" with QoS 1 expecting 1 message using TLS
    And I publish "qos1-secure" to "test/tls-qos" with QoS 1 using TLS
    Then I should receive "qos1-secure"
