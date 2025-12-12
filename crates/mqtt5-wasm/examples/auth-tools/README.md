# MQTT Auth Tools

Browser-based tools for generating MQTT broker authentication files.

## Features

### Password Generator
- Create hashed credentials using Argon2 (same algorithm as `mqttv5 passwd` CLI)
- Add multiple users
- Copy or download generated password file

### ACL Rule Builder
- Define topic-based access control rules
- Support for wildcard usernames (`*`) and MQTT topic wildcards (`+`, `#`)
- Permissions: read, write, readwrite, deny
- Copy or download generated ACL file

## Running

```bash
cd examples
./build.sh
cd auth-tools
python3 -m http.server 8080
```

Open http://localhost:8080

## Output Format

### Password File (passwd.txt)
```
alice:$argon2id$v=19$m=19456,t=2,p=1$...
bob:$argon2id$v=19$m=19456,t=2,p=1$...
```

### ACL File (acl.txt)
```
user alice topic sensors/+ permission read
user bob topic actuators/# permission write
user * topic public/# permission readwrite
user * topic admin/# permission deny
```

## Usage with Native Broker

The generated files can be used directly with the native broker:

```bash
mqttv5 broker --password-file passwd.txt --acl-file acl.txt
```

Or in a configuration file:

```toml
[auth]
password_file = "passwd.txt"
acl_file = "acl.txt"
```
