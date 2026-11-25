# Setting up Jira Desk

This connects Jira Desk to your Jira, with the API token kept in your Mac's Keychain. It takes
about five minutes. To look around first without any of this: `jira-desk --demo`.

You'll end up with:

- a `config.toml` in Jira Desk's data folder, saying which site and which issues;
- an API token in your login Keychain, under the name `jira-desk`;
- `token_command` in `config.toml`, which reads the token from the Keychain each time Jira Desk
  connects. The token itself is never written to the data folder.

## 1. Create an API token (Jira Cloud)

1. Go to <https://id.atlassian.com/manage-profile/security/api-tokens>, signed in with the
   account you use for Jira.
2. Choose **Create API token** (not "Create API token with scopes": those only work through a
   different address, which Jira Desk doesn't use).
3. Name it `Jira Desk`, and pick an expiry. Atlassian tokens expire (a year at most); you'll
   repeat steps 1 and 3 then.
4. Copy the token. Atlassian shows it only once. Leave the page open until step 3 is done.

The token acts as you: it can see and change whatever your account can in Jira. Jira Desk only
changes things when you apply its queue, but treat the token like a password.

**Jira Server or Data Center** uses a personal access token instead: in Jira, open your
profile (your avatar, then **Profile**), then **Personal Access Tokens**, then **Create token**.
Then follow the same steps, and see [Server and Data Center](#server-and-data-center) for the
config.

> Some companies turn off API tokens for managed accounts, or only allow them from the office
> network or VPN. If the token is refused (step 5), ask your Jira admins whether API tokens are
> allowed.

## 2. Find the data folder and make the config

Run Jira Desk once (or `jira-desk --where`). It creates its data folder and puts
`config.example.toml` in it:

```bash
open "$HOME/Library/Application Support/Jira Desk"     # on a Mac
```

Copy `config.example.toml` to `config.toml` in the same folder and fill it in:

```toml
site = "https://your-site.atlassian.net"     # the address you open Jira at
email = "you@example.com"                     # the account the token belongs to
token_command = "security find-generic-password -w -s jira-desk"
jql = "project = ABC AND (sprint in openSprints() OR sprint in futureSprints() OR updated >= -30d)"
refresh_minutes = 10
max_image_mb = 500                            # optional: the most images may take
```

- `site`: just the address, no path (not `…/jira/software/…`). It must start with `https://`:
  the token goes with every request. A Jira Server reachable only over `http://` also needs
  `allow_insecure_http = true`, which sends the token unencrypted; use it only on a network
  you trust.
- `email`: exactly as you sign in to Atlassian; it pairs with the token.
- `jql`: which issues to keep locally. Try it in Jira's issue search first (Filters, then
  switch to JQL): what that search shows is what Jira Desk downloads. Keep it to what you work
  with; every issue in it is kept on your computer.
- `max_image_mb` (optional, 500 if left out): the downloaded images of issues you open are
  kept up to this size; past it, those you opened least recently are deleted and download
  again if you open them. 0 means no limit.
- `[[status_group]]` (optional): your own status chips over the List (which statuses go
  together, and in what order). See "Status chips" in the README.

## 3. Put the token in the Keychain

In Terminal:

```bash
security add-generic-password -s jira-desk -a you@example.com -w
```

It asks for the password twice: paste the token both times (nothing shows as you paste). This
adds an item named `jira-desk` to your login Keychain. You can see it in the **Keychain
Access** app (or **Passwords**), under that name.

Typing the token at the prompt, rather than after `-w` on the command line, keeps it out of
your shell history.

Check that it reads back (this prints the token, so only where no one is looking):

```bash
security find-generic-password -w -s jira-desk
```

Because the item was made by the `security` command, Jira Desk reading it through the same
command doesn't ask for permission each time.

## 4. Check the token works

```bash
curl -s -u "you@example.com:$(security find-generic-password -w -s jira-desk)" \
  https://your-site.atlassian.net/rest/api/3/myself | head -c 300
```

You should see your name (`"displayName": …`). If not, see
[When it doesn't work](#when-it-doesnt-work).

## 5. Start Jira Desk

Open Jira Desk. The status bar shows the first refresh downloading your issues ("Downloaded
120 of 340…"), then "Synced just now". From then on it refreshes in the background every
`refresh_minutes`.

Or from Terminal, with the `jdesk` command that comes with it (in the app's `Contents/MacOS`):

```bash
jdesk sync              # downloads, then says how many
jdesk show ABC-123      # one issue, as Jira Desk sees it
```

## Changing or removing the token

When the token expires or you make a new one:

```bash
security add-generic-password -U -s jira-desk -a you@example.com -w     # -U replaces it
```

To remove it: `security delete-generic-password -s jira-desk`. To stop Jira Desk using it
straight away, also revoke it on the Atlassian page from step 1.

## Other places to keep the token

`token_command` can be any command that prints the token. Jira Desk runs it with `/bin/sh`
each time it connects (on launch and each refresh).

**1Password** (with its command-line tool, `op`, signed in):

```toml
token_command = "/opt/homebrew/bin/op read op://Private/Jira/credential"
```

Give the full path: an app opened from the Finder or Dock doesn't get your shell's `PATH`, so a
command found in Terminal may not be found there. `which op` prints the path.

**Linux** (GNOME Keyring, KWallet, anything with the Secret Service):

```bash
secret-tool store --label="Jira Desk" service jira-desk account you@example.com
```

```toml
token_command = "secret-tool lookup service jira-desk account you@example.com"
```

**An environment variable:** `JIRA_API_TOKEN`, if set, is used instead of `token_command`. It
only reaches Jira Desk when you start it from that shell (`jira-desk` in Terminal), not from
the Finder or Dock, so the Keychain is the better choice for everyday use.

## Server and Data Center

```toml
site = "https://jira.your-company.com"
api = "server"
token_command = "security find-generic-password -w -s jira-desk"
jql = "project = ABC AND resolution = Unresolved"
```

No `email`: the personal access token is sent on its own. Store it the same way (step 3). If
your Jira is only reachable on the company network, Jira Desk needs the VPN like your browser
does. A proxy in `HTTPS_PROXY` is used if set.

## When it doesn't work

Jira Desk shows the problem in the status bar ("Offline: …"); `jdesk sync` prints it in full.

| Message | Meaning and fix |
|---|---|
| `no settings yet: copy config.example.toml to config.toml` | Step 2 isn't done, or the file isn't in the data folder (`jira-desk --where`). |
| `no token: set JIRA_API_TOKEN or token_command` | `config.toml` has no `token_command`. |
| `token_command failed: … could not be found in the keychain` | The Keychain item isn't named `jira-desk` (the `-s` name must match), or it's in another keychain. Redo step 3. |
| `token_command failed: … command not found` | Use the command's full path (see 1Password above). |
| `token_command printed nothing` | The item exists but is empty. Redo step 3 with `-U`. |
| `401` | Jira refused the email and token: a typo in `email`, a token pasted wrong, or an expired or revoked token. Check with step 4. |
| `403` | Signed in, but not allowed: your company may block API tokens, or allow them only from certain networks. Ask your Jira admins. |
| `400 … jql` | The JQL is invalid. Paste it into Jira's issue search to see why. |
| Nothing downloads, but no error | The JQL matches no issues. Try it in Jira's issue search. |
| Certificate errors | Jira Desk trusts the certificates your Mac trusts. Behind a company proxy that inspects traffic, its certificate must be installed in the System keychain. |

Jira Desk never writes the token anywhere: not to `config.toml`, the cache or its logs. If a
refresh fails, your local copy, queued changes and notes are untouched; it tries again at the
next refresh (or ⌘R).
