# Changelog

All notable changes to this project are documented here. The format follows
[Keep a Changelog](https://keepachangelog.com/en/1.1.0/) and the project uses
[Semantic Versioning](https://semver.org/).

## [Unreleased]

## [0.2.0] - 2026-11-04

First public release. Earlier versions (0.1.x) were never published; the
**Changed** section only matters if you configured one of them.

### Added

- Catalog-driven MCP server (RMCP 3.5.1, stdio) for the EcoStruxure Control
  Expert 15.1 COM/UDE API. The executable embeds the catalog: 1,277 operations
  from 120 interfaces and 17 type libraries, 1,178 of them invocable through
  IDispatch.
- Headless sessions through the broker (`ce_session_start`), validated with
  `Version=15.1` and `IsVisible=0`, and an on-demand read-only viewer
  (`ce_viewer_open`).
- `ce_screens_export` and `ce_screens_import` for operator screens, which have
  no COM API and only travel inside `.XFM` files. The import backs up the
  module, restores it on failure and verifies a re-export.
- An execution policy fixed at startup: build, PLC connection, PLC write,
  security administration and private APIs stay blocked unless unlocked, and
  transfers without `CE_MCP_ALLOW_PLC_WRITE` are verified against the
  simulator immediately before the call.
- A Windows release build (`x86_64-pc-windows-msvc`, static CRT) attached to
  each GitHub release.

### Changed

- **Breaking:** the policy variables are now `CE_MCP_ALLOW_BUILD`,
  `CE_MCP_ALLOW_PLC_CONNECTION`, `CE_MCP_ALLOW_PLC_WRITE`,
  `CE_MCP_ALLOW_SECURITY_ADMIN` and `CE_MCP_ALLOW_PRIVATE_APIS`. The former
  `SCE_ALLOW_BUILD` and `SCE_ALLOW_PLC_TRANSFER` are no longer read, so a
  configuration that still uses them keeps everything blocked.

### Fixed

- `ce_screens_import` listed each changed screen twice in `differences` when
  two or more screens changed.

[Unreleased]: https://github.com/jcostasuarez/controlexpert-com-mcp/compare/v0.2.0...HEAD
[0.2.0]: https://github.com/jcostasuarez/controlexpert-com-mcp/releases/tag/v0.2.0
