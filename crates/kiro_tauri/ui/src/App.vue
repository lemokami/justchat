<script setup lang="ts">
import { reactive, ref, computed, nextTick, watch } from "vue";
import {
  Conversation,
  ConversationContent,
  ConversationEmptyState,
  ConversationScrollButton,
} from "@/components/ai-elements/conversation";
import {
  Message,
  MessageContent,
  MessageResponse,
} from "@/components/ai-elements/message";
import { Reasoning, ReasoningTrigger, ReasoningContent } from "@/components/ai-elements/reasoning";
import { Tool, ToolHeader, ToolContent, ToolOutput } from "@/components/ai-elements/tool";
import { Loader } from "@/components/ai-elements/loader";
import { Button } from "@/components/ui/button";
import {
  Select,
  SelectContent,
  SelectItem,
  SelectTrigger,
  SelectValue,
} from "@/components/ui/select";
import {
  Dialog,
  DialogContent,
  DialogHeader,
  DialogTitle,
  DialogDescription,
  DialogFooter,
} from "@/components/ui/dialog";
import { Input } from "@/components/ui/input";
import {
  PaperclipIcon,
  PlusIcon,
  Trash2Icon,
  MessageSquareIcon,
  ImageIcon,
  FileTextIcon,
  TriangleAlertIcon,
  XIcon,
  PencilIcon,
  Settings2Icon,
} from "@lucide/vue";

const tauri = (window as any).__TAURI__;
const invoke = tauri.core.invoke as (cmd: string, args?: any) => Promise<any>;
const listen = tauri.event.listen as (e: string, cb: (e: any) => void) => Promise<any>;

type Role = "user" | "agent" | "system";
interface PermOption { id: string; name: string; kind: string }
interface ToolCall {
  id: string; title: string; kind: string; status: string;
  output: string | null; permission: { requestId: number; options: PermOption[] } | null;
}
interface Msg {
  role: Role; content: string; thoughts: string;
  toolCalls: ToolCall[]; attachments: string[]; streaming: boolean;
}
/** A chat is local-first: `acp` is the (ephemeral) ACP session id, null when not live. */
interface Chat {
  id: string; title: string; messages: Msg[]; status: "idle" | "thinking";
  acp: string | null; updatedAt: number;
}
interface ModelOpt { id: string; name: string; description?: string }
interface SlashCmd { name: string; description: string; hint?: string }
interface EnvPair { name: string; value: string }
/** Frontend agent profile (args edited as a single string). */
interface Agent { id: string; name: string; command: string; argsText: string; env: EnvPair[]; cwd: string }

const state = reactive({
  conn: "connecting" as "connecting" | "connected" | "disconnected",
  protocolVersion: 0,
  chats: [] as Chat[],
  activeId: null as string | null,
  models: [] as ModelOpt[],
  currentModel: "" as string,
  pending: [] as string[],
  input: "",
  commands: [] as SlashCmd[],
  agents: [] as Agent[],
  activeAgentId: "" as string,
  connectedAgentId: "" as string,
  showAgents: false,
});

const ta = ref<HTMLTextAreaElement | null>(null);
const slashIndex = ref(0);
const slashDismissed = ref(false);
const editingId = ref<string | null>(null);
const editTitle = ref("");

// ACP-session-id -> local-chat-id routing, and pending session/prompt bookkeeping.
const acpToLocal = new Map<string, string>();
let pendingSessionFor: string | null = null;
let pendingPrompt: { localId: string; text: string; paths: string[] } | null = null;

const current = computed(() => state.chats.find((c) => c.id === state.activeId) || null);
const thinking = computed(() => !!current.value && current.value.status === "thinking");
const canSend = computed(() => state.conn === "connected" && !!current.value && (!!state.input.trim() || state.pending.length > 0));

// Slash-command menu: open when the input is "/partial" (no space yet).
const slashPartial = computed(() => {
  const m = state.input.match(/^\/([^\s]*)$/);
  return m ? m[1] : null;
});
const filteredCommands = computed(() => {
  if (slashPartial.value === null) return [];
  const p = slashPartial.value.toLowerCase();
  return state.commands.filter((c) => c.name.toLowerCase().startsWith(p));
});
const slashOpen = computed(
  () => !slashDismissed.value && slashPartial.value !== null && filteredCommands.value.length > 0,
);
watch(slashPartial, () => { slashIndex.value = 0; });

