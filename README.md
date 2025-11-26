# ADNT - ADNT Tools Manager

A dynamic CLI tools manager written in Rust that automatically discovers, installs, updates, and manages all ADNT tools from the ADNTIO GitHub organization.

**Key Features:** Auto-discovery of tools, seamless installation on first use, automatic version checking, GitHub OAuth authentication, and zero-configuration tool management.

## Features

- **Automatic tool discovery** - Scans ADNTIO GitHub organization for all `adnt-*` repositories
- **Automatic installation** - Installs tools on first use from GitHub
- **Version checking** - Automatically checks for updates before running
- **Force update** - Option to force rebuild even when up to date
- **Installation time tracking** - Displays time taken for installation/updates
- **Centralized management** - All tools stored in `~/.adnt/tools`
- **Dynamic tool execution** - Run any ADNT tool without hardcoding
- **GitHub authentication** - Support for GitHub tokens via multiple methods
- **Rate limit friendly** - Uses authentication to avoid GitHub API limits

## Installation

### Recommended method (from Git)

```bash
cargo install --git https://github.com/ADNTIO/adnt.git
```

This command automatically compiles and installs `adnt` in `~/.cargo/bin/` (make sure this directory is in your PATH).

### Local development installation

```bash
cargo install --path .
```

### Manual installation

```bash
cargo build --release
sudo cp target/release/adnt /usr/local/bin/
```

## Usage

### List available tools

Discover all available ADNT tools from the ADNTIO GitHub organization:

```bash
adnt list
```

This will display all repositories starting with `adnt-` and show which ones are installed.

### Run any tool
ADNTIO/qrids-mockup


Run any ADNT tool by name (automatically installs if not present):

```bash
adnt run <tool-name> [args...]
```

Example:
```bash
adnt run net-edge --help
```

### Force update

Force update a tool even if it's already up to date:

```bash
adnt --force run net-edge
adnt -f run net-edge
```

### Verbose mode

See all repositories from ADNTIO organization:

```bash
adnt --verbose list
adnt -v list
```

## GitHub Authentication

ADNT can use GitHub authentication to increase API rate limits and access private repositories.

### Quick Start - Interactive Login (Recommended)

The easiest way to authenticate is using OAuth Device Flow:

```bash
adnt config login
```

This will:
1. Display a verification code
2. Open your browser to GitHub
3. Wait for you to enter the code and authorize
4. Automatically save the token


### Alternative Authentication Methods

The tool supports multiple authentication methods (in order of priority):

**Option 1: Interactive OAuth Login (Recommended)**
```bash
adnt config login
```

**Option 2: Manual Token Entry**
```bash
# Interactive (secure input)
adnt config set-token

# Or pass directly
adnt config set-token ghp_your_token_here
```

This saves the token to `~/.config/adnt/config.json`.

**Option 3: Environment Variable (for CI/CD)**
```bash
export GITHUB_TOKEN="ghp_your_token_here"
```

**Option 4: GitHub CLI**
```bash
gh auth login
```

If you have `gh` CLI installed and authenticated, ADNT will automatically use its token.

### Check Authentication Status

```bash
adnt config status
```

This shows:
- Whether a token is configured
- Which source is being used (env var, config file, or gh CLI)

### Creating a Manual GitHub Token

If you prefer to create a token manually instead of using `adnt config login`:

1. Go to https://github.com/settings/tokens
2. Click "Generate new token" → "Generate new token (classic)"
3. Give it a name (e.g., "ADNT Tool Manager")
4. Select scopes:
   - `repo` (for private repositories)
   - `read:org` (for organization repositories)
5. Click "Generate token"
6. Copy the token and configure it using `adnt config set-token`

**Note:**
- Authentication is **optional** for public repositories but recommended to avoid rate limiting
- `adnt config login` is easier and more secure than creating tokens manually
- For private repository access, see [OAUTH_SETUP.md](OAUTH_SETUP.md) to configure a custom OAuth App

### How it works

**On first execution:**
- Discovers the tool repository from GitHub
- Clones the repository to `~/.adnt/tools/adnt-<tool-name>`
- Compiles the tool in release mode
- Displays installation time
- Executes the tool with provided arguments

**On subsequent executions:**
- Checks if an update is available (compares git commits)
- Updates and recompiles if necessary
- Displays update time
- Executes the tool

**With --force flag:**
- Forces git pull and rebuild even if already up to date
- Useful for testing or after manual changes

## Structure

```
adnt/
├── src/
│   ├── main.rs           # CLI entry point and command routing
│   ├── tool_manager.rs   # Tool installation, update, and execution logic
│   └── github.rs         # GitHub API client for repository discovery
├── Cargo.toml
└── README.md
```

## Adding new tools

Simply create a new repository in the ADNTIO organization with the naming pattern `adnt-<tool-name>`. The tool will be automatically discovered and available via:

```bash
adnt run <tool-name>
```

**No code changes needed!** The tool manager automatically discovers and manages all `adnt-*` repositories.

## Dependencies

- `clap` - CLI argument parsing
- `tokio` - Async runtime
- `serde` / `serde_json` - State serialization
- `anyhow` - Error handling
- `colored` - Colored output
- `indicatif` - Progress bars
- `dirs` - System directories
- `chrono` - Date/time handling
- `reqwest` - HTTP client for GitHub API
- `open` - Browser launcher for OAuth flow

## Examples

```bash
# List all available tools
adnt list

# List with verbose mode (shows all repos)
adnt --verbose list

# Configure GitHub authentication (interactive OAuth)
adnt config login
adnt config status

# Run net-edge tool (installs if needed)
adnt run net-edge -- --help

# Force update before running
adnt --force run net-edge

# Run any discovered tool
adnt run my-custom-tool --some-arg value

# Combine flags
adnt -vf run net-edge

# Use with GitHub token from environment
GITHUB_TOKEN=ghp_xxx adnt list
```

## License

This project is licensed under the GNU General Public License v3.0 or later - see the [LICENSE](LICENSE) file for details.

Copyright (C) 2025 ADNT Sàrl <info@adnt.io>
