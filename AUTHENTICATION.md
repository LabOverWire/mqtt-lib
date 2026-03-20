# Authentication and Authorization

## Overview

The broker supports four layers of security:

1. **Authentication** - Verifies client identity
2. **Authorization** - Controls topic access via ACL
3. **RBAC** - Groups permissions into roles
4. **Identity Injection** - Stamps publisher identity on messages

## Authentication Methods

### Anonymous Access

```bash
mqttv5 broker --allow-anonymous
```

### Password Authentication

Passwords stored with Argon2id hashing.

```bash
mqttv5 passwd alice passwd.txt
mqttv5 broker --auth-password-file passwd.txt
```

Password file format: `username:argon2id_hash` (one per line). Lines starting with `#` are comments.

### PLAIN SASL

Enhanced authentication method using RFC 4616 PLAIN SASL. Credentials are sent as three NUL-separated fields: `[authzid]\0username\0password` in the auth data field (authzid is typically empty). Requires the same password file as password authentication.

```bash
mqttv5 broker --auth-password-file passwd.txt
```

Clients connect with `auth_method: "PLAIN"` and provide credentials in the MQTT v5 enhanced authentication flow.

### SCRAM-SHA-256

Challenge-response authentication without transmitting passwords.

```bash
mqttv5 broker --auth-method scram --scram-file scram.txt
```

SCRAM credentials file format: `username:salt_b64:iterations:stored_key_b64:server_key_b64` (one per line, 5 colon-separated fields). Lines starting with `#` are comments.

Generate credentials with:

```bash
mqttv5 scram alice scram.txt
```

Default iteration count: 310,000. Authentication state per client expires after 60 seconds. Maximum 1,000 concurrent SCRAM handshakes.

### JWT Authentication

Stateless token verification with HS256, RS256, or ES256.

```bash
mqttv5 broker \
  --auth-method jwt \
  --jwt-algorithm rs256 \
  --jwt-key-file public.pem \
  --jwt-issuer "https://auth.example.com"
```

### Federated JWT

Multi-issuer support with automatic JWKS key refresh.

```bash
mqttv5 broker \
  --auth-method jwt-federated \
  --jwt-issuer "https://accounts.google.com" \
  --jwt-jwks-uri "https://www.googleapis.com/oauth2/v3/certs" \
  --jwt-fallback-key fallback.pem \
  --jwt-auth-mode identity-only
```

Federated JWT constructs user IDs in the format `issuer_domain:sub` (e.g., `accounts.google.com:12345`). The `sub` claim is validated: maximum 256 characters, no colons, no control characters. A custom prefix can replace the issuer domain via `--jwt-issuer-prefix`.

## Federated Authentication Modes

| Mode | Description |
|------|-------------|
| `identity-only` | IdP verifies identity, broker handles authorization |
| `claim-binding` | Map JWT claims to broker roles |
| `trusted-roles` | Trust role claims from IdP directly |

### IdentityOnly

Use with generic OAuth providers (Google, Auth0). Authorization via ACL.

### ClaimBinding

Map JWT claims to roles:

```bash
mqttv5 broker \
  --jwt-auth-mode claim-binding \
  --jwt-role-claim "email" \
  --jwt-role-map "@company.com:employee" \
  --jwt-default-roles "guest"
```

Claim patterns support: Equals, Contains, EndsWith, StartsWith, Regex, and Any. In JSON config, specify as `{"EndsWith": "@company.com"}` or `"Any"`.

### TrustedRoles

Trust roles from IdP (Keycloak, Azure AD):

```bash
mqttv5 broker \
  --jwt-auth-mode trusted-roles \
  --jwt-trusted-role-claim "realm_access.roles" \
  --jwt-trusted-role-claim "groups"
```

When no trusted role claims are configured, defaults to checking `roles`, `groups`, and `realm_access.roles`.

## Authorization (ACL)

### File Format

```
user alice topic sensors/# permission readwrite
user bob topic sensors/temperature permission read
user * topic public/# permission read

role admin topic # permission readwrite
role sensors topic sensors/# permission readwrite

assign alice admin
assign bob sensors
```

### Permissions

- `read` (or `subscribe`) - Subscribe only
- `write` (or `publish`) - Publish only
- `readwrite` (or `rw`, `all`) - Both
- `deny` (or `none`) - Explicit denial

### Permission Evaluation Order

1. Direct user rules (exact username match) checked first
2. Role-based rules checked next (deny overrides allow across roles)
3. Wildcard user rules (`user *`) checked last
4. Default permission applied if no rule matches (deny unless `allow_all` mode)

