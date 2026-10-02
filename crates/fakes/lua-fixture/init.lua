-- The fixture Lua extension: one command per behaviour of the runtime that a
-- test exercises (docs/extensions.md, "Lua extensions" and "When an extension
-- misbehaves").

local helper = require("lib.helper")

fiber.command("echo", { timeout = 1000, run = function(text) return text end })

fiber.command("globals", {
  timeout = 1000,
  run = function()
    local names = {}
    for name in pairs(_G) do names[#names + 1] = name end
    table.sort(names)
    return table.concat(names, ",")
  end,
})

-- require loads a module once, from the extension's directory.
fiber.command("greet", {
  timeout = 1000,
  run = function(text)
    local again = require("lib.helper")
    return again.greet(text) .. (again == helper and " (cached)" or " (loaded again)")
  end,
})

fiber.command("fail", { timeout = 1000, run = function() error("boom") end })

fiber.command("spin", { timeout = 50, run = function() while true do end end })

-- pcall cannot swallow the deadline.
fiber.command("spin_pcall", {
  timeout = 50,
  run = function()
    while true do pcall(function() while true do end end) end
  end,
})

-- A loop inside a coroutine from coroutine.create.
fiber.command("spin_create", {
  timeout = 50,
  run = function()
    local co = coroutine.create(function() while true do end end)
    return tostring(coroutine.resume(co))
  end,
})

-- A loop under pcall inside a coroutine from coroutine.wrap.
fiber.command("spin_wrap", {
  timeout = 50,
  run = function()
    coroutine.wrap(function()
      while true do pcall(function() while true do end end) end
    end)()
  end,
})

-- A coroutine.create coroutine inside a coroutine.wrap one.
fiber.command("spin_nested", {
  timeout = 50,
  run = function()
    return coroutine.wrap(function()
      local inner = coroutine.create(function() while true do end end)
      return tostring(coroutine.resume(inner))
    end)()
  end,
})

-- Lua runs a __gc finalizer with hooks off, so only the caller's deadline
-- stops this loop.
fiber.command("spin_gc", {
  timeout = 50,
  run = function()
    setmetatable({}, { __gc = function() while true do end end })
    collectgarbage()
  end,
})

-- A backtracking match is one long C call, with no instruction to hook.
fiber.command("spin_find", {
  timeout = 50,
  run = function()
    return tostring(string.find(string.rep("a", 100000), "a*a*a*a*b"))
  end,
})

-- A generator: coroutine.wrap still yields values as Lua's does.
fiber.command("count", {
  timeout = 1000,
  run = function()
    local next_n = coroutine.wrap(function() for i = 1, 3 do coroutine.yield(i) end end)
    return next_n() + next_n() + next_n()
  end,
})

-- The timeout stops it only if the memory cap does not.
fiber.command("grow", {
  timeout = 5000,
  run = function()
    local t = {}
    while true do t[#t + 1] = string.rep("x", 1024) end
  end,
})

-- The fixture provider (docs/model-routing.md, "Model discovery", "Signing a
-- request" and "Credentials"). Its server's address is per test, so the test
-- stores it as the secret `fixture.url`.
local function hex(bytes)
  return (bytes:gsub(".", function(c) return string.format("%02x", c:byte()) end))
end

fiber.provider("fixture", {
  models = {
    timeout = 5000,
    run = function()
      local base = host.secret("fixture.url")
      local reply = host.http({
        url = base .. "/v1/models",
        headers = { authorization = "Bearer " .. host.secret("fixture.api_key") },
      })
      local list = {}
      for _, m in ipairs(json.decode(reply.body).data) do
        list[#list + 1] = {
          id = m.id,
          protocol = "openai-responses",
          base_url = base .. "/v1",
          context_window = m.context_length,
        }
      end
      return list
    end,
  },
  credential = {
    timeout = 5000,
    run = function()
      local reply = host.http({
        method = "POST",
        url = host.secret("fixture.url") .. "/token",
        headers = { ["content-type"] = "application/json" },
        body = json.encode({ key = host.secret("fixture.api_key") }),
      })
      local t = json.decode(reply.body)
      return { token = t.access_token, expires_at = t.expires_at }
    end,
  },
  -- Signs the method, URL and body hash, and reports which fields it saw.
  sign = {
    timeout = 1000,
    run = function(request)
      local seen = {}
      for key in pairs(request) do seen[#seen + 1] = key end
      table.sort(seen)
      local text = request.method .. "\n" .. request.url .. "\n" .. request.body_sha256
      return {
        ["x-fixture-signature"] = hex(host.hmac_sha256("fixture-secret", text)),
        ["x-fixture-content-sha256"] = request.body_sha256,
        ["x-fixture-saw"] = table.concat(seen, ","),
      }
    end,
  },
})
