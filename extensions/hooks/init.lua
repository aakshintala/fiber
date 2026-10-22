-- The first-party `hooks` extension
-- (`docs/extensions.md`, "Hooks declared in configuration"). It reads hook
-- declarations from its settings and registers one real hook per entry, so
-- a declared hook follows every rule a Lua hook follows. Entries come from
-- `host.config.get("hooks")`, approved-only, one entry per name.

local SCHEMA_VERSION = 1

-- The points whose answer may carry `refuse`, and the points whose
-- exit-0 text is added as `context`
-- (`docs/extensions.md`, "What a hook is given and returns").
local refusing = { before_message = true, before_tool = true, before_model_call = true }
local context_points = { session_start = true, before_message = true, turn_start = true }

-- The session's workspace root. The internal `session_start` hook below
-- caches it before any entry runs, and a fresh VM re-seeds it from
-- extension state, which Fiber folds before the first call.
local workspace = state.get("hooks_workspace")

local function strip(text)
  return (text:gsub("\n$", ""))
end

-- Full trim for the exit-2 reason: a reason of only whitespace is no
-- reason, so the entry fails instead of refusing (`docs/extensions.md`,
-- "Hooks declared in configuration").
local function trim(text)
  return (text:match("^%s*(.-)%s*$"))
end

-- A bare name runs on `PATH`, a relative path with a `/` joins onto the
-- cached workspace, an absolute path runs as written
-- (`docs/extensions.md`, "Hooks declared in configuration").
local function resolve(name, command)
  if command:find("/", 1, true) == nil then
    return command
  end
  if command:sub(1, 1) == "/" then
    return command
  end
  if workspace == nil then
    error("entry `" .. name .. "`: no workspace cached yet", 0)
  end
  return workspace .. "/" .. command
end

local function run_entry(name, point, command, args, tools_set)
  return function(given)
    if tools_set ~= nil then
      local tool = given.tool
      if tool == nil or not tools_set[tool] then
        return nil
      end
    end
    local payload = {}
    for key, value in pairs(given) do
      payload[key] = value
    end
    payload.point = point
    payload.schema_version = SCHEMA_VERSION
    local line = json.encode(payload) .. "\n"
    local result = host.exec(resolve(name, command), args, { cwd = workspace, input = line })
    if result.signal ~= nil then
      error("entry `" .. name .. "`: command ended on signal " .. tostring(result.signal), 0)
    end
    local code = result.exit_code
    if code == 2 then
      if not refusing[point] then
        error("entry `" .. name .. "`: command exited 2 at `" .. point .. "`, which cannot refuse", 0)
      end
      local reason = trim(result.stderr)
      if reason == "" then
        error("entry `" .. name .. "`: command exited 2 with no reason at `" .. point .. "`", 0)
      end
      return { refuse = reason }
    end
    if code ~= 0 then
      error("entry `" .. name .. "`: command exited with status " .. tostring(code), 0)
    end
    local out = result.stdout
    if out == "" then
      return nil
    end
    local first = out:match("^([^\n]*)")
    local ok, decoded = pcall(json.decode, first)
    if ok and type(decoded) == "table" then
      return decoded
    end
    local text = strip(out)
    if text == "" then
      return nil
    end
    if context_points[point] then
      return { context = text }
    end
    error("entry `" .. name .. "`: command printed text at `" .. point .. "`, which takes no context", 0)
  end
end

local function register(name, entry)
  if type(entry) ~= "table" then
    error("entry `" .. name .. "` must be a table", 0)
  end
  -- Watcher entries are not hooks: the gate reads them, this package
  -- only registers hooks.
  if entry.watch ~= nil then
    return
  end
  if type(entry.command) ~= "string" then
    error("entry `" .. name .. "`: `command` must be a string", 0)
  end
  -- `phase`, `timeout`, `on_failure`, `check` placement and `tools` are
  -- validated by `fiber.hook`, whose entry-labeled refusal names the entry
  -- and the field, and the entry is not registered. The set below only
  -- filters a registered entry's runs; an unregistered entry never runs,
  -- so a `tools` the refusal rejected leaves no set behind.
  local tools_set = nil
  if type(entry.tools) == "table" then
    tools_set = {}
    for _, tool in ipairs(entry.tools) do
      if type(tool) == "string" then
        tools_set[tool] = true
      end
    end
  end
  fiber.hook(entry.point, {
    phase = entry.phase,
    timeout = entry.timeout,
    on_failure = entry.on_failure,
    tools = entry.tools,
    entry = name,
    run = run_entry(name, entry.point, entry.command, entry.args or {}, tools_set),
  })
end

-- The internal cache hook runs first at every start, so every entry sees
-- the workspace, on this VM and on a fresh one.
fiber.hook("session_start", {
  phase = "sanitize",
  timeout = 5000,
  on_failure = "non-blocking",
  run = function(given)
    workspace = given.workspace
    state.set("hooks_workspace", workspace)
    return nil
  end,
})

local declared = host.config.get("hooks")
if declared ~= nil then
  if type(declared) ~= "table" then
    error("`hooks` must be an object of hook entries, one per name", 0)
  end
  local names = {}
  for name in pairs(declared) do
    names[#names + 1] = name
  end
  table.sort(names)
  for _, name in ipairs(names) do
    register(name, declared[name])
  end
end