### Username Substitution (`%u`)

ACL topic patterns support `%u` as a placeholder for the authenticated username. The broker expands `%u` to the current user's identity before matching, enabling a single rule to scope every user to their own topic namespace.

```
user * topic $DB/u/%u/# permission readwrite
```

When `alice@gmail.com` publishes to `$DB/u/alice@gmail.com/nodes`, the pattern expands to `$DB/u/alice@gmail.com/#` and matches. Publishing to `$DB/u/bob@gmail.com/nodes` does not match.

`%u` works in both direct user rules and role-based rules:

```
role db-user topic $DB/u/%u/# permission readwrite
assign alice db-user
assign bob db-user
```

Anonymous clients (no authenticated username) never match patterns containing `%u`. Usernames containing MQTT special characters (`+`, `#`, `/`) are also excluded from `%u` expansion to prevent wildcard and topic-level injection.

`%u` can be combined with MQTT wildcards:

```
user * topic devices/%u/+/telemetry/# permission read
```

### Sender Identity Injection

The broker stamps two MQTT v5 user properties on every PUBLISH packet before routing:

- **`x-mqtt-sender`** - The authenticated username (`user_id`) of the publishing client. Any client-provided `x-mqtt-sender` property is stripped and replaced by the broker to prevent spoofing.
- **`x-mqtt-client-id`** - The MQTT `client_id` of the immediate publisher. Any client-provided `x-mqtt-client-id` property is stripped and replaced by the broker to prevent spoofing.

Anonymous clients (no authenticated identity) produce no `x-mqtt-sender` property. Internal/bridge messages also carry no `x-mqtt-sender`.

These are distinct from `x-origin-client-id`, which is an application-layer property set by intermediaries (e.g., event republishers) to track the original causation client through republish hops.

### Echo Suppression

When enabled, the broker skips delivery of a PUBLISH to a subscriber whose `client_id` matches the value of a configurable user property on the message. This prevents clients from receiving their own published messages when routed through intermediaries.

Default property key: `x-origin-client-id`. The suppression key is hot-reloadable via SIGHUP.

Echo suppression defaults to checking `x-origin-client-id` (not `x-mqtt-client-id`) because when an intermediary republishes on behalf of the original client, `x-mqtt-client-id` reflects the intermediary's client_id, not the originator.

### CLI Management

```bash
mqttv5 acl add alice "sensors/#" readwrite -f acl.txt
mqttv5 acl role-add admin "#" readwrite -f acl.txt
mqttv5 acl assign alice admin -f acl.txt
mqttv5 acl check alice "sensors/temp" write -f acl.txt
mqttv5 acl list -f acl.txt
```

## CLI Reference

### Authentication Options

| Option | Description |
|--------|-------------|
| `--allow-anonymous` | Allow unauthenticated connections |
| `--auth-password-file` | Password file path |
| `--auth-method` | password, scram, jwt, jwt-federated |
| `--scram-file` | SCRAM credentials file |
| `--jwt-algorithm` | hs256, rs256, es256 |
| `--jwt-key-file` | JWT secret or public key |
| `--jwt-issuer` | Required JWT issuer |
| `--jwt-audience` | Required JWT audience |
| `--jwt-clock-skew` | Clock skew tolerance in seconds (default: 60) |

### Federated JWT Options

| Option | Description |
|--------|-------------|
| `--jwt-jwks-uri` | JWKS endpoint URL |
| `--jwt-jwks-refresh` | JWKS refresh interval in seconds (default: 3600) |
| `--jwt-fallback-key` | Fallback key when JWKS unavailable |
| `--jwt-auth-mode` | identity-only, claim-binding, trusted-roles |
| `--jwt-role-claim` | Claim path for role extraction |
| `--jwt-role-map` | Claim-to-role mapping (repeatable) |
| `--jwt-default-roles` | Default roles for authenticated users (comma-separated) |
| `--jwt-trusted-role-claim` | Trusted role claim paths (repeatable) |
| `--jwt-session-scoped-roles` | Clear roles on disconnect |
| `--jwt-role-merge-mode` | merge or replace (deprecated, use `--jwt-auth-mode`) |
| `--jwt-issuer-prefix` | Custom prefix for user ID namespacing |
| `--jwt-config-file` | JSON config for multi-issuer |

### ACL Commands