const fileName = (p: string) => p.split("/").pop() || p;
const isImage = (p: string) => /\.(png|jpe?g|gif|webp|bmp|svg)$/i.test(p);
function uuid() {
  try { return crypto.randomUUID(); } catch { return "c-" + Date.now() + "-" + Math.random().toString(16).slice(2); }
}

const chatById = (id: string) => state.chats.find((c) => c.id === id);
const chatByAcp = (acp: string) => { const l = acpToLocal.get(acp); return l ? chatById(l) : undefined; };

function streamingMsg(c: Chat): Msg {
  const last = c.messages[c.messages.length - 1];
  if (last && last.role === "agent" && last.streaming) return last;
  const m: Msg = { role: "agent", content: "", thoughts: "", toolCalls: [], attachments: [], streaming: true };
  c.messages.push(m);
  return m;
}
function findTool(c: Chat, id: string): ToolCall | null {
  for (let i = c.messages.length - 1; i >= 0; i--) {
    const t = c.messages[i].toolCalls.find((x) => x.id === id);
    if (t) return t;
  }
  return null;
}
function deriveTitle(t: string) {
  const l = (t.trim().split("\n")[0] || "").slice(0, 38);
  return t.trim().length > 38 ? l + "…" : l || "New chat";
}
function toolState(tc: ToolCall): any {
  if (tc.permission) return "approval-requested";
  switch (tc.status) {
    case "completed": return "output-available";
    case "failed": return "output-error";
    case "in_progress": return "input-available";
    default: return "input-streaming";
  }
}

// ---- Persistence ----
function persist(c: Chat | null) {
  if (!c || !c.messages.length) return;
  c.updatedAt = Date.now();
  const data = {
    id: c.id, title: c.title, updatedAt: c.updatedAt,
    messages: c.messages.map((m) => ({
      role: m.role, content: m.content, thoughts: m.thoughts,
      attachments: m.attachments,
      // tool calls without transient permission prompts
      toolCalls: m.toolCalls.map((t) => ({ id: t.id, title: t.title, kind: t.kind, status: t.status, output: t.output })),
    })),
  };
  invoke("save_chat", { id: c.id, data });
}

// ---- Session lifecycle ----
function newChat(): Chat {
  const c: Chat = { id: uuid(), title: "New chat", messages: [], status: "idle", acp: null, updatedAt: Date.now() };
  state.chats.unshift(c);
  state.activeId = c.id;
  requestSession(c.id);
  return c;
}
function requestSession(localId: string) {
  pendingSessionFor = localId;
  invoke("create_session");
}
function selectChat(id: string) { state.activeId = id; }

function startRename(c: Chat) {
  editingId.value = c.id;
  editTitle.value = c.title;
  nextTick(() => {
    const el = document.querySelector("[data-rename]") as HTMLInputElement | null;
    el?.focus();
    el?.select();
  });
}
function commitRename() {
  const id = editingId.value;
  if (!id) return;
  const c = chatById(id);
  const title = editTitle.value.trim();
  if (c && title) { c.title = title; persist(c); }
  editingId.value = null;
}
function cancelRename() { editingId.value = null; }
function deleteChat(c: Chat) {
  const idx = state.chats.findIndex((x) => x.id === c.id);
  if (idx >= 0) state.chats.splice(idx, 1);
  if (c.acp) acpToLocal.delete(c.acp);
  invoke("delete_chat", { id: c.id });
  if (state.activeId === c.id) {
    state.activeId = state.chats[0]?.id ?? null;
    if (!state.activeId) newChat();
  }
}

