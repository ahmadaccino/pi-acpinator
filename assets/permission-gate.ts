import type { ExtensionAPI } from "@earendil-works/pi-coding-agent";

const READ_ONLY = new Set(["read", "grep", "find", "ls", "glob"]);

type Mode = "off" | "mutating" | "all";

function mode(): Mode {
  const m = (process.env.PI_ACP_APPROVAL_MODE ?? "mutating").toLowerCase();
  return m === "off" || m === "all" ? m : "mutating";
}

function detail(input: Record<string, unknown>): string {
  for (const key of ["command", "file_path", "path", "pattern", "query", "url"]) {
    const value = input[key];
    if (typeof value === "string" && value.trim()) return value.trim();
  }
  return "";
}

export default function (pi: ExtensionAPI) {
  // Sentinel command: lets the host confirm the gate loaded via `get_commands`.
  pi.registerCommand("acp-permission-gate", {
    description: "pi-acpinator permission gate (internal)",
    handler: async () => {},
  });

  pi.on("tool_call", async (event, ctx) => {
    const m = mode();
    if (m === "off") return;
    if (m === "mutating" && READ_ONLY.has(event.toolName)) return;

    const summary = detail((event.input ?? {}) as Record<string, unknown>);
    const message = summary ? `${event.toolName}: ${summary}` : `Tool: ${event.toolName}`;
    if (!ctx.hasUI) {
      return { block: true, reason: "Permission required but no UI is available." };
    }
    const allowed = await ctx.ui.confirm(`Allow ${event.toolName}?`, message);
    if (!allowed) return { block: true, reason: "Denied by user." };
  });
}
