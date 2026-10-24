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
local function resolve(command)
  if command:find("/", 1, true) == nil then
    return command
  end
  if command:sub(1, 1) == "/" then
    return command
  end
  return workspace .. "/" .. command
end

local function run_entry(name, point, command, args, tools_set)
  return function(given)
    -- The workspace lands in `cwd` and under `/`-programs: a hook that
    -- fires with none cached fails via `on_failure` instead of running in
    -- the wrong directory (`docs/extensions.md`, "Hooks declared in
    -- configuration"). The cache hook below runs first, so this fires
    -- only if that hook never ran.
    if workspace == nil then
      error("entry `" .. name .. "`: no workspace cached yet", 0)
    end
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
    local result = host.exec(resolve(command), args, { cwd = workspace, input = line })
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

local function run_watcher(name, command, args)
  return function(event)
    -- The workspace lands in `cwd` and under `/`-programs: a watcher that
    -- fires with none cached fails instead of running in the wrong
    -- directory (`docs/extensions.md`, "Hooks declared in configuration").
    -- The cache hook below runs first, so this fires only if that hook
    -- never ran.
    if workspace == nil then
      error("entry `" .. name .. "`: no workspace cached yet", 0)
    end
    -- Standard input carries one JSON line: the event, its kind beside
    -- its payload, with no hook-only field (`docs/extensions.md`, "Hooks
    -- declared in configuration", "What the command reads").
    local line = json.encode({ kind = event.kind, payload = event.payload }) .. "\n"
    local result = host.exec(resolve(command), args, { cwd = workspace, input = line })
    if result.signal ~= nil then
      error("entry `" .. name .. "`: command ended on signal " .. tostring(result.signal), 0)
    end
    -- A watcher changes nothing, so any exit but 0 fails: the failure is
    -- a notice, and there is no `on_failure` (`docs/extensions.md`,
    -- "Watchers", "How the command answers").
    if result.exit_code ~= 0 then
      error("entry `" .. name .. "`: command exited with status " .. tostring(result.exit_code), 0)
    end
    -- A watcher's answer is ignored (`docs/extensions.md`, "Hooks declared
    -- in configuration", "How the command answers").
    return nil
  end
end

local function register_watcher(name, entry)
  -- No command to run: refuse through `fiber.hook` with a point that is
  -- not one, so the entry is unregistered with a notice naming the entry
  -- and `command` instead of aborting the entries after it, and the rest
  -- register.
  if type(entry.command) ~= "string" then
    fiber.hook("command", { entry = name })
    return
  end
  -- No timeout: `fiber.watch` cannot name the entry, so refuse the same
  -- way as above, leaving a notice that names the entry and `timeout`,
  -- and the rest register (`docs/extensions.md`, "Hooks declared in
  -- configuration"). Any other invalid field is refused by `fiber.watch`
  -- itself.
  if entry.timeout == nil then
    fiber.hook("watch", { entry = name })
    return
  end
  fiber.watch(entry.watch, {
    timeout = entry.timeout,
    entry = name,
    run = run_watcher(name, entry.command, entry.args or {}),
  })
end

local function register(name, entry)
  if type(entry) ~= "table" then
    error("entry `" .. name .. "` must be a table", 0)
  end
  -- A watcher entry names `watch`, not a hook point: it registers through
  -- `fiber.watch`, never through the hook path, and takes no `on_failure`
  -- (`docs/extensions.md`, "Hooks declared in configuration").
  if entry.watch ~= nil then
    register_watcher(name, entry)
    return
  end
  -- No command to run: refuse the same way, so the entry is unregistered
  -- with a notice naming the entry and `command` instead of aborting the
  -- entries after it, and the rest register.
  if type(entry.command) ~= "string" then
    fiber.hook("command", { entry = name })
    return
  end
  -- `tools` filters the tool points only, and an empty list can never run:
  -- either way the entry is unregistered with a notice naming the entry and
  -- `tools`, refused the same way as `command` above. Omitted, it means
  -- every tool call.
  local tools_set = nil
  if entry.tools ~= nil then
    local at_tool_point = entry.point == "before_tool" or entry.point == "after_tool"
    if not at_tool_point or type(entry.tools) ~= "table" or next(entry.tools) == nil then
      fiber.hook("tools", { entry = name })
      return
    end
    -- A `tools` list names exact tool names in order: a keyed or sparse
    -- table would silently drop entries under `ipairs`, so only a dense
    -- string list registers, and anything else is refused like above.
    local len = #entry.tools
    local count = 0
    for key, tool in pairs(entry.tools) do
      if type(key) ~= "number" or key < 1 or key % 1 ~= 0 or key > len or type(tool) ~= "string" then
        fiber.hook("tools", { entry = name })
        return
      end
      count = count + 1
    end
    if count ~= len then
      fiber.hook("tools", { entry = name })
      return
    end
    tools_set = {}
    for _, tool in ipairs(entry.tools) do
      if type(tool) == "string" then
        tools_set[tool] = true
      end
    end
  end
  -- `phase`, `timeout`, `on_failure` and `check` placement are validated
  -- by `fiber.hook`, whose entry-labeled refusal names the entry and the
  -- field, and the entry is not registered.
  fiber.hook(entry.point, {
    phase = entry.phase,
    timeout = entry.timeout,
    on_failure = entry.on_failure,
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