function applyEvent(ev: any) {
  switch (ev.type) {
    case "connected":
      state.conn = "connected"; state.protocolVersion = ev.protocol_version;
      // Start a fresh live chat (history stays in the sidebar).
      newChat();
      break;
    case "disconnected": state.conn = "disconnected"; break;
    case "session_created": {
      const acp = ev.session_id as string;
      const localId = pendingSessionFor;
      pendingSessionFor = null;
      const c = localId ? chatById(localId) : null;
      if (c) {
        c.acp = acp;
        acpToLocal.set(acp, c.id);
        if (pendingPrompt && pendingPrompt.localId === c.id) {
          const p = pendingPrompt; pendingPrompt = null;
          invoke("send_prompt", { sessionId: acp, text: p.text, paths: p.paths });
        }
      }
      break;
    }
    case "models_available": state.models = ev.models; state.currentModel = ev.current; break;
    case "commands_available": state.commands = ev.commands || []; break;
    case "message_chunk": { const c = chatByAcp(ev.session_id); if (c) { streamingMsg(c).content += ev.text; } break; }
    case "thought_chunk": { const c = chatByAcp(ev.session_id); if (c) { streamingMsg(c).thoughts += ev.text; } break; }
    case "tool_call": { const c = chatByAcp(ev.session_id); if (c) streamingMsg(c).toolCalls.push({ id: ev.id, title: ev.title, kind: ev.kind, status: ev.status, output: null, permission: null }); break; }
    case "tool_call_update": { const c = chatByAcp(ev.session_id); if (c) { const t = findTool(c, ev.id); if (t) { if (ev.status) t.status = ev.status; if (ev.output != null) t.output = (t.output || "") + ev.output; } } break; }
    case "plan": { const c = chatByAcp(ev.session_id); if (c) streamingMsg(c).thoughts = "Plan:\n- " + ev.entries.join("\n- "); break; }
    case "permission_requested": {
      const c = chatByAcp(ev.session_id); if (!c) break;
      const m = streamingMsg(c);
      let t = m.toolCalls.find((x) => x.title === ev.title) || m.toolCalls[m.toolCalls.length - 1];
      if (!t) { t = { id: "perm-" + ev.request_id, title: ev.title, kind: "other", status: "pending", output: null, permission: null }; m.toolCalls.push(t); }
      t.permission = { requestId: ev.request_id, options: ev.options };
      break;
    }
    case "turn_ended": {
      const c = chatByAcp(ev.session_id); if (!c) break; c.status = "idle";
      const last = c.messages[c.messages.length - 1];
      if (last && last.role === "agent") {
        last.streaming = false;
        last.toolCalls.forEach((t) => { if (t.status === "pending" || t.status === "in_progress") t.status = "completed"; });
      }
      persist(c);
      break;
    }
    case "error": {
      const c = chatByAcp(ev.session_id) || current.value;
      if (c) { c.status = "idle"; c.messages.push({ role: "system", content: "Error: " + ev.message, thoughts: "", toolCalls: [], attachments: [], streaming: false }); persist(c); }
      break;
    }
  }
}

function send() {
  const text = state.input.trim();
  const c = current.value;
  if (!c) return;
  if (c.status === "thinking") { if (c.acp) invoke("cancel", { sessionId: c.acp }); return; }
  if (!text && !state.pending.length) return;
  const attachments = [...state.pending];
  if (c.title === "New chat") c.title = deriveTitle(text || (attachments[0] ? fileName(attachments[0]) : "Attachments"));
  c.messages.push({ role: "user", content: text, thoughts: "", toolCalls: [], attachments, streaming: false });
  c.messages.push({ role: "agent", content: "", thoughts: "", toolCalls: [], attachments: [], streaming: true });
  c.status = "thinking";
  state.pending = [];
  state.input = "";
  if (ta.value) ta.value.style.height = "auto";
  persist(c);
  if (c.acp) {
    invoke("send_prompt", { sessionId: c.acp, text, paths: attachments });
  } else {
    pendingPrompt = { localId: c.id, text, paths: attachments };
    requestSession(c.id);
  }
}

function autogrow() {
  const el = ta.value;
  if (el) { el.style.height = "auto"; el.style.height = Math.min(el.scrollHeight, 160) + "px"; }
  slashDismissed.value = false;
}
function applyCommand(cmd: SlashCmd) {
  state.input = "/" + cmd.name + " ";
  slashDismissed.value = true;
  nextTick(() => { ta.value?.focus(); });
}
function onKey(e: KeyboardEvent) {
  if (slashOpen.value) {
    const n = filteredCommands.value.length;
    if (e.key === "ArrowDown") { e.preventDefault(); slashIndex.value = (slashIndex.value + 1) % n; return; }
    if (e.key === "ArrowUp") { e.preventDefault(); slashIndex.value = (slashIndex.value - 1 + n) % n; return; }
    if (e.key === "Enter" || e.key === "Tab") { e.preventDefault(); applyCommand(filteredCommands.value[slashIndex.value] || filteredCommands.value[0]); return; }
    if (e.key === "Escape") { e.preventDefault(); slashDismissed.value = true; return; }
  }
  if (e.key === "Enter" && !e.shiftKey) { e.preventDefault(); send(); }
}

