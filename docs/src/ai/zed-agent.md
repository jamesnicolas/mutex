---
title: Mutex Agent
description: Use Mutex's native AI agent with Mutex-configured models, tools, profiles, skills, instructions, and MCP servers.
---

# Mutex Agent

Mutex Agent is Mutex's native agent path. It runs in the [Agent Panel](./agent-panel.md) and [Threads Sidebar](./parallel-agents.md#threads-sidebar), uses models configured through [LLM Providers](./llm-providers.md), and integrates with Mutex's project, editor, terminal, and review surfaces.

Use Mutex Agent when you want the agent to:

- read and search your project
- edit files
- run terminal commands
- use Mutex-managed MCP tools
- follow [Agent Profiles](./agent-profiles.md)
- use Mutex [Skills](./skills.md) and [Instructions](./instructions.md)
- show changes in Mutex's review UI

## What Mutex Agent Uses {#what-zed-agent-uses}

| Capability                 | Source of truth                           |
| -------------------------- | ----------------------------------------- |
| Model access               | [LLM Providers](./llm-providers.md)       |
| Panel workflow             | [Agent Panel](./agent-panel.md)           |
| Tool availability          | [Agent Profiles](./agent-profiles.md)     |
| Tool approval behavior     | [Tool Permissions](./tool-permissions.md) |
| Built-in tools             | [Tools](./tools.md)                       |
| External tools             | [MCP](./mcp.md)                           |
| Reusable task instructions | [Skills](./skills.md)                     |
| Always-on instructions     | [Instructions](./instructions.md)         |

## How It Differs from Other Agent Paths {#other-agent-paths}

| Agent path                                | Main difference                                                                              |
| ----------------------------------------- | -------------------------------------------------------------------------------------------- |
| [Mutex Agent](./zed-agent.md)               | Uses Mutex's model, tool, profile, skill, instruction, and MCP configuration                   |
| [External Agents](./external-agents.md)   | Use an ACP integration and often own auth, model, tool, and native instruction configuration |
| [Terminal Threads](./terminal-threads.md) | Run a CLI/TUI in a terminal-backed thread; the CLI owns auth and configuration               |

See [Agents](./agents.md) for the full comparison.
