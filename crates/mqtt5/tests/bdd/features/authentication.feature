Feature: Authentication
  As a CLI user
  I want to authenticate with username and password
  So that my broker connections are secure

  Scenario: Valid credentials allow connection
    Given a broker with authentication is running
    When I set username to "testuser"
    And I set password to "testpass"
    And I publish "authenticated" to "test/auth"
    Then the publish command should succeed

  Scenario: Invalid credentials reject connection
    Given a broker with authentication is running
    When I set username to "baduser"
    And I set password to "badpass"
    And I publish "rejected" to "test/auth"
    Then the publish command should fail
    And the error should contain "BadUsernameOrPassword"

  Scenario: Missing credentials reject connection
    Given a broker with authentication is running
    When I publish "no-auth" to "test/auth"
    Then the publish command should fail
