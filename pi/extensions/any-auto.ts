import type { ExtensionAPI, ExtensionContext } from "@earendil-works/pi-coding-agent";
import { spawn } from "node:child_process";
import { randomUUID } from "node:crypto";

// The installer replaces this literal with the absolute executable path.
const executable = "any-auto";

const USER_MESSAGE_LIMIT = 5;

// The active branch supplies user-origin evidence, never assistant summaries.
function authorizationContext(ctx: ExtensionContext) {
  const unavailable = { availability: "unavailable", latest_user_message: null, relevant_prior_messages: [] };
  try {
    const messages: { id: string; role: "user"; text: string; source: string }[] = [];
    let truncated = false;
    let bytes = 0;
    for (const entry of [...ctx.sessionManager.getBranch()].reverse()) {
      // Older entries are outside the intended window, not missing evidence.
      if (messages.length === USER_MESSAGE_LIMIT) break;
      if (entry.type === "compaction" || entry.type === "branch_summary") truncated = true;
      if (entry.type !== "message" || entry.message.role !== "user") continue;
      const content = entry.message.content;
      const text = typeof content === "string" ? content : content.filter((part) => part.type === "text").map((part) => part.text).join("\n");
      if (Array.isArray(content) && content.some((part) => part.type !== "text")) truncated = true;
      bytes += Buffer.byteLength(text, "utf8");
      if (bytes > 12 * 1024) { truncated = true; break; }
      if (!text.trim()) { truncated = true; continue; }
      messages.push({ id: entry.id, role: "user", text, source: "current_branch" });
    }
    if (messages.length === 0) return unavailable;
    return { availability: truncated ? "truncated" : "available", latest_user_message: messages[0], relevant_prior_messages: messages.slice(1).reverse() };
  } catch { return unavailable; }
}

export default function (pi: ExtensionAPI) {
  let branch = "";
  pi.on("session_start", async (_event, ctx) => {
    const marker = [...ctx.sessionManager.getBranch()].reverse().find((entry) => entry.type === "custom" && entry.customType === "agy-approval-branch");
    branch = marker?.type === "custom" && typeof marker.data === "string" ? marker.data : "";
  });
  pi.on("session_tree", async () => {
    branch = randomUUID();
    pi.appendEntry("agy-approval-branch", branch);
  });
  pi.on("tool_call", async (event, ctx) => {
    if (process.env.ANY_AUTO_REVIEWER) {
      return { block: true, reason: "Approval reviewers may not invoke tools." };
    }
    const requestId = randomUUID();
    const session = `${ctx.sessionManager.getSessionId()}${branch ? `:${branch}` : ""}`;
    const payload = {
      request_id: requestId,
      builtin_tool: pi.getAllTools().find((tool) => tool.name === event.toolName)?.sourceInfo.source === "builtin",
      conversationId: session,
      toolCall: { name: event.toolName, args: event.input },
      workspacePaths: [ctx.cwd],
      authorization: authorizationContext(ctx),
    };
    try {
      const response = await new Promise<any>((resolve, reject) => {
        const child = spawn(executable, ["hook", "--agent", "pi"], { stdio: ["pipe", "pipe", "pipe"] });
        let stdout = "";
        let bytes = 0;
        const stop = () => { child.kill("SIGKILL"); reject(new Error("Approval cancelled")); };
        const timer = setTimeout(() => { child.kill("SIGKILL"); reject(new Error("Approval timed out")); }, 30_000);
        ctx.signal?.addEventListener("abort", stop, { once: true });
        if (ctx.signal?.aborted) stop();
        child.stdout.setEncoding("utf8");
        child.stdout.on("data", (chunk: string) => {
          bytes += Buffer.byteLength(chunk);
          if (bytes > 1024 * 1024) { child.kill("SIGKILL"); reject(new Error("Approval response too large")); return; }
          stdout += chunk;
        });
        child.stderr.resume();
        child.stdin.on("error", reject);
        child.on("error", reject);
        child.on("close", (code) => {
          clearTimeout(timer);
          ctx.signal?.removeEventListener("abort", stop);
          if (code !== 0) { reject(new Error(`Approver exited with ${code}`)); return; }
          try { resolve(JSON.parse(stdout)); } catch { reject(new Error("Invalid approver response")); }
        });
        child.stdin.end(JSON.stringify(payload));
      });
      if (ctx.signal?.aborted) return { block: true, reason: "Approval cancelled.", terminate: true };
      if (response.decision === "allow") return;
      if (response.decision === "ask" || response.decision === "force_ask") {
        let allowed = false;
        if (ctx.hasUI && !ctx.signal?.aborted) {
          allowed = await ctx.ui.confirm("Tool approval required", `${event.toolName}\n${JSON.stringify(event.input)}\n\n${response.reason ?? "Review required"}`, { timeout: 60_000 });
        }
        // Report the human outcome separately; it never changes model usage statistics.
        await new Promise<void>((resolve) => {
          const child = spawn(executable, ["human-result", "--agent", "pi"], { stdio: ["pipe", "ignore", "ignore"] });
          const timer = setTimeout(() => { child.kill("SIGKILL"); resolve(); }, 2000);
          child.on("error", () => { clearTimeout(timer); resolve(); });
          child.on("close", () => { clearTimeout(timer); resolve(); });
          child.stdin.on("error", () => {});
          child.stdin.end(JSON.stringify({ request_id: response.request_id ?? requestId, conversation_id: session, allowed: allowed && !ctx.signal?.aborted }));
        });
        if (allowed && !ctx.signal?.aborted) return;
        return { block: true, reason: "Human confirmation unavailable, declined, or cancelled.", terminate: true };
      }
      return { block: true, reason: typeof response.reason === "string" ? response.reason : "Approval denied or invalid response." };
    } catch (error) {
      return { block: true, reason: `Approval failed: ${error instanceof Error ? error.message : String(error)}`, terminate: true };
    }
  });
}
