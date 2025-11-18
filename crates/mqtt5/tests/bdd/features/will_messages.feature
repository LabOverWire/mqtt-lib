Feature: Will Messages (Last Will and Testament)
  As a CLI user
  I want to send will messages when disconnecting unexpectedly
  So that other clients can detect my absence

  Scenario: Will message is delivered on disconnect
    Given a broker is running
    When I subscribe to "will/topic" expecting 1 message
    And I publish with will message "offline" on topic "will/topic"
    Then I should receive "offline"

  Scenario: Will delay interval works correctly
    Given a broker is running
    When I subscribe to "will/delayed" expecting 1 message
    And I publish with will message "delayed-offline" on topic "will/delayed" with delay 2
    And I wait 500 milliseconds
    Then the subscriber should not have received any messages yet
    When I wait 2000 milliseconds
    Then I should receive "delayed-offline"

  Scenario: Will QoS is respected
    Given a broker is running
    When I subscribe to "will/qos1" with QoS 1 expecting 1 message
    And I publish with will message "qos1-offline" on topic "will/qos1" with QoS 1
    Then I should receive "qos1-offline"