| Command | Description |
|---------|-------------|
| `acl add <user> <topic> <perm> -f <file>` | Add user rule |
| `acl remove <user> [topic] -f <file>` | Remove rules |
| `acl list [user] -f <file>` | List rules |
| `acl check <user> <topic> <action> -f <file>` | Test permission |
| `acl role-add <role> <topic> <perm> -f <file>` | Add role rule |
| `acl role-remove <role> [topic] -f <file>` | Remove role |
| `acl role-list [role] -f <file>` | List role rules |
| `acl assign <user> <role> -f <file>` | Assign role |
| `acl unassign <user> <role> -f <file>` | Remove role assignment |
| `acl user-roles <user> -f <file>` | List user's roles |

### Password Commands

| Command | Description |
|---------|-------------|
| `passwd <user> [file]` | Add/update user (file optional with `-n`) |
| `passwd <user> [file] -b <pass>` | Batch mode |
| `passwd -D <user> <file>` | Delete user |
| `passwd -c <user> <file>` | Create new password file |
| `passwd -n <user>` | Output hash to stdout |

## Common Configurations

### Internal Users

```bash
mqttv5 broker \
  --auth-password-file passwd.txt \
  --acl-file acl.txt
```

### Google OAuth

```bash
mqttv5 broker \
  --auth-method jwt-federated \
  --jwt-issuer "https://accounts.google.com" \
  --jwt-jwks-uri "https://www.googleapis.com/oauth2/v3/certs" \
  --jwt-fallback-key fallback.pem \
  --jwt-audience "YOUR_CLIENT_ID.apps.googleusercontent.com" \
  --jwt-auth-mode identity-only \
  --acl-file acl.txt
```

### Keycloak

```bash
mqttv5 broker \
  --auth-method jwt-federated \
  --jwt-issuer "https://keycloak.example.com/realms/mqtt" \
  --jwt-jwks-uri "https://keycloak.example.com/realms/mqtt/protocol/openid-connect/certs" \
  --jwt-fallback-key fallback.pem \
  --jwt-auth-mode trusted-roles \
  --jwt-trusted-role-claim "realm_access.roles"
```

### Multi-Issuer (JSON Config)

```json
{
  "issuers": [
    {
      "name": "corporate",
      "issuer": "https://login.corp.example.com",
      "key_source": {
        "Jwks": {
          "uri": "https://login.corp.example.com/.well-known/jwks",
          "fallback_key_file": "corp-fallback.pem"
        }
      },
      "auth_mode": "TrustedRoles",
      "trusted_role_claims": ["groups"]
    },
    {
      "name": "public",
      "issuer": "https://accounts.google.com",
      "key_source": {
        "Jwks": {
          "uri": "https://www.googleapis.com/oauth2/v3/certs",
          "fallback_key_file": "google-fallback.pem"
        }
      },
      "audience": "YOUR_CLIENT_ID",
      "auth_mode": "IdentityOnly",
      "default_roles": ["public-user"]
    }
  ]
}
```

## Auth Provider Architecture

### ComprehensiveAuthProvider

The primary broker auth provider. Wraps `PasswordAuthProvider` + `AclManager` into a single provider handling both authentication and authorization. Factory methods: `from_files()`, `with_password_file_and_allow_all_acl()`, `with_providers()`. Supports file reloading for both password and ACL files.

### CompositeAuthProvider

Chains a primary and fallback auth provider. If the primary returns `BadAuthenticationMethod`, the fallback is tried. Other rejection reasons (e.g., `NotAuthorized`) are final.

Authorization modes control how publish/subscribe checks combine:

| Mode | Behavior |
|------|----------|
| `PrimaryOnly` (default) | Only primary provider's authorization checked |
| `Or` | Allowed if either provider authorizes |
| `And` | Allowed only if both providers authorize |

## Session Security

### Session-to-User Binding

Sessions store the authenticated `user_id`. When a client reconnects with `clean_start=false`, the broker verifies the reconnecting user matches the session owner. Mismatched users are rejected with `NotAuthorized`, preventing session hijacking where an attacker reconnects using a known client ID.

### ACL Re-Check on Session Restore

When restoring subscriptions from a previous session, each topic filter is re-authorized against current ACL rules. Subscriptions that no longer pass authorization are silently pruned from the restored session. This ensures ACL rule changes take effect even for persistent sessions.

## Security Notes

### Credentials

- Passwords hashed with Argon2id
- SCRAM never transmits passwords; client-side passwords zeroized on drop
- Password fields are not logged (no explicit tracing of credentials)
- Password/ACL files should have mode 0600
- Bridge config Debug output redacts password fields
- Enable TLS to protect credentials in transit

### JWT

