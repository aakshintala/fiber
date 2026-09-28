-- A ledger row for the shell tool: a coloured exit-code badge, the command,
-- and a bar for how long it ran. Run with --lua-renderer lua/shell_row.lua.

local GREEN, RED, ORANGE = "#2e7d4f", "#b3261e", "orange"

-- cuts s to n characters, ending in "…" if it was longer
local function cut(s, n)
  if n <= 0 then return "" end
  if utf8.len(s) <= n then return s end
  return s:sub(1, utf8.offset(s, n) - 1) .. "…"
end

local function secs(ms)
  if ms < 1000 then return string.format("%dms", ms) end
  if ms < 60000 then return string.format("%.1fs", ms / 1000) end
  return string.format("%dm%02ds", ms // 60000, ms % 60000 // 1000)
end

-- 8 cells on a log scale: 10 ms is empty, 100 s is full
local function bar(ms)
  local f = math.max(0, math.min(1, (math.log(math.max(ms, 1), 10) - 1) / 4))
  local n = math.floor(f * 8 + 0.5)
  return string.rep("▰", n), string.rep("▱", 8 - n)
end

local function render(call, width)
  local line = {}
  local function add(span) line[#line + 1] = span end

  -- the badge: the exit code, or what happened instead
  local badge, bg
  if call.status == "running" or call.status == "pending" then
    badge, bg = " … ", ORANGE
  elseif call.exit_code ~= nil then
    badge, bg = string.format(" exit %d ", call.exit_code), call.exit_code == 0 and GREEN or RED
  else
    badge, bg = " " .. call.status .. " ", RED
  end
  add({ text = badge, fg = "#ffffff", bg = bg, bold = true, click = "badge" .. badge })
  add({ text = " " })

  -- the right-hand side: the duration bar and the line count
  local right = {}
  if call.duration_ms then
    local on, off = bar(call.duration_ms)
    right[#right + 1] = { text = on, fg = call.duration_ms > 10000 and "orange" or "cyan" }
    right[#right + 1] = { text = off, dim = true }
    right[#right + 1] = { text = string.format(" %6s", secs(call.duration_ms)), dim = true }
  end
  right[#right + 1] = { text = string.format(" %5d lines", call.lines), dim = true }
  local rw = 0
  for _, s in ipairs(right) do rw = rw + utf8.len(s.text) end

  local cmd = (call.arguments and call.arguments.command) or ""
  local room = width - utf8.len(badge) - 1 - rw - 2
  local c = cut(cmd, room)
  add({ text = c })
  add({ text = string.rep(" ", math.max(0, room - utf8.len(c)) + 2) })
  for _, s in ipairs(right) do add(s) end
  return { line }
end

return { tool = "shell", render = render }
