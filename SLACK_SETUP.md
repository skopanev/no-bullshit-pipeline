# Slack Integration Setup

## Quick Setup Guide

### 1. Create Slack App

1. Go to https://api.slack.com/apps
2. Click **Create New App** → **From scratch**
3. Name: `NBP Bot` (or whatever you want)
4. Select your workspace
5. Click **Create App**

### 2. Add Bot Token Scopes

1. Go to **OAuth & Permissions** in the left sidebar
2. Scroll to **Scopes** → **Bot Token Scopes**
3. Add these scopes:
   - `chat:write` - Post messages to channels
   - `chat:write.public` - Post to public channels without joining
   - `channels:read` - List public channels
   - `groups:read` - List private channels
   - `im:write` - Send direct messages
   - `users:read` - Find users by email
   - `users:read.email` - Read user emails (for DM by email)

### 3. Install App to Workspace

1. Scroll to **OAuth Tokens for Your Workspace**
2. Click **Install to Workspace**
3. Review permissions and click **Allow**
4. Copy the **Bot User OAuth Token** (starts with `xoxb-...`)

### 4. Add Token to NBP

1. Open NBP → **Settings** → **Integrations**
2. Click **Add Workspace**
3. Enter:
   - **Name**: Work Slack (or whatever)
   - **Bot Token**: paste the `xoxb-...` token
4. Click **Save**

### 5. Invite Bot to Channels

For private channels, you need to invite the bot:
```
/invite @NBP Bot
```

For public channels with `chat:write.public` scope, the bot can post without being invited.

## Usage in Pipelines

### Post to Channel

```json
{
  "name": "post_slack",
  "connector": "slack",
  "input": "meeting_notes",
  "config": {
    "integration_id": "work-slack",
    "target": "#team-updates"
  }
}
```

### Post to DM (by email)

```json
{
  "config": {
    "integration_id": "work-slack",
    "target": "alice@example.com"
  }
}
```

### Post to Thread

```json
{
  "config": {
    "integration_id": "work-slack",
    "target": "#team-updates",
    "thread_ts": "1234567890.123456"
  }
}
```

## Target Types

- **Channel name**: `#team-updates` or `team-updates`
- **Channel ID**: `C123456789`
- **User email**: `alice@example.com`
- **User ID**: `U123456789`
- **DM ID**: `D123456789`

The connector automatically resolves names/emails to IDs.

## Troubleshooting

### "not_in_channel" error
- Invite the bot to the channel: `/invite @NBP Bot`
- Or add `chat:write.public` scope for public channels

### "user_not_found" error
- Check the email is correct
- Add `users:read.email` scope

### "invalid_auth" error
- Token expired or revoked
- Remove and re-add the integration with a fresh token

## Security

Bot tokens are stored securely in macOS Keychain under:
- **Service**: `one.nbp.skk`
- **Account**: `slack:{integration_id}`

Only token metadata (workspace name) is stored in `~/.nbp/settings.json`.
