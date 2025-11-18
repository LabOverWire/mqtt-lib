Feature: Basic Publish and Subscribe
  As a CLI user
  I want to publish and receive messages
  So that I can communicate via MQTT

  Scenario: Simple message delivery
    Given a broker is running
    When I subscribe to "test/simple" expecting 1 message
    And I publish "Hello World" to "test/simple"
    Then I should receive "Hello World"
    And the publish command should succeed

  Scenario: Publish to multiple topics
    Given a broker is running
    When I subscribe to "test/topic1" expecting 1 message
    And I publish "Message One" to "test/topic1"
    Then I should receive "Message One"
    And the publish command should succeed

  Scenario: Topic with wildcards using single-level
    Given a broker is running
    When I subscribe to "sensors/+/temperature" expecting 1 message
    And I publish "22.5" to "sensors/room1/temperature"
    Then I should receive "22.5"

  Scenario: Topic with wildcards using multi-level
    Given a broker is running
    When I subscribe to "devices/#" expecting 1 message
    And I publish "device data" to "devices/sensor/room1/temp"
    Then I should receive "device data"

  Scenario: Multiple subscribers on same topic
    Given a broker is running
    When I subscribe to "broadcast/message" expecting 1 message
    And I publish "Broadcast" to "broadcast/message"
    Then I should receive "Broadcast"
