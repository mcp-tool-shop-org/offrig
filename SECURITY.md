# Security

## Reporting

Report a vulnerability privately through GitHub's security advisories on this
repository, or by email to 64996768+mcp-tool-shop@users.noreply.github.com. Please do
not open a public issue for it.

## What offrig handles

- **Your RunPod API key**, read from the `RUNPOD_API_KEY` environment variable. offrig
  never writes it to disk, logs it, or sends it anywhere but RunPod's API.
- **SSH access to your pods**, with your own key. Pods allow key login only, and their
  sshd permits only local port forwarding.
- **Your Zed settings and SSH config**, which offrig edits in marked or named places
  only, keeping a backup of Zed's settings before the first change.

The design and its limits are in the README under "Threat model".
