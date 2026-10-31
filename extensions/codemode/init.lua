-- The first-party `codemode` extension (`docs/extensions.md`, "Running a
-- tool"): one tool that runs a Lua script. The script reaches session tools
-- only through `host.tool`, whose outcomes it sees as the model would see
-- them (`status`, `content`, `artifact`, and `reason` or `error` when the
-- call did not complete). Printed output comes only from `print()`; a chunk
-- return value is ignored. The result is the printed lines, then one status
-- line, then one ledger row per inner call that finished.
--
-- The tool declares `executes`, so admission reviews the script
-- (`docs/extensions.md`, "Running a tool"). An inner call runs inside the
-- outer call: it counts against the outer call's timeout, and cancelling
-- the outer call cancels its inner calls.

fiber.tool("codemode", {
  description = "Runs a Lua script. The script calls session tools through host.tool; the result is the printed lines, then one status line and a ledger.",
  input_schema = {
    type = "object",
    properties = { source = { type = "string", description = "The Lua to run." } },
    required = { "source" },
  },
  effects = { effects = { "executes" }, reversible = false },
  timeout = 60000,
  keep = { head = 8192, tail = 8192 },
  run = function(args)
    local source = args.source
    if type(source) ~= "string" then
      error("codemode: `source` must be a string", 0)
    end
    -- Printed lines, in order. `print` formats as `tostring` of each
    -- argument, separated by tabs, with a newline per call; a chunk return
    -- value is ignored and appended nowhere.
    local printed = {}
    local function capture(...)
      local parts = {}
      for i = 1, select("#", ...) do
        parts[i] = tostring(select(i, ...))
      end
      printed[#printed + 1] = table.concat(parts, "\t")
    end
    -- Finished inner calls, in call order: the name, the status the script
    -- saw, and the artifact path when the outcome carries one. A call that
    -- raises instead of returning finishes nothing and leaves no row.
    local ledger = {}
    local real_tool = host.tool
    local function observing_tool(name, call_args)
      local outcome = real_tool(name, call_args)
      local row = { name = name, status = outcome.status }
      if outcome.artifact ~= nil then
        row.artifact = outcome.artifact
      end
      ledger[#ledger + 1] = row
      return outcome
    end
    local script_host = { tool = observing_tool }
    local env = {}
    for name, value in pairs(_G) do
      if name ~= "_G" and name ~= "host" and name ~= "fiber" and name ~= "load" then
        env[name] = value
      end
    end
    env.print = capture
    env.host = script_host
    env._G = env
    env.load = function(source, chunkname, mode, chunk_env)
      return load(source, chunkname, mode, chunk_env or env)
    end
    local chunk, load_error = load(source, "codemode", "t", env)
    local ok, failure = true, nil
    if chunk == nil then
      ok, failure = false, load_error
    else
      ok, failure = pcall(chunk)
    end
    local out = {}
    for _, line in ipairs(printed) do
      out[#out + 1] = line
    end
    if ok then
      out[#out + 1] = "Script finished"
    else
      out[#out + 1] = "Script failed: " .. tostring(failure)
    end
    for _, row in ipairs(ledger) do
      if row.artifact ~= nil then
        out[#out + 1] = "- " .. row.name .. " " .. row.status .. " " .. row.artifact
      else
        out[#out + 1] = "- " .. row.name .. " " .. row.status
      end
    end
    return table.concat(out, "\n")
  end,
})