- Tokens **must** include `exp` (expiration) and `sub` (subject) claims; tokens without either are rejected
- Verifier selection uses `kid` (key ID) header matching, not the `alg` header, preventing algorithm confusion attacks
- Single-verifier configurations ignore the token's `alg` header entirely
- JWKS endpoints must use HTTPS
- Claim pattern regexes compiled once at config load (invalid patterns fail early)
- Default clock skew tolerance: 60 seconds

### JWKS

- Background refresh with configurable interval (default: 3600s)
- Cache TTL configurable per endpoint (default: 86400s)
- Circuit breaker: after 3 consecutive failures, endpoint marked as open for 60 seconds before half-open retry
- Fallback static key used when JWKS endpoint is unavailable or key not found

### SCRAM

- Authentication state keyed by `client_id`; concurrent authentication for the same client ID is rejected
- Channel binding is **not** supported; clients using `y,,` (requesting) or `p=` (requiring) channel binding are rejected
- Constant-time comparison used for credential verification
- PBKDF2-HMAC-SHA256 key derivation

### Certificate Authentication

- `CertificateAuthProvider` validates TLS peer certificate fingerprints (64-char hex SHA-256)
- Client IDs starting with `cert:` are matched against registered fingerprints from the actual TLS connection
- The broker rejects `cert:` prefixed client IDs at the transport layer before authentication unless the connection has a verified TLS client certificate (`client_cert_info`). This prevents spoofing over plain TCP, WebSocket, or QUIC
- Fingerprints are case-insensitive and must be exactly 64 hex characters

### ACL

- Deny rules evaluated before allow rules within role-based checks
- `%u` substitution rejects usernames containing `+`, `#`, or `/` to prevent wildcard injection
- Topic names validated on publish after topic alias resolution

### Transport

- Rate limiting enabled by default (5 attempts per 60s, 5-minute lockout)
- Rate limiting tracks both IP address and username independently
- QUIC bridges default to certificate verification enabled
- WebSocket `allowed_origins` configuration prevents Cross-Site WebSocket Hijacking
- WebSocket path enforcement rejects connections to non-configured paths with HTTP 404

## Rate Limiting

Authentication rate limiting protects against brute-force attacks. Enabled by default with configurable limits. Tracks failed attempts by both IP address and username independently.

### Default Settings

| Setting | Default | Description |
|---------|---------|-------------|
| `max_attempts` | 5 | Failed attempts before lockout |
| `window_secs` | 60 | Time window for attempt counting |
| `lockout_secs` | 300 | Lockout duration after exceeding limit |

### Configuration

Rate limiting is configured via the `rate_limit` section in `AuthConfig`:

```rust
AuthConfig {
    rate_limit: RateLimitConfig {
        enabled: true,
        max_attempts: 5,
        window_secs: 60,
        lockout_secs: 300,
    },
    ..Default::default()
}
```

To disable rate limiting:

```rust
AuthConfig {
    rate_limit: RateLimitConfig {
        enabled: false,
        ..Default::default()
    },
    ..Default::default()
}
```

Successful authentication clears the attempt counter for that IP/username. Expired entries are cleaned up periodically.

## Security

### Transport Security

- TLS 1.2+ with certificate validation
- QUIC with built-in TLS 1.3 (certificate verification enforced by default)
- Mutual TLS (client certificates) with fingerprint validation
- WebSocket Origin validation (`allowed_origins`) for CSWSH prevention
- WebSocket path enforcement (rejects non-configured paths with HTTP 404)

### Authentication Security

- JWT tokens require `exp` and `sub` claims (reject tokens without expiration or subject)
- JWT algorithm confusion prevention via `kid`-based verifier selection
- SCRAM-SHA-256 rejects concurrent authentication for the same client ID
- SCRAM client passwords zeroized on drop
- Password fields excluded from log output
- Authentication rate limiting (5 attempts/60s, 5-minute lockout)
- Certificate auth validates TLS peer fingerprints (64-char hex) instead of trusting client ID prefix

### Session & Authorization Security

- Sessions bound to authenticated user identity (rejects reconnection from different user)
- ACL re-checked on session restore (prunes subscriptions that no longer pass authorization)
- Topic name validation on publish (after topic alias resolution)
- Bridge config Debug output redacts passwords

### Storage Security

- File storage uses percent-encoding for topic-to-filename mapping (bijective, no collisions)
- Atomic writes with fsync before rename for crash-safe durability
- `NoVerification` TLS bypass restricted to `pub(crate)` scope

### Example Security

- All WASM example HTML files use safe DOM manipulation (no innerHTML with user data)
