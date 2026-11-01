# ax

Multi-account switcher for Claude Code and Codex, in Rust. A simpler, CLI-only
take on [claude-swap](https://github.com/realiti4/claude-swap): switch between
accounts without logging out, let it switch for you before a rate limit, group
accounts into pools that rotate among themselves, and run accounts in parallel
per terminal or per directory. Linux only.

## Installing

With [mise](https://mise.jdx.dev):

```bash
mise use -g github:monorkin/ax                # install it and put ax on your PATH
mise exec github:monorkin/ax -- ax --help     # or run it once without installing
```

On [Omarchy](https://omarchy.org):

```bash
omarchy-mise-install github:monorkin/ax ax
```

## Usage

```bash
ax account add claude                           # store the current Claude Code login
ax account add claude --token sk-ant-oat01-... \
               --email me@example.com --alias work
ax account add codex                            # store the current Codex login
ax account list                                 # all accounts, active ones marked *
ax account alias 2 work                         # alias an account
ax account remove work                          # remove a stored account

ax switch work                                  # switch the default login (number, email, or alias)
ax auto-switch                                  # watch usage, switch before hitting a limit
ax auto-switch --threshold 80 --once            # single check, for cron

ax run --account work -- --dangerously-skip-permissions
ax map ./client-app --to work                   # `ax run` in that dir launches `work`
ax map ./client-app --to work-codex             # and `ax run codex` launches this one
ax mapping list
ax mapping remove ./client-app codex            # one tool's mapping, or both without a tool

ax pool add work 2 3 work2                      # group accounts, creating the pool if it's new
ax pool list                                    # * marks who each pool is logged in as
ax pool remove work 3                           # take an account out
ax pool remove work                             # or remove the pool

ax run --pool work                              # Claude Code on the pool's own login
ax run codex --pool work                        # Codex on the pool's own login
ax auto-switch --pool work                      # rotate that login among the pool's members

ax shell-completion install zsh                 # bash, elvish, zsh, fish, nu, or powershell
ax shell-completion print zsh                   # or write the script yourself
```

To add more accounts: log into Claude Code with the next account and run
`ax account add claude` again. Don't `/logout` first — Claude Code may revoke the
stored refresh token of the account you're leaving.

To add more Codex accounts: `codex login` with the next account, then
`ax account add codex` again. API key logins have no account to switch.

## Pools

A pool has its own login for each tool, under `~/.local/share/ax/pools/`. So
`work` and `private` can run and rotate side by side, each only among its own
accounts, and `~/.claude` and `~/.codex` stay as they are. Chat history stays in
the pool's folder across rotations. A pool can hold both Claude Code and Codex
accounts; each tool only ever rotates to its own.

`run --pool` starts on the member with the most left, or keeps the one the pool
is on unless it's past 90%. `auto-switch --pool` does the rotating while you
work, the same way it does for the default login, with its own cooldown.

An account can't safely be live in two logins, because refreshing its token in
one logs the other out. Auto-switching skips accounts live in another login,
`run --pool` won't start if that leaves nobody, and `switch` and `run --account`
warn. `ax` can't see a session in an account's own profile, so don't run
`--account work` while a pool is using `work`.

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

- Codex keeps its login in `~/.codex/auth.json` and `run` points `CODEX_HOME` at
  the profile, sharing `config.toml`, `AGENTS.md`, hooks, skills, rules, prompts,
  and plugins. Only the file credential store works, not the keyring. A switch
  applies to Codex sessions started after it; restart a running one to move it.

## Building

```bash
cargo build --release   # → target/release/ax
```

## License

MIT — see [LICENSE](LICENSE).
