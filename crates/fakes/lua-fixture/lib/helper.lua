-- A module the fixture loads with require, from inside its own directory.
local M = {}

function M.greet(name)
  return "hello, " .. name
end

return M
