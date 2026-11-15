fiber.provider("anthropic", {
  models = {
    timeout = 10000,
    run = function(call)
      local function encode(value)
        return (value:gsub("[^A-Za-z0-9%-_%.~]", function(c)
          return string.format("%%%02X", string.byte(c))
        end))
      end

      local models = {}
      local after_id = nil
      while true do
        local url = "https://api.anthropic.com/v1/models?limit=1000"
        if after_id ~= nil then
          url = url .. "&after_id=" .. encode(after_id)
        end
        local reply = host.http({
          method = "GET",
          url = url,
          headers = {
            ["x-api-key"] = call.key,
            ["anthropic-version"] = "2023-06-01",
          },
        })
        if reply.status ~= 200 then
          error("anthropic models: unexpected status " .. tostring(reply.status), 0)
        end
        local ok, page = pcall(json.decode, reply.body)
        if not ok then
          error("anthropic models: the reply body is not JSON", 0)
        end
        if type(page) ~= "table" or type(page.data) ~= "table" then
          error("anthropic models: the reply holds no model list", 0)
        end
        for _, item in ipairs(page.data) do
          if type(item) == "table" and type(item.id) == "string" and item.lifecycle ~= "retired" then
            models[#models + 1] = {
              id = item.id,
              protocol = "anthropic-messages",
              base_url = "https://api.anthropic.com/v1",
            }
          end
        end
        if page.has_more ~= true then
          return models
        end
        if type(page.last_id) ~= "string" then
          error("anthropic models: the reply has more pages but no last id", 0)
        end
        after_id = page.last_id
      end
    end,
  },
})
