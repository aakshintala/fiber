-- The first-party `codemode` extension (`docs/extensions.md`, "Running a
-- tool"): one tool that runs a Lua script. The script reaches session tools
-- only through `host.tool`, whose outcomes it sees as the model would see
-- them (`status`, `content`, `artifact`, and `reason` or `error` when the
-- call did not complete). Printed output comes only from `print()`; a chunk
-- return value is ignored. The result is the printed lines, then one status
-- line, then one ledger row per inner call that finished. A printed outcome
-- shows its text parts in order, then its error message or reason when it
-- carries one, then its status when that status is not `completed`.
--
-- The tool declares `executes`, so admission reviews the script
-- (`docs/extensions.md`, "Running a tool"). An inner call runs inside the
-- outer call: it counts against the outer call's timeout, and cancelling
-- the outer call cancels its inner calls.

fiber.tool("codemode", {
  description = "Runs a Lua script. The script calls session tools through host.tool(name, args); "
    .. "each call returns its outcome table: status, content, artifact, and reason or error when the call "
    .. "did not complete. Printing an outcome prints its text content. "
    .. "A JSON null decodes to the null value: compare with json.decode('null') for equality, "
    .. "since indexing null raises an error. Example:\n"
    .. "local o = host.tool(\"read\", { path = \"x\" })\n"
    .. "print(o)\n"
    .. "print(o.content[1].text)",
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
    -- Invalid UTF-8 never reaches the outer result: the result's
    -- conversion rejects it and fails the run, so `capture` replaces
    -- each invalid sequence with one U+FFFD through `host.utf8_lossy`.
    local lossy = host.utf8_lossy
    local function capture(...)
      local parts = {}
      for i = 1, select("#", ...) do
        parts[i] = lossy(tostring(select(i, ...)))
      end
      printed[#printed + 1] = table.concat(parts, "\t")
    end
    -- Finished inner calls, in call order: the name, the status the script
    -- saw, and the artifact path when the outcome carries one. A call that
    -- raises instead of returning finishes nothing and leaves no row.
    local ledger = {}
    local real_tool = host.tool
    -- How a printed outcome reads: its text parts in order, then its error
    -- message or reason when it carries one, then its status when that
    -- status is not `completed`. Non-text parts print nothing.
    local function outcome_text(outcome)
      local lines = {}
      for _, part in ipairs(outcome.content) do
        if part.type == "text" then
          lines[#lines + 1] = part.text
        end
      end
      if outcome.error ~= nil then
        lines[#lines + 1] = outcome.error.message
      elseif outcome.reason ~= nil then
        lines[#lines + 1] = outcome.reason
      end
      if outcome.status ~= "completed" then
        lines[#lines + 1] = outcome.status
      end
      return table.concat(lines, "\n")
    end
    local outcome_metatable = { __tostring = outcome_text }
    local function observing_tool(name, call_args)
      local outcome = real_tool(name, call_args)
      setmetatable(outcome, outcome_metatable)
      local row = { name = name, status = outcome.status }
      -- A call that started a delegate answers with the receipt sentence
      -- as its first text part; name its job id in the row. The tool-name
      -- gate keeps every other tool's row byte-identical, even when its
      -- content carries a receipt-shaped sentence. Delegate answers are
      -- text-only, so a first part of another shape ends the search.
      local first = outcome.content[1]
      if type(first) == "table" and first.type == "text" and type(first.text) == "string" then
        if name == "delegate_spawn" or name == "delegate_fork" then
          -- The log path keeps spaces from the project key or home
          -- directory, so only the id prefix is matched.
          local id = first.text:match("^Started delegate (%S+)%. Its log is ")
          if id ~= nil then
            row.job = id
          end
        elseif name == "shell" then
          -- Background, monitor and terminal receipts put the id on the
          -- second line; match only its prefix so paths may contain spaces.
          local id = first.text:match("\nJob (%S+)%. Output: ")
          if id ~= nil then
            row.job = id
          end
        end
      end
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
    -- The script's own `utf8`: a copy of the shared table, so adding
    -- `sub` leaks into no other extension's `_G`. `sub` mirrors
    -- `string.sub` on characters, never splitting a multibyte sequence;
    -- invalid input raises instead of slicing garbage.
    local script_utf8 = {}
    for name, value in pairs(utf8) do
      script_utf8[name] = value
    end
    script_utf8.sub = function(s, i, j)
      if type(s) ~= "string" then
        error("bad argument #1 to 'sub' (string expected, got " .. type(s) .. ")", 2)
      end
      if utf8.len(s) == nil then
        error("bad argument #1 to 'sub' (invalid UTF-8)", 2)
      end
      local function checkint(v, n)
        if type(v) ~= "number" then
          error("bad argument #" .. n .. " to 'sub' (number expected, got " .. type(v) .. ")", 2)
        end
        local k = math.tointeger(v)
        if k == nil then
          error("bad argument #" .. n .. " to 'sub' (number has no integer representation)", 2)
        end
        return k
      end
      if i == nil then
        i = 1
      else
        i = checkint(i, 2)
      end
      if j == nil then
        j = -1
      else
        j = checkint(j, 3)
      end
      -- The `string.sub` corrections, on characters: negatives count
      -- back from the end, out-of-range ends clamp, an empty range is "".
      local total = utf8.len(s)
      if i < 0 then
        i = total + i + 1
      end
      if j < 0 then
        j = total + j + 1
      end
      if i < 1 then
        i = 1
      end
      if j > total then
        j = total
      end
      if i > j then
        return ""
      end
      local first = utf8.offset(s, i)
      local last
      if j == total then
        last = #s
      else
        last = utf8.offset(s, j + 1) - 1
      end
      return s:sub(first, last)
    end
    env.utf8 = script_utf8
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
      local line = "- " .. row.name .. " " .. row.status
      if row.job ~= nil then
        line = line .. " job " .. row.job
      end
      if row.artifact ~= nil then
        line = line .. " " .. row.artifact
      end
      out[#out + 1] = line
    end
    return table.concat(out, "\n")
  end,
})
