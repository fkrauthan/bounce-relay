# bounce-relay

A Rust-based email-to-webhook relay service that processes bounce notification emails from Postfix and delivers JSON payloads to configured webhook endpoints.

## Features

- Parses DSN (Delivery Status Notification) bounce emails
- Multi-database support (PostgreSQL, MySQL, SQLite)
- Reliable webhook delivery with exponential backoff retry
- HMAC-SHA512 signed payloads for authenticity verification
- Multiple webhook routes per domain
- User-specific and catch-all routing (e.g., `john@example.com` vs `*@example.com`)

## Installation

```bash
cargo build --release
```

The binary will be at `target/release/bounce-relay`.

## Usage

### Initialize the Database

```bash
bounce-relay init
```

Creates the required database tables (`email_routes` and `webhook_queue`).

### Process Incoming Emails

```bash
bounce-relay ingest
```

Reads an email from stdin, parses bounce information, and queues webhook deliveries.

### Run the Worker

```bash
bounce-relay worker
```

Background process that delivers queued webhooks with automatic retry on failure.

## Configuration

Configuration is loaded from (in priority order):
1. `./settings.toml`
2. `/etc/bounce-relay/settings.toml`
3. Custom file via `-c <path>`
4. Environment variables with `BOUNCE_RELAY_` prefix

### Example settings.toml

```toml
database_url = "postgres://user:pass@localhost/email_hook"

# Worker settings (optional)
worker_max_retries = 50
worker_max_delay_seconds = 1800
worker_api_timeout_seconds = 60
worker_interval_seconds = 5
worker_items_per_iteration = 50
```

### Environment Variables

```bash
export BOUNCE_RELAY_DATABASE_URL="sqlite:email_hook.db?mode=rwc"
export BOUNCE_RELAY_WORKER_MAX_RETRIES=50
```

## Database Setup

### Adding a Webhook Route

Routes can be configured for specific users or as catch-all routes for entire domains.

#### Catch-all Route (any user at domain)

```sql
INSERT INTO email_routes (domain, url, secret_token, is_active)
VALUES ('example.com', 'https://api.example.com/webhook/bounce', 'your-secret-key', true);
```

This route matches all emails to `*@example.com`.

#### User-specific Route

```sql
INSERT INTO email_routes (domain, user, url, secret_token, is_active)
VALUES ('example.com', 'john', 'https://api.example.com/webhook/john', 'johns-secret', true);
```

This route only matches emails to `john@example.com`.

#### Routing Behavior

- **Both routes fire**: If an email matches both a user-specific route and a catch-all route, webhooks are sent to both destinations.
- **Case-insensitive**: User matching is case-insensitive (`John@example.com` matches the `john` route).

## Webhook Payload

### Format

```json
{
  "event": "bounce",
  "timestamp": "2024-01-15T10:30:00Z",
  "message_id": "<original-message-id@example.com>",
  "from": "sender@example.com",
  "subject": "Original message subject",
  "email": "bounced-recipient@example.com",
  "reason": "550 5.1.1 User unknown",
  "status": "5.1.1",
  "action": "failed",
  "is_permanent": true
}
```

### Headers

Each webhook request includes:

| Header         | Description                          |
|----------------|--------------------------------------|
| `Content-Type` | `application/json`                   |
| `X-Timestamp`  | Unix timestamp (seconds)             |
| `X-Signature`  | Base64-encoded HMAC-SHA512 signature |

### Signature Verification

The signature is computed as:

```
HMAC-SHA512(secret_token, "{timestamp}.{payload}")
```

Example verification in Python:

```python
import hmac
import hashlib
import base64

def verify_signature(secret: str, timestamp: str, payload: str, signature: str) -> bool:
    message = f"{timestamp}.{payload}".encode()
    expected = hmac.new(secret.encode(), message, hashlib.sha512).digest()
    return hmac.compare_digest(base64.b64encode(expected).decode(), signature)
```

## Postfix Configuration

To configure Postfix to forward bounce emails to bounce-relay:

### 1. Create a Transport Map Entry

Edit `/etc/postfix/transport` and add:

```
bounces.yourdomain.com    bounce-relay:
```

Run `postmap /etc/postfix/transport` after editing.

### 2. Configure the Pipe Transport

Edit `/etc/postfix/master.cf` and add:

```
bounce-relay unix  -       n       n       -       -       pipe
  flags=F user=nobody argv=/usr/local/bin/bounce-relay ingest
```

Adjust the path to `bounce-relay` and the user as needed. The user must have read access to the configuration file and write access to the database.

### 3. Update main.cf

Edit `/etc/postfix/main.cf`:

```
transport_maps = hash:/etc/postfix/transport
```

### 4. Configure Bounce Handling

To forward all bounces to bounce-relay, configure Postfix to send bounce notifications to a specific address that routes through the pipe:

```
# In /etc/postfix/main.cf
notify_classes = bounce, delay, policy, protocol, resource, software
bounce_notice_recipient = bounces@bounces.yourdomain.com
```

### 5. Alternative: Direct Alias

For simpler setups, you can use an alias in `/etc/aliases`:

```
bounces: "|/usr/local/bin/bounce-relay ingest"
```

Run `newaliases` after editing, then configure Postfix to send bounces to this alias:

```
# In /etc/postfix/main.cf
notify_classes = bounce
bounce_notice_recipient = bounces
```

### 6. Reload Postfix

```bash
postfix reload
```

## Running the Worker as a Service

### systemd

Create `/etc/systemd/system/bounce-relay-worker.service`:

```ini
[Unit]
Description=Email Hook Webhook Worker
After=network.target

[Service]
Type=simple
User=bounce-relay
ExecStart=/usr/local/bin/bounce-relay worker
Restart=always
RestartSec=5

[Install]
WantedBy=multi-user.target
```

Enable and start:

```bash
systemctl enable bounce-relay-worker
systemctl start bounce-relay-worker
```

## License

See LICENSE file.
