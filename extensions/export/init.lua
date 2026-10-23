-- `/export [path]` writes the session's transcript as Markdown to `path`,
-- default `./<session id>.md` in the workspace, and refuses an existing
-- file as `fiber sessions export` does. The terminal shows the written
-- path as a status line. The extension is lazy: this script runs on the
-- first run of `/export`, and it registers only the one command the
-- manifest declares.
--
-- How the transcript is built, from `history` pages of whole log lines:
-- a probe with no `from_seq` answers the last lines, so its last line's
-- `seq` is the latest and its first line's `session_id` names the session
-- for the default path; pages then tile `0..latest` 256 lines at a time,
-- never asking past the latest, which a `from_seq` past it would reject.
-- Each prompt is a `turn_started` message from the person (`source` of
-- `driver`), quoted line by line; a steering message applied mid-turn is
-- quoted the same way. Each reply part with text is written as is. Each
-- model tool call is one line with its arguments, then the first
-- `TOOL_LINES` lines of its result's text, indented; a call that did not
-- complete names its status. Inner calls (an envelope `ran_by`) and
-- completions for an unknown or already used call write nothing. Parts
-- and items of a kind the code does not know are skipped.
-- The file is built whole and written once, after the check that the
-- target does not exist. The check and the write are two calls, so a
-- file created between them is overwritten; `host.fs` has no create-new
-- mode. The command's timeout covers one host call per page plus the
-- write, and the 16 MiB manifest cap covers a page of large tool results.

local TOOL_LINES = 3
local PAGE = 256

-- The lines of `text`, without their newlines.
local function split_lines(text)
  local lines = {}
  for line in string.gmatch(text .. "\n", "(.-)\n") do
    lines[#lines + 1] = line
  end
  return lines
end

-- The `text` parts of content parts, joined in order.
local function text_of(content)
  local parts = {}
  for _, part in ipairs(content) do
    if part.type == "text" and type(part.text) == "string" then
      parts[#parts + 1] = part.text
    end
  end
  return table.concat(parts, "\n")
end

-- A person's message as a quote: every line prefixed `> `, then blank.
local function quote(out, content)
  local said = text_of(content)
  if said == "" then
    return
  end
  for _, line in ipairs(split_lines(said)) do
    out[#out + 1] = "> " .. line
  end
  out[#out + 1] = ""
end

local function render_requested(out, pending, line)
  local asked = line.payload or {}
  if asked.ran_by ~= nil then
    return
  end
  local id = line.action_id
  if id == nil then
    return
  end
  pending[id] = { name = asked.name, arguments = asked.arguments }
end

local function render_completed(out, pending, line)
  local id = line.action_id
  if id == nil then
    return
  end
  local call = pending[id]
  if call == nil then
    return
  end
  pending[id] = nil
  local arguments = call.arguments
  if type(arguments) ~= "string" then
    arguments = json.encode(arguments)
  end
  local head = "- `" .. tostring(call.name) .. "` " .. tostring(arguments)
  local done = line.payload or {}
  if done.status ~= "completed" then
    head = head .. " (" .. tostring(done.status) .. ")"
  end
  out[#out + 1] = head
  local result = split_lines(text_of(done.content or {}))
  for i = 1, math.min(#result, TOOL_LINES) do
    out[#out + 1] = "    " .. result[i]
  end
  out[#out + 1] = ""
end

local function render_line(out, pending, line)
  local kind = line.kind
  local payload = line.payload or {}
  if kind == "turn_started" then
    for _, item in ipairs(payload.input or {}) do
      if item.type == "message" and item.source == "driver" then
        quote(out, item.content or {})
      end
    end
  elseif kind == "steering_applied" then
    quote(out, payload.content or {})
  elseif kind == "text_completed" then
    if type(payload.text) == "string" and payload.text ~= "" then
      out[#out + 1] = payload.text
      out[#out + 1] = ""
    end
  elseif kind == "tool_call_requested" then
    render_requested(out, pending, line)
  elseif kind == "tool_call_completed" then
    render_completed(out, pending, line)
  end
end

fiber.command("export", {
  description = "Write the transcript as Markdown",
  timeout = 60000,
  run = function(text)
    local probe = host.drive("history", {})
    local first = probe.lines
    if #first == 0 then
      error("the session log is empty", 0)
    end
    local session_id = first[1].session_id
    local latest = first[#first].seq
    local path = ""
    if text ~= nil then
      path = string.match(text, "^%s*(.-)%s*$")
    end
    if path == "" then
      path = session_id .. ".md"
    end
    if host.fs.stat(path) ~= nil then
      error("`" .. path .. "` already exists; nothing was written", 0)
    end
    local out = { "# Session " .. session_id, "" }
    local pending = {}
    local from_seq = 0
    while from_seq <= latest do
      local to_seq = from_seq + PAGE - 1
      if to_seq > latest then
        to_seq = latest
      end
      local page = host.drive("history", { from_seq = from_seq, to_seq = to_seq })
      for _, line in ipairs(page.lines) do
        render_line(out, pending, line)
      end
      from_seq = from_seq + PAGE
    end
    host.fs.write(path, table.concat(out, "\n") .. "\n")
    host.status("wrote " .. path)
  end,
})
