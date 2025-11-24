Feature: Retained Messages
  As a CLI user
  I want to use retained messages
  So that late subscribers can receive the last message

  Scenario: Publish and receive retained message
    Given a broker is running
    When the retained flag is set
    And I publish "Retained data" to "test/retained"
    And I wait 500 milliseconds
    And I subscribe to "test/retained" expecting 1 message
    Then I should receive "Retained data"
    And the publish command should succeed

  Scenario: Retained message persistence
    Given a broker is running
    When the retained flag is set
    And I publish "Persistent" to "config/settings"
    And I wait 500 milliseconds
    And I subscribe to "config/settings" expecting 1 message
    Then I should receive "Persistent"

  Scenario: Clear retained message with empty payload
    Given a broker is running
    When the retained flag is set
    And I publish "Initial" to "test/clear"
    And I wait 500 milliseconds
    And the retained flag is set
    And I publish "" to "test/clear"
    Then the publish command should succeed
