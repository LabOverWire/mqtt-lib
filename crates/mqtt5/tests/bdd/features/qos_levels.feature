Feature: QoS Levels
  As a CLI user
  I want to use different QoS levels
  So that I can control message delivery guarantees

  Scenario: QoS 0 - At most once delivery
    Given a broker is running
    When I subscribe to "test/qos0" expecting 1 message
    And I publish "QoS 0 message" to "test/qos0"
    Then I should receive "QoS 0 message"

  Scenario: QoS 1 - At least once delivery
    Given a broker is running
    When I subscribe to "test/qos1" with QoS 1 expecting 1 message
    And I publish "QoS 1 message" to "test/qos1" with QoS 1
    Then I should receive "QoS 1 message"
    And the publish command should succeed

  Scenario: QoS 2 - Exactly once delivery
    Given a broker is running
    When I subscribe to "test/qos2" with QoS 2 expecting 1 message
    And I publish "QoS 2 message" to "test/qos2" with QoS 2
    Then I should receive "QoS 2 message"
    And the publish command should succeed

  Scenario: Mixed QoS levels
    Given a broker is running
    When I subscribe to "test/mixed" with QoS 2 expecting 1 message
    And I publish "Mixed QoS" to "test/mixed" with QoS 1
    Then I should receive "Mixed QoS"
