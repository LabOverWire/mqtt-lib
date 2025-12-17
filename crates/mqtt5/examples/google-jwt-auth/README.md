# Google JWT Auth Example

Test federated JWT authentication with Google OAuth against the MQTT broker.

## Prerequisites

1. A Google Cloud project with OAuth 2.0 credentials
2. The MQTT broker built with WebSocket support

## Setup

### 1. Create Google OAuth Credentials

1. Go to [Google Cloud Console](https://console.cloud.google.com/apis/credentials)
2. Create a new project or select existing
3. Click **Create Credentials** → **OAuth client ID**
4. Select **Web application**
5. Add authorized JavaScript origins:
   - `http://localhost:8000` (for local testing)
   - `http://127.0.0.1:8000`
6. Copy the **Client ID** (looks like: `xxxx.apps.googleusercontent.com`)

### 2. Generate a Fallback Key

The broker requires a fallback key file for when JWKS is unavailable:

```bash
# Generate an RSA key pair (the broker needs the public key)
openssl genrsa -out /tmp/fallback-private.pem 2048
openssl rsa -in /tmp/fallback-private.pem -pubout -out /tmp/fallback-public.pem
```

### 3. Start the MQTT Broker

```bash
# Build the CLI
cargo build --release -p mqttv5-cli

# Start broker with Google federated auth + WebSocket
./target/release/mqttv5 broker \
  --auth-method jwt-federated \
  --jwt-issuer "https://accounts.google.com" \
  --jwt-jwks-uri "https://www.googleapis.com/oauth2/v3/certs" \
  --jwt-fallback-key /tmp/fallback-public.pem \
  --jwt-audience "YOUR_CLIENT_ID.apps.googleusercontent.com" \
  --ws-host 0.0.0.0:8080
```

Replace `YOUR_CLIENT_ID` with your actual Google Client ID.

### 4. Serve the HTML Application

```bash
# From the repository root
cd examples/google-jwt-auth
python3 -m http.server 8000
```

### 5. Test the Flow

1. Open http://localhost:8000 in your browser
2. Enter your Google Client ID
3. Click **Initialize Google Sign-In**
4. Sign in with Google
5. Click **Connect with JWT**
6. Subscribe and publish messages

## With ACL (Role-Based Access)

To test role-based access control based on JWT claims:

### Create an ACL file

```bash
cat > /tmp/mqtt-acl.txt << 'EOF'
# Roles
role admin
role user

# Admin can do everything
role admin topic # permission readwrite

# Users can only access their own topics
role user topic test/# permission readwrite

# Map Google users to roles (you'd normally map by email domain or groups)
user * role user
EOF
```

### Start broker with ACL

```bash
./target/release/mqttv5 broker \
  --auth-method jwt-federated \
  --jwt-issuer "https://accounts.google.com" \
  --jwt-jwks-uri "https://www.googleapis.com/oauth2/v3/certs" \
  --jwt-fallback-key /tmp/fallback-public.pem \
  --jwt-audience "YOUR_CLIENT_ID.apps.googleusercontent.com" \
  --acl-file /tmp/mqtt-acl.txt \
  --jwt-role-claim "email" \
  --jwt-role-map "@yourcompany.com:admin" \
  --jwt-default-roles "user" \
  --ws-host 0.0.0.0:8080
```

This maps users with `@yourcompany.com` email to the `admin` role.

## Multi-Issuer Configuration

For production with multiple identity providers, use a config file:

```bash
cat > /tmp/federated-jwt.json << 'EOF'
{
  "issuers": [
    {
      "name": "google",
      "issuer": "https://accounts.google.com",
      "key_source": {
        "Jwks": {
          "uri": "https://www.googleapis.com/oauth2/v3/certs",
          "fallback_key_file": "/tmp/fallback-public.pem",
          "refresh_interval_secs": 3600,
          "cache_ttl_secs": 86400
        }
      },
      "audience": "YOUR_GOOGLE_CLIENT_ID.apps.googleusercontent.com",
      "default_roles": ["user"],
      "role_mappings": [
        {
          "claim_path": "hd",
          "pattern": { "Equals": "yourcompany.com" },
          "assign_roles": ["admin"]
        }
      ]
    },
    {
      "name": "auth0",
      "issuer": "https://YOUR_TENANT.auth0.com/",
      "key_source": {
        "Jwks": {
          "uri": "https://YOUR_TENANT.auth0.com/.well-known/jwks.json",
          "fallback_key_file": "/tmp/fallback-public.pem",
          "refresh_interval_secs": 3600,
          "cache_ttl_secs": 86400
        }
      },
      "default_roles": ["user"]
    }
  ],
  "clock_skew_secs": 60
}
EOF

./target/release/mqttv5 broker \
  --auth-method jwt-federated \
  --jwt-config-file /tmp/federated-jwt.json \
  --acl-file /tmp/mqtt-acl.txt \
  --ws-host 0.0.0.0:8080
```

## Troubleshooting

### "Not Authorized" on connect
- Check that the `--jwt-audience` matches your Google Client ID exactly
- Verify the token hasn't expired (Google tokens are valid for ~1 hour)
- Check broker logs with `RUST_LOG=mqtt5=debug`

### JWKS fetch fails
- Ensure the broker can reach `www.googleapis.com`
- Check that the fallback key file exists and is readable

### WebSocket connection fails
- Verify the broker started with `--ws-host`
- Check for CORS issues in browser console
- Try using `ws://127.0.0.1:8080/mqtt` instead of `localhost`
