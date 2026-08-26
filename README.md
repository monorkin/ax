# ax

Multi-account switcher for Claude Code, in Rust. A simpler, CLI-only take on
[claude-swap](https://github.com/realiti4/claude-swap): switch between Claude
accounts without logging out, let it switch for you before a rate limit, and
run accounts in parallel per terminal or per directory. Linux only.

## Usage

```bash
ax account add                                  # store the currently logged-in account
ax account add --token sk-ant-oat01-... \
               --email me@example.com --alias work
ax account list                                 # all accounts, active one marked *
ax account alias 2 work                         # alias an account
ax account remove work                          # remove a stored account

ax switch work                                  # switch the default login (number, email, or alias)
ax auto-switch                                  # watch usage, switch before hitting a limit
ax auto-switch --threshold 80 --once            # single check, for cron

ax run --account work -- --dangerously-skip-permissions
ax map ./client-app --to work                   # bare `ax run` in that dir launches `work`
ax mapping list
ax mapping remove ./client-app
```

To add more accounts: log into Claude Code with the next account and run
`ax account add` again. Don't `/logout` first — Claude Code may revoke the
stored refresh token of the account you're leaving.

## How it works

- `add` snapshots the live login (`~/.claude/.credentials.json` plus the
  identity in `~/.claude.json`) into `~/.local/share/ax/`.
- `switch` backs up the outgoing login into its slot, then writes the target's
  credentials and identity. Machine-shared state (MCP server logins, plugin
  secrets) stays live instead of being overwritten by a slot's older snapshot.
  Claude Code re-reads credentials when the file changes, so a running session
  picks up the switch without a restart.
- Switches hold Claude Code's own credential and config locks (the
  `proper-lockfile` directory protocol on `.oauth_refresh.lock`,
  `~/.claude.lock`, and `~/.claude.json.lock`), so a swap never interleaves
  with a token refresh.
- `run` gives the account its own profile under `~/.local/share/ax/sessions/`
  and launches `claude` with `CLAUDE_CONFIG_DIR` pointing at it — the default
  login stays untouched. Settings, CLAUDE.md, skills, commands, and agents are
  shared from `~/.claude`; user-scope MCP servers are mirrored on every
  launch; chat history stays per-profile.
- `auto-switch` polls the usage API and, once the active account's 5-hour or
  7-day window crosses the threshold (default 90%), moves to the account with
  the most headroom — with a hysteresis margin and a cooldown so two accounts
  hovering at the line never ping-pong.

## Building

```bash
cargo build --release   # → target/release/ax
```

## License

MIT — see [LICENSE](LICENSE).
