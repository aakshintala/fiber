-- OpenRouter's generation lookup (docs/model-routing.md, "Cost"): the cost of
-- a call that ended without one, from GET <base_url>/generation?id=<id>.
-- The lookup answers 404 until the generation's cost is known, which is
-- nothing, not an error.

-- Every byte outside A-Z, a-z, 0-9 and -_.~ becomes %XX.
local function encode(text)
  return (text:gsub("[^A-Za-z0-9%-_%.~]", function(c)
    return string.format("%%%02X", string.byte(c))
  end))
end

fiber.provider("openrouter", {
  cost = {
    timeout = 10000,
    run = function(call)
      local headers = {}
      if call.key ~= nil then
        headers.authorization = "Bearer " .. call.key
      end
      local reply = host.http({
        url = call.base_url .. "/generation?id=" .. encode(call.generation_id),
        headers = headers,
      })
      if reply.status ~= 200 then
        return nil
      end
      local data = json.decode(reply.body).data
      if type(data) ~= "table" then
        return nil
      end
      if type(data.total_cost) == "number" then
        return data.total_cost
      end
      return nil
    end,
  },
})