function decide(tc: ToolCall, o: PermOption) {
  if (!tc.permission) return;
  invoke("permission_decision", { requestId: tc.permission.requestId, optionId: o.id });
  tc.permission = null;
}

// ---- Agents ----
const activeAgent = computed(() => state.agents.find((a) => a.id === state.activeAgentId) || state.agents[0] || null);
const connectedAgent = computed(() => state.agents.find((a) => a.id === state.connectedAgentId) || null);

function toBackendAgent(a: Agent) {
  return {
    id: a.id, name: a.name, command: a.command,
    args: a.argsText.trim() ? a.argsText.trim().split(/\s+/) : [],
    env: a.env.filter((e) => e.name.trim()),
    cwd: a.cwd.trim() || null,
  };
}
function resetConnection() {
  state.conn = "connecting"; state.activeId = null; acpToLocal.clear();
  state.models = []; state.commands = []; state.currentModel = "";
}
function persistAgents() {
  invoke("save_agents", { store: { agents: state.agents.map(toBackendAgent), activeId: state.activeAgentId } });
}
function connectAgent(a: Agent) {
  if (!a || !a.command.trim()) { openAgents(); return; }
  state.activeAgentId = a.id;
  state.connectedAgentId = a.id;
  persistAgents();
  resetConnection();
  state.showAgents = false;
  invoke("connect", { agent: toBackendAgent(a) });
}
function connectActive() { const a = activeAgent.value; if (a) connectAgent(a); }
function reconnect() { connectActive(); }
function disconnect() {
  state.connectedAgentId = "";
  state.conn = "disconnected";
  invoke("disconnect");
}
function onModelChange(v: any) { const c = current.value; if (c && c.acp && v) invoke("set_model", { sessionId: c.acp, modelId: v }); }
async function attach() {
  const paths: string[] = await invoke("pick_files");
  paths.forEach((p) => { if (!state.pending.includes(p)) state.pending.push(p); });
}

function openAgents() { state.showAgents = true; }
function addAgent() {
  state.agents.push({ id: uuid(), name: "New agent", command: "", argsText: "", env: [], cwd: "" });
}
function removeAgent(a: Agent) {
  const i = state.agents.findIndex((x) => x.id === a.id);
  if (i >= 0) state.agents.splice(i, 1);
  if (state.activeAgentId === a.id) state.activeAgentId = state.agents[0]?.id ?? "";
}
function addEnv(a: Agent) { a.env.push({ name: "", value: "" }); }
function removeEnv(a: Agent, idx: number) { a.env.splice(idx, 1); }
function closeAgents() {
  state.agents = state.agents.filter((a) => a.name.trim() || a.command.trim());
  persistAgents();
  state.showAgents = false;
}

// ---- Boot: load history, then connect ----
async function boot() {
  try {
    const saved: any[] = await invoke("load_chats");
    state.chats = saved.map((c) => ({
      id: c.id, title: c.title || "Chat", updatedAt: c.updatedAt || 0, acp: null, status: "idle" as const,
      messages: (c.messages || []).map((m: any) => ({
        role: m.role, content: m.content || "", thoughts: m.thoughts || "",
        attachments: m.attachments || [], streaming: false,
        toolCalls: (m.toolCalls || []).map((t: any) => ({ id: t.id, title: t.title, kind: t.kind, status: t.status, output: t.output ?? null, permission: null })),
      })),
    }));
  } catch { /* no history */ }

  // Load configured agents.
  try {
    const store: any = await invoke("load_agents");
    state.agents = (store.agents || []).map((a: any) => ({
      id: a.id, name: a.name, command: a.command,
      argsText: (a.args || []).join(" "),
      env: (a.env || []).map((e: any) => ({ name: e.name, value: e.value })),
      cwd: a.cwd || "",
    }));
    state.activeAgentId = store.activeId || state.agents[0]?.id || "";
  } catch { /* defaults missing */ }

  await listen("acp-event", (e: any) => applyEvent(e.payload));
  await nextTick();
  connectActive();
}
boot();
</script>

