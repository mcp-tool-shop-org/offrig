# Changelog

## 0.1.0 (unreleased)

First version, built and tested live on 2026-10-02.

- Core library: RunPod REST and GraphQL client, pod spec with a pinned Ollama image and
  an sshd bootstrap, SSH tunnel with orphan reclaim, on-pod model pulls that survive the
  client exiting, Zed settings edits through a JSONC syntax tree, guard checks, cost and
  idle tracking.
- `offrig` CLI: status, gpus, profiles, up, tunnel, pull, models, zed, zed-remove,
  guard, check, connect, down.
- `offrig-app` desktop app (egui): profiles and live prices, launch with runway check,
  pod status and cost, tunnel control, model pulls and tests, Zed wiring, guard checks,
  shutdown and close-with-pod confirmations, idle auto-stop.
- Three default tiers: small, medium, frontier.
- Renamed from podbay before release: the name collides with Podbay Cloud and podbay.fm.
