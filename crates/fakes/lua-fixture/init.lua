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
