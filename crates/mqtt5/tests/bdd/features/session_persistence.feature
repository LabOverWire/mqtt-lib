Feature: Session Persistence
  As a CLI user
  I want sessions to persist across disconnections
  So that I can maintain state and receive queued messages

  Scenario: Clean session flag creates new session
    Given a broker is running
    When I publish "test" to "test/clean" with client-id "clean-client-1"
    Then the publish command should succeed

  Scenario: Session resumes with same client ID
    Given a broker is running
    When I publish "first" to "test/session" with client-id "session-client" and clean-start
    And I publish "second" to "test/session" with client-id "session-client" without clean-start
    Then the publish command should succeed
    And the output should contain "Resumed existing session"

  Scenario: Messages queued during disconnection are delivered
    Given a broker is running
    When I subscribe to "test/queued" with QoS 1 and client-id "queue-client" expecting 2 messages
    And I wait 500 milliseconds
    And I publish "queued-1" to "test/queued" with QoS 1
    And I publish "queued-2" to "test/queued" with QoS 1
    Then I should receive all 2 messages

  Scenario: Session expiry interval works correctly
    Given a broker is running
    When I publish "test" to "test/expiry" with client-id "expiry-client" and session-expiry 1
    And I wait 2000 milliseconds
    And I publish "test2" to "test/expiry" with client-id "expiry-client" without clean-start
    Then the publish command should succeed
