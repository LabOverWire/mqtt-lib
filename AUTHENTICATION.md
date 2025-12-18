# Authentication and Authorization

## Overview

The broker supports three layers of security:

1. **Authentication** - Verifies client identity
2. **Authorization** - Controls topic access via ACL
3. **RBAC** - Groups permissions into roles

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

### SCRAM-SHA-256

Challenge-response authentication without transmitting passwords.

```bash
mqttv5 broker --auth-method scram --scram-file scram.txt
```

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

### TrustedRoles

Trust roles from IdP (Keycloak, Azure AD):

```bash
mqttv5 broker \
  --jwt-auth-mode trusted-roles \
  --jwt-trusted-role-claim "realm_access.roles" \
  --jwt-trusted-role-claim "groups"
```

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

- `read` - Subscribe only
- `write` - Publish only
- `readwrite` - Both
- `deny` - Explicit denial

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

### Federated JWT Options

| Option | Description |
|--------|-------------|
| `--jwt-jwks-uri` | JWKS endpoint URL |
| `--jwt-fallback-key` | Fallback key when JWKS unavailable |
| `--jwt-auth-mode` | identity-only, claim-binding, trusted-roles |
| `--jwt-role-claim` | Claim path for role extraction |
| `--jwt-role-map` | Claim-to-role mapping (repeatable) |
| `--jwt-trusted-role-claim` | Trusted role claim paths (repeatable) |
| `--jwt-session-scoped-roles` | Clear roles on disconnect |
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
| `acl assign <user> <role> -f <file>` | Assign role |
| `acl unassign <user> <role> -f <file>` | Remove role assignment |

### Password Commands

| Command | Description |
|---------|-------------|
| `passwd <user> <file>` | Add/update user |
| `passwd -b <user> <pass> <file>` | Batch mode |
| `passwd -D <user> <file>` | Delete user |

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

## Security Notes

- Passwords hashed with Argon2id
- SCRAM never transmits passwords
- JWKS endpoints must use HTTPS
- Password/ACL files should have mode 0600
- Deny rules evaluated before allow rules
- Enable TLS to protect credentials in transit
