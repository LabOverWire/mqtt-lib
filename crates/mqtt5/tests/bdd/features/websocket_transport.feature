Feature: WebSocket Transport
  As a CLI user
  I want to use WebSocket connections
  So that I can connect from browser environments

  Scenario: Basic WebSocket connection works
    Given a broker with WebSocket is running
    When I publish "ws message" to "test/ws" using WebSocket
    Then the publish command should succeed

  Scenario: WebSocket publish and subscribe
    Given a broker with WebSocket is running
    When I subscribe to "test/ws-pubsub" expecting 1 message using WebSocket
    And I publish "websocket data" to "test/ws-pubsub" using WebSocket
    Then I should receive "websocket data"

  Scenario: QoS works over WebSocket
    Given a broker with WebSocket is running
    When I subscribe to "test/ws-qos" with QoS 1 expecting 1 message using WebSocket
    And I publish "qos1-ws" to "test/ws-qos" with QoS 1 using WebSocket
    Then I should receive "qos1-ws"
