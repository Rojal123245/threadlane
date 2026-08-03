# Threadlane CLI Login Design

## Goal

Add a provider login popup to the Threadlane CLI TUI so users can authenticate Codex, Antigravity, or OpenAI without leaving the terminal. OpenAI API keys must be pasteable into a masked input and must never appear in the transcript or normal screen output.

## User flow

`/login` opens a provider popup with three choices:

- Codex — reuse the existing OpenAI device-code flow.
- Antigravity — reuse the existing Google OAuth PKCE flow.
- OpenAI API key — switch to masked key entry.

Provider selection uses the existing completion-popup keyboard behavior: Up/Down, Tab, Enter, and Escape. OpenAI key entry accepts Crossterm paste events as one input operation, masks typed/pasted content, saves only after Enter, and clears the secret from UI state after save or cancel. The transcript receives only safe status text such as `OpenAI key saved` or an error without secret content.

## Architecture

`threadlane-auth` owns credential persistence and provider-specific authentication. Extend its existing credential model with an explicit OpenAI API-key field or adjacent secure storage, preserving existing Codex credentials and Antigravity credentials. The CLI owns only login popup state and flow orchestration in a dedicated login module; it does not parse, log, or persist secrets itself.

The runtime handles `/login` as a modal interaction rather than a normal command submission. While login is active, normal prompt input is paused. Browser/device flows run asynchronously so the TUI remains responsive, and completion is reported through a safe status message. Escape cancels provider selection or key entry without mutating stored credentials.

## Security and failure behavior

- Never render the raw OpenAI key, including on paste, error, or debug paths.
- Persist credentials under the existing `~/.threadlane` location with restrictive file permissions where supported.
- Empty keys are rejected without writing.
- Existing credentials remain intact if a new login fails or is cancelled.
- Provider errors are surfaced as bounded, secret-free status messages.
- Existing environment-variable credentials remain supported as a fallback.

## Verification

- Unit-test provider selection and login state transitions.
- Unit-test paste masking, empty-key rejection, cancel, and safe status output.
- Test credential persistence without printing secret values.
- Run `cargo test -p threadlane-cli`, relevant `threadlane-auth` tests, `cargo check -p threadlane-cli`, and `git diff --check`.
- Manually verify `/login`, provider navigation, masked paste, Escape, and successful key save in a real terminal.