<template>
  <div class="flex h-screen bg-background text-foreground">
    <!-- Sidebar -->
    <aside class="w-60 shrink-0 border-r bg-sidebar flex flex-col">
      <div class="p-3">
        <Button class="w-full justify-center gap-2" @click="newChat">
          <PlusIcon class="size-4" /> New chat
        </Button>
      </div>
      <div class="px-3 pb-1 text-xs uppercase tracking-wider text-muted-foreground">Chats</div>
      <div class="flex-1 overflow-y-auto px-2 pb-2 space-y-1">
        <div v-for="c in state.chats" :key="c.id"
          class="group/item flex items-center rounded-md text-sm transition-colors"
          :class="c.id === state.activeId ? 'bg-sidebar-accent text-sidebar-accent-foreground' : 'hover:bg-muted text-muted-foreground'">
          <input v-if="editingId === c.id" data-rename v-model="editTitle"
            class="flex-1 min-w-0 mx-2 my-1 px-2 py-1 rounded bg-background border border-input text-sm text-foreground outline-none focus:border-primary"
            @keydown.enter.prevent="commitRename" @keydown.esc.prevent="cancelRename" @blur="commitRename" />
          <template v-else>
            <button class="flex-1 min-w-0 text-left px-3 py-2 truncate flex items-center gap-2"
              @click="selectChat(c.id)" @dblclick="startRename(c)">
              <MessageSquareIcon class="size-3.5 shrink-0 opacity-70" />
              <span class="truncate">{{ c.title }}</span>
            </button>
            <button class="opacity-0 group-hover/item:opacity-100 px-1.5 text-muted-foreground hover:text-foreground transition-opacity"
              title="Rename chat" @click.stop="startRename(c)">
              <PencilIcon class="size-3.5" />
            </button>
            <button class="opacity-0 group-hover/item:opacity-100 px-1.5 pr-2 text-muted-foreground hover:text-destructive transition-opacity"
              title="Delete chat" @click.stop="deleteChat(c)">
              <Trash2Icon class="size-3.5" />
            </button>
          </template>
        </div>
        <p v-if="!state.chats.length" class="px-3 py-2 text-xs text-muted-foreground">No chats yet</p>
      </div>
    </aside>

    <!-- Main -->
    <main class="flex-1 flex flex-col min-w-0">
      <header class="h-12 shrink-0 flex items-center justify-between px-4 border-b">
        <div class="flex items-center gap-2.5">
          <div class="h-7 w-7 rounded-lg bg-primary text-primary-foreground flex items-center justify-center font-bold text-sm">K</div>
          <span class="font-semibold">KiroUI</span>
          <span class="ml-2 text-xs px-2 py-0.5 rounded-full"
            :class="state.conn === 'connected' ? 'bg-green-500/10 text-green-500' : state.conn === 'connecting' ? 'bg-amber-500/10 text-amber-500' : 'bg-destructive/10 text-destructive'">
            {{ state.conn === 'connected' ? 'Connected · ACP v' + state.protocolVersion : state.conn === 'connecting' ? 'Connecting…' : 'Disconnected' }}
          </span>
        </div>
        <div class="flex items-center gap-2">
          <template v-if="state.models.length">
            <span class="text-xs text-muted-foreground">Model</span>
            <Select v-model="state.currentModel" @update:modelValue="onModelChange">
              <SelectTrigger class="h-8 text-xs w-[200px]"><SelectValue placeholder="Select model" /></SelectTrigger>
              <SelectContent>
                <SelectItem v-for="m in state.models" :key="m.id" :value="m.id">{{ m.name }}</SelectItem>
              </SelectContent>
            </Select>
          </template>
          <span v-if="state.conn === 'connected' && connectedAgent" class="text-xs text-muted-foreground">
            {{ connectedAgent.name }}
          </span>
          <Button v-if="state.conn === 'connected'" size="sm" variant="outline" @click="disconnect">Disconnect</Button>
          <Button v-else size="sm" @click="connectActive">Connect</Button>
          <Button variant="ghost" size="icon-sm" title="Manage agents" @click="openAgents">
            <Settings2Icon class="size-4" />
          </Button>
        </div>
      </header>

      <Conversation class="flex-1">
        <ConversationContent class="max-w-3xl mx-auto w-full gap-4 py-6">
          <ConversationEmptyState v-if="!current || !current.messages.length"
            title="Ask Kiro anything" description="Chat, run tools, and read files right from your desktop." />

          <Message v-for="(m, i) in (current ? current.messages : [])" :key="i" :from="m.role === 'agent' ? 'assistant' : m.role">
            <div v-if="m.role !== 'user'"
              class="size-7 shrink-0 rounded-full flex items-center justify-center text-[11px] font-bold mt-0.5"
              :class="m.role === 'agent' ? 'bg-primary text-primary-foreground' : 'bg-destructive/20 text-destructive'">
              {{ m.role === 'agent' ? 'K' : '!' }}
            </div>
            <MessageContent>
              <Reasoning v-if="m.thoughts" :is-streaming="m.streaming" :default-open="false" class="mb-1">
                <ReasoningTrigger />
                <ReasoningContent :content="m.thoughts" />
              </Reasoning>

              <p v-if="m.role !== 'agent' && m.content" class="whitespace-pre-wrap break-words">{{ m.content }}</p>

              <div v-if="m.attachments.length" class="flex flex-wrap gap-2 mt-1">
                <span v-for="p in m.attachments" :key="p" class="inline-flex items-center gap-1.5 text-xs bg-muted rounded-md px-2 py-1 border">
                  <ImageIcon v-if="isImage(p)" class="size-3.5" /><FileTextIcon v-else class="size-3.5" />
                  {{ fileName(p) }}
                </span>
              </div>

              <Tool v-for="tc in m.toolCalls" :key="tc.id" default-open class="mt-2">
                <ToolHeader type="dynamic-tool" :tool-name="tc.kind" :title="tc.title" :state="toolState(tc)" />
                <ToolContent>
                  <ToolOutput v-if="tc.output" :output="tc.output" :error-text="tc.status === 'failed' ? tc.output : undefined" />
                  <div v-if="tc.permission" class="p-3 border-t">
                    <div class="flex items-center gap-1.5 text-xs text-amber-500 mb-2">
                      <TriangleAlertIcon class="size-3.5" /> Approval required
                    </div>
                    <div class="flex gap-2">
                      <Button v-for="o in tc.permission.options" :key="o.id" size="xs"
                        :variant="o.kind.includes('reject') ? 'destructive' : 'default'" @click="decide(tc, o)">
                        {{ o.name }}
                      </Button>
                    </div>
                  </div>
                </ToolContent>
              </Tool>

              <MessageResponse v-if="m.role === 'agent' && m.content" :content="m.content" />

              <div v-if="m.streaming && !m.content && !m.toolCalls.length" class="inline-flex items-center gap-2 text-muted-foreground text-sm leading-none">
                <Loader :size="14" />
                <span>Thinking…</span>
              </div>
            </MessageContent>
          </Message>
        </ConversationContent>
        <ConversationScrollButton />
      </Conversation>

      <div class="px-4 pb-4 pt-2">
        <div class="relative max-w-3xl mx-auto">
          <!-- Slash command menu -->
          <div v-if="slashOpen"
            class="absolute bottom-full mb-2 left-0 right-0 max-h-72 overflow-y-auto rounded-xl border bg-popover shadow-lg p-1 z-20">
            <div v-for="(cmd, idx) in filteredCommands" :key="cmd.name"
              class="flex flex-col gap-0.5 px-3 py-2 rounded-lg cursor-pointer"
              :class="idx === slashIndex ? 'bg-accent text-accent-foreground' : 'hover:bg-muted'"
              @mouseenter="slashIndex = idx" @mousedown.prevent="applyCommand(cmd)">
              <div class="flex items-center gap-2 text-sm">
                <span class="font-medium">/{{ cmd.name }}</span>
                <span v-if="cmd.hint" class="text-xs text-muted-foreground">{{ cmd.hint }}</span>
              </div>
              <span class="text-xs text-muted-foreground truncate">{{ cmd.description }}</span>
            </div>
          </div>

          <!-- Attachment chips -->
          <div v-if="state.pending.length" class="flex flex-wrap gap-2 mb-2">
            <span v-for="(p, idx) in state.pending" :key="p" class="inline-flex items-center gap-2 text-xs bg-muted border rounded-md px-2 py-1">
              <span class="inline-flex items-center gap-1.5">
                <ImageIcon v-if="isImage(p)" class="size-3.5" /><FileTextIcon v-else class="size-3.5" />
                {{ fileName(p) }}
              </span>
              <button class="text-muted-foreground hover:text-destructive" @click="state.pending.splice(idx, 1)">
                <XIcon class="size-3.5" />
              </button>
            </span>
          </div>

          <!-- Input -->
          <div class="flex items-end gap-2 rounded-2xl border bg-card px-2 py-2 focus-within:border-primary/60 transition-colors">
            <Button variant="ghost" size="icon-sm" class="shrink-0" title="Attach files" @click="attach">
              <PaperclipIcon class="size-4" />
            </Button>
            <textarea ref="ta" v-model="state.input" rows="1" @keydown="onKey" @input="autogrow"
              :disabled="state.conn !== 'connected'"
              :placeholder="state.conn === 'connected' ? 'Message agent…  ( / for commands · Enter to send · Shift+Enter for newline )' : 'Connect an agent to start chatting…'"
              class="flex-1 bg-transparent resize-none outline-none py-2 text-sm max-h-40 placeholder:text-muted-foreground disabled:opacity-60"></textarea>
            <Button v-if="thinking" variant="secondary" size="sm" class="shrink-0" @click="send">Stop</Button>
            <Button v-else size="sm" class="shrink-0" :disabled="!canSend" @click="send">Send</Button>
          </div>
        </div>
      </div>
    </main>

    <!-- Agent management -->
    <Dialog v-model:open="state.showAgents">
      <DialogContent class="sm:max-w-2xl max-h-[82vh] overflow-y-auto">
        <DialogHeader>
          <DialogTitle>Agents</DialogTitle>
          <DialogDescription>
            Connect any ACP-compatible agent. Set its command, arguments, and any API-key environment variables.
          </DialogDescription>
        </DialogHeader>

        <div class="space-y-4 py-2">
          <div v-for="a in state.agents" :key="a.id" class="rounded-lg border p-3 space-y-2"
            :class="state.connectedAgentId === a.id && state.conn === 'connected' ? 'border-primary/60' : ''">
            <div class="flex items-center gap-2">
              <Input v-model="a.name" placeholder="Agent name" class="h-8 font-medium" />
              <span v-if="state.connectedAgentId === a.id && state.conn === 'connected'"
                class="text-[11px] px-2 py-0.5 rounded-full bg-green-500/10 text-green-500 shrink-0">Connected</span>
              <Button v-if="state.connectedAgentId === a.id && state.conn === 'connected'"
                size="sm" variant="outline" class="shrink-0" @click="disconnect">Disconnect</Button>
              <Button v-else size="sm" class="shrink-0" :disabled="!a.command.trim()" @click="connectAgent(a)">Connect</Button>
              <Button variant="ghost" size="icon-sm" title="Remove" class="shrink-0" @click="removeAgent(a)">
                <Trash2Icon class="size-4 text-destructive" />
              </Button>
            </div>
            <div class="grid grid-cols-3 gap-2">
              <Input v-model="a.command" placeholder="command" class="h-8 text-xs font-mono" />
              <Input v-model="a.argsText" placeholder="arguments (space-separated)" class="h-8 text-xs font-mono col-span-2" />
            </div>
            <Input v-model="a.cwd" placeholder="working directory (optional)" class="h-8 text-xs font-mono" />
            <div class="space-y-1.5">
              <div class="text-xs text-muted-foreground">Environment variables (API keys)</div>
              <div v-for="(e, idx) in a.env" :key="idx" class="flex gap-2">
                <Input v-model="e.name" placeholder="NAME" class="h-8 text-xs font-mono w-1/3" />
                <Input v-model="e.value" type="password" placeholder="value" class="h-8 text-xs font-mono flex-1" />
                <Button variant="ghost" size="icon-sm" @click="removeEnv(a, idx)"><XIcon class="size-4" /></Button>
              </div>
              <Button variant="outline" size="xs" @click="addEnv(a)">
                <PlusIcon class="size-3" /> Add variable
              </Button>
            </div>
          </div>
          <Button variant="outline" class="w-full" @click="addAgent">
            <PlusIcon class="size-4" /> Add agent
          </Button>
        </div>

        <DialogFooter>
          <Button variant="outline" @click="closeAgents">Done</Button>
        </DialogFooter>
      </DialogContent>
    </Dialog>
  </div>
</template>
