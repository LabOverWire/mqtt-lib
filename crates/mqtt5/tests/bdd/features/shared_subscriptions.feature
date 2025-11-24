Feature: Shared Subscriptions
  As a CLI user
  I want to use shared subscriptions for load balancing
  So that multiple subscribers can share message load

  Scenario: Shared subscription distributes messages
    Given a broker is running
    When I subscribe to "$share/group1/test/shared" expecting 1 message
    And I publish "message-1" to "test/shared"
    And I publish "message-2" to "test/shared"
    Then I should receive "message"

  Scenario: Non-shared and shared subscriptions coexist
    Given a broker is running
    When I subscribe to "test/mixed" expecting 2 messages
    And I publish "first" to "test/mixed"
    And I publish "second" to "test/mixed"
    Then I should receive all 2 messages
